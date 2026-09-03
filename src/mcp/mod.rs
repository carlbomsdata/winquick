//! WinQuick as an MCP server.
//!
//! `winquick mcp` is a normal mode of the same binary, not a separate program:
//! it speaks JSON-RPC 2.0 over stdin/stdout and calls the very same internal
//! functions the CLI calls. There is no subprocess, no output parsing, and no
//! second implementation of anything.
//!
//! ```text
//!   CLI ─┐
//!        ├──> runner / desktop / facts  ──> QEMU + Windows
//!   MCP ─┘
//! ```
//!
//! ## Why stdout is taken away from the rest of the program
//!
//! One stray `println!` anywhere in WinQuick — or in code added later — would
//! interleave with the protocol and break the connection, and the failure would
//! look like a client bug. Rather than trusting every call path to stay quiet,
//! the server takes the real stdout for itself and points file descriptor 1 at
//! stderr before doing anything else. Existing code that prints keeps working;
//! its output simply goes where diagnostics belong. The protocol writer holds
//! the only handle to the original descriptor.

pub mod protocol;
pub mod tools;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

use protocol::{
    Request, Response, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND,
    PARSE_ERROR, PROTOCOL_VERSION, SUPPORTED_VERSIONS,
};

/// Take exclusive ownership of stdout, and redirect everyone else's to stderr.
///
/// Returns the private handle the protocol is written through. After this, a
/// `println!` from any module lands on stderr, which is harmless.
///
/// Both platforms do the same thing — duplicate descriptor 1, then point 1 at
/// 2 — because both C runtimes provide it. Only the symbol names differ, and
/// the Windows CRT prefixes them with an underscore.
#[cfg(unix)]
fn capture_stdout() -> Result<std::fs::File> {
    use std::os::unix::io::FromRawFd;
    extern "C" {
        fn dup(fd: i32) -> i32;
        fn dup2(src: i32, dst: i32) -> i32;
    }
    // SAFETY: fd 1 and 2 are open in any process started by an MCP client, and
    // the duplicate is handed straight to a File that owns it from here on.
    unsafe {
        let saved = dup(1);
        if saved < 0 {
            anyhow::bail!("could not duplicate stdout for the MCP protocol");
        }
        if dup2(2, 1) < 0 {
            anyhow::bail!("could not redirect stdout to stderr for the MCP protocol");
        }
        Ok(std::fs::File::from_raw_fd(saved))
    }
}

#[cfg(windows)]
fn capture_stdout() -> Result<std::fs::File> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    extern "C" {
        fn _dup(fd: i32) -> i32;
        fn _dup2(src: i32, dst: i32) -> i32;
        fn _get_osfhandle(fd: i32) -> isize;
    }
    // SAFETY: the CRT descriptors 1 and 2 exist in any console or redirected
    // process, and the duplicated descriptor's OS handle is handed straight to
    // a File that owns it from here on.
    unsafe {
        let saved = _dup(1);
        if saved < 0 {
            anyhow::bail!("could not duplicate stdout for the MCP protocol");
        }
        if _dup2(2, 1) < 0 {
            anyhow::bail!("could not redirect stdout to stderr for the MCP protocol");
        }
        let handle = _get_osfhandle(saved);
        if handle == -1 {
            anyhow::bail!("could not resolve the duplicated stdout handle");
        }
        Ok(std::fs::File::from_raw_handle(handle as RawHandle))
    }
}

/// Run the server until stdin reaches end of file.
pub fn serve() -> Result<i32> {
    let mut out = capture_stdout().context("preparing the MCP transport")?;
    let stdin = std::io::stdin();
    let mut server = Server::new();

    for line in stdin.lock().lines() {
        // A Ctrl-C during a tool call aborts the run itself; the server must
        // then stop too rather than sit waiting for a client that is going away.
        if crate::interrupt::interrupted() {
            break;
        }
        let line = match line {
            Ok(l) => l,
            // The client went away mid-stream; that is a normal shutdown.
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle(&line) {
            // A closed pipe means the client has already gone. Stop, rather
            // than failing loudly about output nobody is reading.
            if writeln!(out, "{response}").is_err() || out.flush().is_err() {
                break;
            }
        }
        if server.shutdown || crate::interrupt::interrupted() {
            break;
        }
    }

    // The client is gone. Anything WinQuick started on its behalf goes with it,
    // so no Windows VM outlives the conversation that asked for it.
    server.cleanup();
    if crate::interrupt::interrupted() {
        // The conventional shell code for "terminated by Ctrl-C".
        return Ok(130);
    }
    Ok(0)
}

pub struct Server {
    initialized: bool,
    shutdown: bool,
    /// Whether this server was the one that started the desktop session. Only
    /// then does it clean it up on the way out.
    started_desktop: bool,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server { initialized: false, shutdown: false, started_desktop: false }
    }

    /// Handle one line of input. `None` means "say nothing", which is the
    /// correct answer to a notification and to an unparseable notification.
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // Malformed JSON has no id to answer with; the spec says use null.
                return Some(render(Response::err(
                    Value::Null,
                    PARSE_ERROR,
                    format!("invalid JSON: {e}"),
                )));
            }
        };

        // A batch is a JSON array. Answer each member and return only the
        // responses that are owed.
        if let Value::Array(items) = value {
            let mut answers = Vec::new();
            for item in items {
                if let Some(a) = self.dispatch_value(item) {
                    answers.push(a);
                }
            }
            if answers.is_empty() {
                return None;
            }
            return Some(format!("[{}]", answers.join(",")));
        }

        self.dispatch_value(value)
    }

    fn dispatch_value(&mut self, value: Value) -> Option<String> {
        let req: Request = match serde_json::from_value(value.clone()) {
            Ok(r) => r,
            Err(e) => {
                // Without a method we cannot tell a request from a notification;
                // if it carries an id, it deserves an answer.
                let id = value.get("id").cloned().unwrap_or(Value::Null);
                value.get("id")?;
                return Some(render(Response::err(
                    id,
                    INVALID_REQUEST,
                    format!("not a valid JSON-RPC request: {e}"),
                )));
            }
        };

        if req.is_notification() {
            self.notify(&req);
            return None;
        }
        let id = req.id.clone().unwrap_or(Value::Null);
        if !req.version_ok() {
            return Some(render(Response::err(
                id,
                INVALID_REQUEST,
                format!("unsupported jsonrpc version {:?}; this server speaks 2.0", req.jsonrpc),
            )));
        }
        Some(render(self.request(&req, id)))
    }

    fn notify(&mut self, req: &Request) {
        match req.method.as_str() {
            "notifications/initialized" => self.initialized = true,
            // Cancellation arrives as a notification. WinQuick's operations are
            // synchronous, so there is nothing in flight to cancel on this
            // thread; acknowledging silently is correct and required — a
            // notification must never be answered.
            "notifications/cancelled" => {}
            _ => {}
        }
    }

    fn request(&mut self, req: &Request, id: Value) -> Response {
        match req.method.as_str() {
            "initialize" => {
                let asked = req
                    .params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(PROTOCOL_VERSION);
                // Echo a version we both know; otherwise state ours and let the
                // client decide whether it can proceed.
                let version =
                    if SUPPORTED_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSION };
                Response::ok(
                    id,
                    json!({
                        "protocolVersion": version,
                        // Only what is actually implemented: no prompts, no
                        // resources, no sampling.
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": {
                            "name": "winquick",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "instructions":
                            "WinQuick runs commands and GUI applications inside a real, \
                             disposable Windows environment on this Mac. Use windows_run for \
                             builds, tests and any Windows command. Use the desktop_* and ui_* \
                             tools only when graphical behaviour has to be verified: start a \
                             session, launch the application, wait for its window, then read it \
                             with ui_tree and drive it by automationId. ui_screenshot returns a \
                             real PNG of the Windows desktop. The guest has no network."
                    }),
                )
            }
            "ping" => Response::ok(id, json!({})),
            "tools/list" => Response::ok(id, tools::list()),
            "tools/call" => self.call_tool(req, id),
            // Advertised as unsupported, so answer honestly rather than
            // pretending to have an empty collection.
            _ => Response::err(id, METHOD_NOT_FOUND, format!("unknown method `{}`", req.method)),
        }
    }

    fn call_tool(&mut self, req: &Request, id: Value) -> Response {
        let Some(name) = req.params.get("name").and_then(Value::as_str) else {
            return Response::err(id, INVALID_PARAMS, "tools/call needs a `name`");
        };
        let args = req.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return Response::err(id, INVALID_PARAMS, "`arguments` must be an object");
        }

        match tools::call(name, &args) {
            Ok(result) => {
                // Remember that we own the session, so shutdown can clean up.
                if name == "desktop_start" && result["isError"] == json!(false) {
                    self.started_desktop = true;
                }
                if name == "desktop_stop" {
                    self.started_desktop = false;
                }
                Response::ok(id, result)
            }
            Err(tools::CallError::UnknownTool(n)) => {
                Response::err(id, METHOD_NOT_FOUND, format!("unknown tool `{n}`"))
            }
        }
    }

    /// Leave nothing running that this server started.
    pub fn cleanup(&mut self) {
        if self.started_desktop && crate::desktop::running().is_some() {
            // Best effort: the client has already gone, so there is nobody left
            // to report a failure to, but a stranded VM would be worse.
            let _ = crate::desktop::stop();
        }
    }
}

fn render(r: Response) -> String {
    serde_json::to_string(&r).unwrap_or_else(|_| {
        // Serialising a response should not be able to fail, but emitting
        // nothing would hang the client, so fall back to a valid error.
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{INTERNAL_ERROR},"message":"response could not be serialised"}}}}"#
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(s: &mut Server, msg: &str) -> Value {
        let out = s.handle(msg).expect("this message deserves an answer");
        serde_json::from_str(&out).expect("every answer is valid JSON")
    }

    #[test]
    fn initialize_advertises_only_tools() {
        let mut s = Server::new();
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(v["result"]["serverInfo"]["name"], "winquick");
        assert_eq!(v["result"]["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        let caps = &v["result"]["capabilities"];
        assert!(caps.get("tools").is_some());
        for unimplemented in ["prompts", "resources", "sampling", "elicitation"] {
            assert!(caps.get(unimplemented).is_none(), "{unimplemented} must not be advertised");
        }
    }

    /// A client on a newer revision gets its own version back; an unknown one
    /// gets ours, which is what lets it decide rather than guess.
    #[test]
    fn protocol_version_is_negotiated() {
        let mut s = Server::new();
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    /// The whole point of a notification: no reply, ever.
    #[test]
    fn notifications_are_never_answered() {
        let mut s = Server::new();
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(s.initialized);
        assert!(s
            .handle(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#
            )
            .is_none());
        // Even an unknown notification stays silent.
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/something_new"}"#).is_none());
    }

    #[test]
    fn ping_answers_empty() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#);
        assert_eq!(v["id"], "p");
        assert_eq!(v["result"], json!({}));
    }

    #[test]
    fn tools_list_returns_every_tool_with_a_schema() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let list = v["result"]["tools"].as_array().unwrap();
        assert_eq!(list.len(), tools::TOOLS.len());
        for t in list {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn unknown_method_and_unknown_tool_are_reported_distinctly() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":3,"method":"no/such/method"}"#);
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        );
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        assert!(v["error"]["message"].as_str().unwrap().contains("nope"));
    }

    #[test]
    fn malformed_json_is_answered_not_fatal() {
        let mut s = Server::new();
        let v = ask(&mut s, "{not json at all");
        assert_eq!(v["error"]["code"], PARSE_ERROR);
        assert_eq!(v["id"], Value::Null);
        // The server is still usable afterwards.
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#);
        assert_eq!(v["result"], json!({}));
    }

    #[test]
    fn a_request_without_a_method_is_an_invalid_request() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":5}"#);
        assert_eq!(v["error"]["code"], INVALID_REQUEST);
        // The same thing without an id is a notification: stay silent.
        assert!(s.handle(r#"{"jsonrpc":"2.0"}"#).is_none());
    }

    #[test]
    fn tools_call_validates_its_own_parameters() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}"#);
        assert_eq!(v["error"]["code"], INVALID_PARAMS);
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"winquick_info","arguments":[]}}"#,
        );
        assert_eq!(v["error"]["code"], INVALID_PARAMS);
    }

    /// A tool that fails is still a successful JSON-RPC call: the agent needs to
    /// read the reason, not see a transport error.
    #[test]
    fn tool_failures_are_results_not_rpc_errors() {
        let mut s = Server::new();
        let v = ask(
            &mut s,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"windows_run","arguments":{}}}"#,
        );
        assert!(v.get("error").is_none(), "a tool-level failure must not be an RPC error");
        assert_eq!(v["result"]["isError"], true);
    }

    #[test]
    fn a_batch_answers_only_what_is_owed() {
        let mut s = Server::new();
        let out = s
            .handle(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"}]"#)
            .expect("the batch contains one request");
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the request is answered, not the notification");
        assert_eq!(arr[0]["id"], 1);
    }

    /// A batch of nothing but notifications produces no output at all.
    #[test]
    fn a_batch_of_notifications_is_silent() {
        let mut s = Server::new();
        assert!(s.handle(r#"[{"jsonrpc":"2.0","method":"notifications/initialized"}]"#).is_none());
    }

    /// Cleanup only stops what this server started; a session the user started
    /// from the CLI is not ours to kill.
    #[test]
    fn cleanup_only_touches_a_session_we_started() {
        let mut s = Server::new();
        assert!(!s.started_desktop);
        s.cleanup(); // must be a no-op, and must not panic
        assert!(!s.started_desktop);
    }

    /// A client claiming a different JSON-RPC version is a real disagreement,
    /// but a missing field is tolerated because some clients omit it.
    #[test]
    fn the_jsonrpc_version_is_checked_but_not_pedantically() {
        let mut s = Server::new();
        let v = ask(&mut s, r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#);
        assert_eq!(v["error"]["code"], INVALID_REQUEST);
        let v = ask(&mut s, r#"{"id":2,"method":"ping"}"#);
        assert_eq!(v["result"], json!({}));
    }

    #[test]
    fn every_answer_is_a_single_line_of_json() {
        let mut s = Server::new();
        for msg in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
            "garbage",
        ] {
            let out = s.handle(msg).expect("answer");
            assert!(!out.contains('\n'), "a response must occupy exactly one line: {out}");
            let _: Value = serde_json::from_str(&out).expect("valid JSON");
        }
    }
}
