//! The tools WinQuick offers an agent, and the code behind them.
//!
//! Every handler here calls the same internal functions the CLI calls —
//! `runner::run_capture`, `desktop::start`, `desktop::call`, `facts::info`.
//! Nothing shells out to `winquick`, so there is no terminal output to parse
//! and no second implementation to drift.
//!
//! The surface is deliberately small and semantic. An agent should not have to
//! know that a Windows command line is a string, that quoting differs between
//! `cmd` and a real executable, or that a desktop session is a QEMU process. It
//! passes argv as an array and reads structured fields back.

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::protocol::{base64, tool_error, tool_image, tool_result};
use crate::{argv, artifact, artifact_patterns, desktop, facts, runner};

/// How long a bridge call may take before we stop waiting.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Output beyond this is truncated, with the fact reported. Generous on
/// purpose: a build log is meant to be readable, and WinQuick has been tested
/// with far larger. This exists to protect the transport from a runaway
/// process, not to keep results tidy.
const MAX_STREAM_BYTES: usize = 512 * 1024;

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
}

/// Everything the server advertises. `tools/list` renders this, and
/// `tools/call` dispatches on the same names, so the two cannot disagree.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "windows_run",
        description:
            "Run one command inside a real, disposable Windows environment and return its \
             stdout, stderr and exit code. This is the tool to reach for whenever something \
             must be built, tested or checked on Windows rather than reasoned about: \
             `dotnet build`, `dotnet test`, a PowerShell script, or any Windows executable. \
             Each call starts from a pristine Windows and throws it away afterwards, so runs \
             never contaminate each other. Pass the program and its arguments separately — \
             never a single shell string. The guest has no network. To build a project, set \
             `workspace` to its absolute host path: it appears inside Windows as C:\\workspace \
             and is the working directory. The host directory is copied in and never written \
             back, so use `artifacts` to bring build output home. Because the guest is offline, \
             a NuGet restore that needs to reach the network fails with NU1301: run \
             `winquick cache sync` on the Mac once to make packages available offline, or add a \
             NuGet.config to a project that needs no packages.",
        schema: || json!({
            "type": "object",
            "properties": {
                "program": {
                    "type": "string",
                    "description": "Executable to run, e.g. \"cmd\", \"pwsh\", \"dotnet\"."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments, one array element each. Do not pre-quote them; \
                                    WinQuick quotes correctly for cmd and for native programs."
                },
                "workspace": {
                    "type": "string",
                    "description": "Absolute host directory to expose as C:\\workspace. Copied \
                                    in, never copied back."
                },
                "artifacts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns, relative to the workspace, for files to \
                                    bring back to the host: \"bin/**/*.exe\", \"**/*.dll\", \
                                    \"logs/*.txt\". A single * is one directory level; ** \
                                    recurses. Collected even when the command fails."
                },
                "artifactsDir": {
                    "type": "string",
                    "description": "Absolute host directory for retrieved files. \
                                    Defaults to ./winquick-artifacts."
                },
                "timeoutSeconds": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Give up after this long. Default 300."
                }
            },
            "required": ["program"],
            "additionalProperties": false
        }),
    },
    Tool {
        name: "desktop_start",
        description:
            "Boot a real Windows desktop session and leave it running, so GUI applications can \
             be launched and driven. Use this only when graphical behaviour actually has to be \
             verified — plain builds and tests need windows_run instead, which is far cheaper. \
             The session is disposable and there is one at a time; starting when one is already \
             running returns the existing session rather than a second one. Requires the \
             desktop capability (`winquick capability install desktop`). Point `app` at a \
             published build directory to make it available inside Windows as `app`.",
        schema: || json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "Absolute host path to a published application directory. \
                                    Appears inside Windows as `app`, so an executable is then \
                                    launched as \"app\\\\YourApp.exe\"."
                }
            },
            "additionalProperties": false
        }),
    },
    Tool {
        name: "desktop_stop",
        description:
            "Shut the desktop session down and delete its disposable disk. Safe to call when \
             nothing is running. Call it when GUI verification is finished so no Windows VM is \
             left running.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    },
    Tool {
        name: "desktop_status",
        description:
            "Report whether a desktop session is currently running, and for how long. Use it to \
             decide whether desktop_start is needed.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    },
    Tool {
        name: "desktop_launch",
        description:
            "Start a program inside the running desktop session. Give the executable path as it \
             appears inside Windows — typically \"app\\\\YourApp.exe\" when desktop_start was \
             given an `app` directory — and its arguments separately. Follow this with \
             desktop_wait_window before inspecting the UI: launching returns as soon as the \
             process starts, which is before its window exists.",
        schema: || json!({
            "type": "object",
            "properties": {
                "program": {
                    "type": "string",
                    "description": "Windows path of the executable, e.g. \"app\\\\Demo.exe\"."
                },
                "args": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["program"],
            "additionalProperties": false
        }),
    },
    Tool {
        name: "desktop_wait_window",
        description:
            "Wait until a window whose title contains the given text exists, and return its \
             handle. Use this after desktop_launch, before reading or driving the UI. A timeout \
             is reported as a tool error with the time waited, not as a crash.",
        schema: || json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Text the window title must contain."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "How long to wait. Default 60000."
                }
            },
            "required": ["title"],
            "additionalProperties": false
        }),
    },
    Tool {
        name: "ui_tree",
        description:
            "Return the Microsoft UI Automation tree of a window as compact structured JSON: \
             control type, name, automationId, enabled state and value for each element. This \
             is the tool that tells you what a Windows GUI actually contains and what to \
             address it by. Read the tree first, then use its automationId values with ui_click \
             and ui_type. Scope it with `title` or `hwnd` to one window, and use `depth` to keep \
             it small.",
        schema: || json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Limit to the window whose title contains this." },
                "hwnd": { "type": "integer", "description": "Limit to this window handle." },
                "automationId": { "type": "string", "description": "Start from this element instead of the window root." },
                "depth": { "type": "integer", "minimum": 1, "description": "Maximum levels to descend." }
            },
            "additionalProperties": false
        }),
    },
    Tool {
        name: "ui_get",
        description:
            "Read one UI element and return its name, value, control type and enabled state. \
             Use it to verify what an application did — checking a status label after clicking, \
             for instance. Address the element by automationId when it has one; otherwise \
             combine name with controlType. A selector matching several elements is an error \
             listing the candidates rather than a guess. Note that WPF derives automationId \
             from x:Name automatically, while WinForms only exposes one if Control.Name was set \
             — so for WinForms, name plus controlType is often the reliable selector.",
        schema: || json!({
            "type": "object",
            "properties": {
                "automationId": { "type": "string" },
                "name": { "type": "string", "description": "The element's accessible name." },
                "controlType": { "type": "string", "description": "Button, Edit, Text, CheckBox, ComboBox, List ..." },
                "className": { "type": "string" },
                "title": { "type": "string", "description": "Limit the search to this window." },
                "hwnd": { "type": "integer" }
            },
            "additionalProperties": false
        }),
    },
    Tool {
        name: "ui_click",
        description:
            "Click a UI element, addressed semantically rather than by coordinates. Prefer \
             automationId; fall back to name plus controlType. If the element is disabled the \
             tool says so explicitly instead of silently doing nothing.",
        schema: || json!({
            "type": "object",
            "properties": {
                "automationId": { "type": "string" },
                "name": { "type": "string" },
                "controlType": { "type": "string" },
                "className": { "type": "string" },
                "title": { "type": "string", "description": "Limit the search to this window." },
                "hwnd": { "type": "integer" },
                "right": { "type": "boolean", "description": "Right-click instead of left." }
            },
            "additionalProperties": false
        }),
    },
    Tool {
        name: "ui_type",
        description:
            "Type text into a UI element, addressed the same way as ui_click. Unicode is \
             supported. Use this to fill in text boxes before clicking a button.",
        schema: || json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The text to type." },
                "automationId": { "type": "string" },
                "name": { "type": "string" },
                "controlType": { "type": "string" },
                "className": { "type": "string" },
                "title": { "type": "string" },
                "hwnd": { "type": "integer" }
            },
            "required": ["text"],
            "additionalProperties": false
        }),
    },
    Tool {
        name: "ui_screenshot",
        description:
            "Capture what Windows is actually rendering and return it as a PNG image. This is \
             how you check things UI Automation cannot tell you: layout, overlap, clipping, \
             whether a control is visible at all. The image is captured inside Windows, so it \
             shows the real composited desktop. Pass `title` or `hwnd` to frame a single \
             window, which is also how you disambiguate two windows sharing a title.",
        schema: || json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Capture the window whose title contains this." },
                "hwnd": { "type": "integer", "description": "Capture this exact window." }
            },
            "additionalProperties": false
        }),
    },
    Tool {
        name: "winquick_info",
        description:
            "Report what is installed: WinQuick version, the Windows runtime, optional \
             capabilities such as PowerShell and the .NET SDK, and whether the desktop \
             capability and a session are available. Use it to find out whether a capability \
             you need is present before relying on it.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    },
    Tool {
        name: "winquick_doctor",
        description:
            "Check the installation and return each check with its status, plus a list of \
             problems to fix. Use it when something failed unexpectedly, to tell an incomplete \
             setup, a missing QEMU or hivex, or a missing desktop bridge apart from a genuine \
             failure of the thing you were running.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    },
];

pub fn list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": (t.schema)()
        }))
        .collect();
    json!({ "tools": tools })
}

/// Dispatch one `tools/call`.
///
/// `Ok(value)` is a tool result, successful or not. `Err` is reserved for the
/// caller getting the request itself wrong — an unknown tool, or arguments that
/// do not typecheck — which is a JSON-RPC error rather than a tool answer.
pub fn call(name: &str, args: &Value) -> Result<Value, CallError> {
    match name {
        "windows_run" => Ok(windows_run(args)),
        "desktop_start" => Ok(desktop_start(args)),
        "desktop_stop" => Ok(desktop_stop()),
        "desktop_status" => Ok(desktop_status()),
        "desktop_launch" => Ok(desktop_launch(args)),
        "desktop_wait_window" => Ok(desktop_wait_window(args)),
        "ui_tree" => Ok(ui_tree(args)),
        "ui_get" => Ok(bridge_verb("get", args, &[])),
        "ui_click" => Ok(ui_click(args)),
        "ui_type" => Ok(ui_type(args)),
        "ui_screenshot" => Ok(ui_screenshot(args)),
        "winquick_info" => Ok(winquick_info()),
        "winquick_doctor" => Ok(winquick_doctor()),
        _ => Err(CallError::UnknownTool(name.to_string())),
    }
}

/// The only thing `tools/call` refuses outright.
///
/// Everything else — a missing field, a bad path, a pattern that tries to leave
/// the workspace — is answered as a tool result, because the agent needs to read
/// the reason and try again rather than see a transport failure.
pub enum CallError {
    UnknownTool(String),
}

// ------------------------------------------------------------ windows_run

fn windows_run(args: &Value) -> Value {
    let Some(program) = args.get("program").and_then(Value::as_str) else {
        return tool_error("windows_run needs a `program`, for example \"cmd\" or \"dotnet\".");
    };
    if program.trim().is_empty() {
        return tool_error("`program` cannot be empty.");
    }

    // argv stays an array all the way down, and argv::join applies the same
    // context-aware quoting the CLI uses. This is what keeps `echo say \"hi\"`
    // and a PowerShell -Command argument both correct.
    let mut parts = vec![program.to_string()];
    match args.get("args") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            for v in a {
                match v.as_str() {
                    Some(s) => parts.push(s.to_string()),
                    None => return tool_error("every element of `args` must be a string."),
                }
            }
        }
        Some(_) => return tool_error("`args` must be an array of strings."),
    }
    let command = argv::join(&parts);

    let workspace = match args.get("workspace").and_then(Value::as_str) {
        Some(w) => {
            let p = PathBuf::from(w);
            if !p.is_absolute() {
                return tool_error(format!("`workspace` must be an absolute host path, got {w:?}."));
            }
            if !p.is_dir() {
                return tool_error(format!("`workspace` is not a directory: {w}"));
            }
            Some(p)
        }
        None => None,
    };

    let mut artifacts = Vec::new();
    if let Some(v) = args.get("artifacts") {
        match v {
            Value::Null => {}
            Value::Array(a) => {
                for x in a {
                    match x.as_str() {
                        Some(s) => artifacts.push(s.to_string()),
                        None => return tool_error("every element of `artifacts` must be a string."),
                    }
                }
            }
            _ => return tool_error("`artifacts` must be an array of glob patterns."),
        }
    }
    if !artifacts.is_empty() {
        if workspace.is_none() {
            return tool_error("`artifacts` only makes sense with a `workspace` to collect them from.");
        }
        // Traversal is refused here, before a run is paid for, using exactly
        // the rules the CLI uses.
        if let Err(e) = artifact_patterns::validate(&artifacts) {
            return tool_error(format!("{e:#}"));
        }
    }

    let artifacts_dir = match args.get("artifactsDir").and_then(Value::as_str) {
        Some(d) => PathBuf::from(d),
        None => artifact::default_dest(),
    };
    let timeout = args
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(300);

    let opts = runner::Options {
        memory_mb: runner::DEFAULT_MEMORY_MB,
        cpus: runner::DEFAULT_CPUS,
        timeout: Duration::from_secs(timeout),
        verbose: false,
        force_cold: false,
        workspace,
        artifacts: artifacts.clone(),
        artifacts_dir: artifacts_dir.clone(),
        // Agents re-run the same build repeatedly; refusing because the
        // directory already has last run's output in it would be noise.
        artifact_overwrite: true,
    };

    let t0 = Instant::now();
    let outcome = match runner::run_capture(&command, &opts) {
        Ok(o) => o,
        Err(e) => return tool_error(format!("{e:#}")),
    };
    let duration_ms = t0.elapsed().as_millis() as u64;

    let (stdout, out_trunc, out_total) = clamp(&outcome.stdout);
    let (stderr, err_trunc, err_total) = clamp(&outcome.stderr);

    let mut result = json!({
        "exitCode": outcome.exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "durationMs": duration_ms,
        "command": outcome.command,
    });
    if out_trunc {
        result["stdoutTruncated"] = json!(true);
        result["stdoutTotalBytes"] = json!(out_total);
    }
    if err_trunc {
        result["stderrTruncated"] = json!(true);
        result["stderrTotalBytes"] = json!(err_total);
    }
    if !artifacts.is_empty() {
        result["artifacts"] = json!(collected(&artifacts_dir));
        result["artifactsDir"] = json!(artifacts_dir.display().to_string());
    }
    tool_result(result)
}

/// Keep the head and the tail: a failing build usually says what it was doing
/// at the start and why it stopped at the end, and the middle is repetition.
fn clamp(bytes: &[u8]) -> (String, bool, usize) {
    let total = bytes.len();
    if total <= MAX_STREAM_BYTES {
        return (String::from_utf8_lossy(bytes).into_owned(), false, total);
    }
    let half = MAX_STREAM_BYTES / 2;
    let head = String::from_utf8_lossy(&bytes[..half]).into_owned();
    let tail = String::from_utf8_lossy(&bytes[total - half..]).into_owned();
    let dropped = total - MAX_STREAM_BYTES;
    (
        format!("{head}\n\n... [{dropped} bytes omitted by WinQuick; {total} bytes total] ...\n\n{tail}"),
        true,
        total,
    )
}

fn collected(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    fn walk(base: &std::path::Path, at: &std::path::Path, out: &mut Vec<Value>) {
        let Ok(rd) = std::fs::read_dir(at) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(json!({
                    "path": rel.display().to_string(),
                    "hostPath": p.display().to_string(),
                    "bytes": p.metadata().map(|m| m.len()).unwrap_or(0),
                }));
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    out
}

// ---------------------------------------------------------------- desktop

fn desktop_start(args: &Value) -> Value {
    if let Some(s) = desktop::running() {
        return tool_result(json!({
            "running": true,
            "alreadyRunning": true,
            "pid": s.pid,
            "app": s.app,
            "note": "A desktop session was already running; it was reused rather than replaced."
        }));
    }
    let app = match args.get("app").and_then(Value::as_str) {
        Some(a) => {
            let p = PathBuf::from(a);
            if !p.is_dir() {
                return tool_error(format!("`app` is not a directory: {a}"));
            }
            Some(p)
        }
        None => None,
    };
    let t0 = Instant::now();
    match desktop::start(&desktop::StartOptions {
        app,
        memory_mb: desktop::DEFAULT_MEMORY_MB,
        cpus: desktop::DEFAULT_CPUS,
        verbose: false,
    }) {
        Ok(()) => {
            let s = desktop::running();
            tool_result(json!({
                "running": true,
                "alreadyRunning": false,
                "startupDurationMs": t0.elapsed().as_millis() as u64,
                "pid": s.as_ref().map(|s| s.pid),
                "app": s.and_then(|s| s.app),
            }))
        }
        Err(e) => tool_error(format!("{e:#}")),
    }
}

fn desktop_stop() -> Value {
    match desktop::stop() {
        Ok(true) => tool_result(json!({ "stopped": true, "wasRunning": true })),
        Ok(false) => tool_result(json!({ "stopped": true, "wasRunning": false })),
        Err(e) => tool_error(format!("{e:#}")),
    }
}

fn desktop_status() -> Value {
    match desktop::running() {
        Some(s) => {
            let uptime = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(s.started_unix) * 1000)
                .unwrap_or(0);
            tool_result(json!({
                "running": true,
                "pid": s.pid,
                "app": s.app,
                "uptimeMs": uptime,
            }))
        }
        None => tool_result(json!({ "running": false })),
    }
}

fn desktop_launch(args: &Value) -> Value {
    let Some(program) = args.get("program").and_then(Value::as_str) else {
        return tool_error("desktop_launch needs a `program`, e.g. \"app\\\\Demo.exe\".");
    };
    let mut extra = vec![program.to_string()];
    if let Some(Value::Array(a)) = args.get("args") {
        for v in a {
            match v.as_str() {
                Some(s) => extra.push(s.to_string()),
                None => return tool_error("every element of `args` must be a string."),
            }
        }
    }
    bridge("launch", extra)
}

fn desktop_wait_window(args: &Value) -> Value {
    let Some(title) = args.get("title").and_then(Value::as_str) else {
        return tool_error("desktop_wait_window needs a `title` to wait for.");
    };
    let timeout = args.get("timeoutMs").and_then(Value::as_u64).unwrap_or(60_000);
    let r = bridge(
        "wait-window",
        vec!["--title".into(), title.into(), "--timeout".into(), timeout.to_string()],
    );
    lift_window(r)
}

/// The bridge nests the window it found under `window`. An agent's very next
/// step is almost always "use that hwnd", so the identifying fields are lifted
/// to the top level rather than left one dereference away. The full object
/// stays put for anything that wants the bounds.
fn lift_window(mut r: Value) -> Value {
    let Some(w) = r.get("structuredContent").and_then(|s| s.get("window")).cloned() else {
        return r;
    };
    for key in ["hwnd", "title", "pid"] {
        if let Some(v) = w.get(key) {
            r["structuredContent"][key] = v.clone();
        }
    }
    // The mirrored text must keep agreeing with the structured payload.
    if let Some(sc) = r.get("structuredContent") {
        let text = serde_json::to_string_pretty(sc).unwrap_or_default();
        r["content"] = json!([{ "type": "text", "text": text }]);
    }
    r
}

// --------------------------------------------------------- ui automation

/// Turn the shared selector fields into the bridge's own flags.
fn selector(args: &Value) -> Vec<String> {
    let mut v = Vec::new();
    for (key, flag) in [
        ("automationId", "--automation-id"),
        ("name", "--name"),
        ("controlType", "--control-type"),
        ("className", "--class"),
        ("title", "--title"),
    ] {
        if let Some(s) = args.get(key).and_then(Value::as_str) {
            v.push(flag.to_string());
            v.push(s.to_string());
        }
    }
    if let Some(h) = args.get("hwnd").and_then(Value::as_i64) {
        v.push("--hwnd".into());
        v.push(h.to_string());
    }
    v
}

fn bridge_verb(verb: &str, args: &Value, extra: &[String]) -> Value {
    let mut a = selector(args);
    a.extend_from_slice(extra);
    bridge(verb, a)
}

fn ui_tree(args: &Value) -> Value {
    let mut extra = Vec::new();
    if let Some(d) = args.get("depth").and_then(Value::as_i64) {
        extra.push("--depth".to_string());
        extra.push(d.to_string());
    }
    bridge_verb("tree", args, &extra)
}

fn ui_click(args: &Value) -> Value {
    let extra = if args.get("right").and_then(Value::as_bool).unwrap_or(false) {
        vec!["--right".to_string()]
    } else {
        Vec::new()
    };
    if selector(args).is_empty() {
        return tool_error(
            "ui_click needs a selector: automationId, or name with controlType. \
             Read the window with ui_tree first to see what is available.",
        );
    }
    bridge_verb("click", args, &extra)
}

fn ui_type(args: &Value) -> Value {
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return tool_error("ui_type needs `text`.");
    };
    if selector(args).is_empty() {
        return tool_error(
            "ui_type needs a selector saying which element to type into: automationId, \
             or name with controlType.",
        );
    }
    bridge_verb("type", args, &["--text".to_string(), text.to_string()])
}

fn ui_screenshot(args: &Value) -> Value {
    if desktop::running().is_none() {
        return tool_error(NO_SESSION);
    }
    // The bridge writes a PNG to a host path; MCP then carries the bytes
    // themselves, so the agent never has to open a file to see the screen.
    let dir = match std::env::temp_dir().join("winquick-mcp-shots") {
        d => {
            if let Err(e) = std::fs::create_dir_all(&d) {
                return tool_error(format!("cannot prepare a place for the screenshot: {e}"));
            }
            d
        }
    };
    let file = dir.join(format!("shot-{}.png", std::process::id()));
    let mut extra = Vec::new();
    if let Some(t) = args.get("title").and_then(Value::as_str) {
        extra.push("--title".to_string());
        extra.push(t.to_string());
    }
    if let Some(h) = args.get("hwnd").and_then(Value::as_i64) {
        extra.push("--hwnd".to_string());
        extra.push(h.to_string());
    }
    let meta = match desktop::screenshot(&file, &extra, BRIDGE_TIMEOUT) {
        Ok(v) => v,
        Err(e) => return tool_error(format!("{e:#}")),
    };
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => return tool_error(format!("the screenshot could not be read back: {e}")),
    };
    let _ = std::fs::remove_file(&file);
    if bytes.is_empty() {
        return tool_error("the screenshot came back empty.");
    }
    let described = json!({
        "width": meta.get("width").and_then(Value::as_i64).unwrap_or(0),
        "height": meta.get("height").and_then(Value::as_i64).unwrap_or(0),
        "bytes": bytes.len(),
        "nonBlackFraction": meta.get("nonBlackFraction").and_then(Value::as_f64).unwrap_or(0.0),
    });
    tool_image("image/png", base64(&bytes), described)
}

// ----------------------------------------------------------------- system

fn winquick_info() -> Value {
    match facts::info() {
        Ok(i) => match serde_json::to_value(i) {
            Ok(v) => tool_result(v),
            Err(e) => tool_error(format!("{e}")),
        },
        Err(e) => tool_error(format!("{e:#}")),
    }
}

fn winquick_doctor() -> Value {
    match facts::doctor() {
        Ok(d) => match serde_json::to_value(d) {
            Ok(v) => tool_result(v),
            Err(e) => tool_error(format!("{e}")),
        },
        Err(e) => tool_error(format!("{e:#}")),
    }
}

// ------------------------------------------------------------------ bridge

const NO_SESSION: &str =
    "No desktop session is running. Call desktop_start first — and only if the task really \
     needs a GUI; windows_run is the tool for builds and tests.";

/// Send one verb to the guest bridge and hand back whatever JSON it answered.
fn bridge(verb: &str, args: Vec<String>) -> Value {
    if desktop::running().is_none() {
        return tool_error(NO_SESSION);
    }
    let mut argv = vec![verb.to_string()];
    argv.extend(args);
    match desktop::call(&argv, BRIDGE_TIMEOUT) {
        Ok(r) => {
            if r.exit_code != 0 {
                return tool_error(desktop::describe_failure(&r));
            }
            match r.json {
                Some(v) => tool_result(v),
                None => tool_result(json!({
                    "ok": true,
                    "output": String::from_utf8_lossy(&r.stdout).trim().to_string()
                })),
            }
        }
        Err(e) => tool_error(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every advertised tool must be dispatchable, and every dispatchable name
    /// must be advertised. This is the mismatch that otherwise ships silently.
    #[test]
    fn every_advertised_tool_is_reachable() {
        let listed = list();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), TOOLS.len());
        for n in &names {
            // An unknown name is the only thing `call` refuses outright.
            assert!(
                !matches!(call(n, &json!({})), Err(CallError::UnknownTool(_))),
                "{n} is advertised but not dispatched"
            );
        }
        assert!(matches!(call("nope_not_a_tool", &json!({})), Err(CallError::UnknownTool(_))));
    }

    /// A description that only restates the name teaches an agent nothing.
    #[test]
    fn every_tool_has_a_schema_and_a_real_description() {
        for t in TOOLS {
            let s = (t.schema)();
            assert_eq!(s["type"], "object", "{}: schema is not an object", t.name);
            assert!(s.get("properties").is_some(), "{}: no properties", t.name);
            assert!(
                t.description.len() > 80,
                "{}: description is too thin to guide a tool choice",
                t.name
            );
            if let Some(req) = s.get("required").and_then(Value::as_array) {
                let props = s["properties"].as_object().unwrap();
                for r in req {
                    let key = r.as_str().unwrap();
                    assert!(props.contains_key(key), "{}: required `{key}` is not a property", t.name);
                }
            }
        }
    }

    #[test]
    fn the_tool_set_is_the_documented_one() {
        let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "windows_run",
                "desktop_start",
                "desktop_stop",
                "desktop_status",
                "desktop_launch",
                "desktop_wait_window",
                "ui_tree",
                "ui_get",
                "ui_click",
                "ui_type",
                "ui_screenshot",
                "winquick_info",
                "winquick_doctor",
            ]
        );
    }

    /// Bad arguments are answered, not crashed on, and the answer says what to fix.
    #[test]
    fn windows_run_rejects_bad_arguments_readably() {
        let v = windows_run(&json!({}));
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"].as_str().unwrap().contains("program"));

        let v = windows_run(&json!({ "program": "cmd", "args": "not an array" }));
        assert_eq!(v["isError"], true);

        let v = windows_run(&json!({ "program": "cmd", "args": [1, 2] }));
        assert_eq!(v["isError"], true);

        let v = windows_run(&json!({ "program": "cmd", "workspace": "relative/path" }));
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"].as_str().unwrap().contains("absolute"));
    }

    /// Traversal is refused before a Windows boot is paid for.
    #[test]
    fn artifact_traversal_is_refused_up_front() {
        let dir = std::env::temp_dir();
        for bad in ["../escape.txt", "../../etc/passwd", "bin/../../x"] {
            let v = windows_run(&json!({
                "program": "cmd",
                "workspace": dir.display().to_string(),
                "artifacts": [bad]
            }));
            assert_eq!(v["isError"], true, "{bad} was not refused");
        }
    }

    #[test]
    fn artifacts_without_a_workspace_are_refused() {
        let v = windows_run(&json!({ "program": "cmd", "artifacts": ["*.dll"] }));
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"].as_str().unwrap().contains("workspace"));
    }

    /// The selector mapping is what lets an agent speak in UIA terms.
    #[test]
    fn selectors_become_bridge_flags() {
        let s = selector(&json!({
            "automationId": "SaveButton",
            "name": "Save",
            "controlType": "Button",
            "className": "Button",
            "title": "My App",
            "hwnd": 1234
        }));
        assert_eq!(
            s,
            vec!["--automation-id", "SaveButton", "--name", "Save", "--control-type", "Button",
                 "--class", "Button", "--title", "My App", "--hwnd", "1234"]
        );
        assert!(selector(&json!({})).is_empty());
    }

    /// Clicking needs to know what to click; saying so beats a bridge error.
    #[test]
    fn ui_actions_insist_on_a_selector() {
        let v = ui_click(&json!({}));
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"].as_str().unwrap().contains("selector"));
        let v = ui_type(&json!({ "text": "hi" }));
        assert_eq!(v["isError"], true);
        let v = ui_type(&json!({ "automationId": "Box" }));
        assert_eq!(v["isError"], true, "typing with no text should be refused");
    }

    /// The handle an agent needs next must be at the top level, not nested.
    #[test]
    fn wait_window_lifts_the_handle_to_the_top() {
        let raw = tool_result(json!({
            "ok": true,
            "waitedMs": 12,
            "window": { "hwnd": 65576, "title": "WinQuick Demo", "pid": 1072,
                        "bounds": { "width": 620, "height": 460 } }
        }));
        let r = lift_window(raw);
        assert_eq!(r["structuredContent"]["hwnd"], 65576);
        assert_eq!(r["structuredContent"]["title"], "WinQuick Demo");
        assert_eq!(r["structuredContent"]["pid"], 1072);
        // The full object survives for anything that wants the geometry.
        assert_eq!(r["structuredContent"]["window"]["bounds"]["width"], 620);
        // Text and structured content still say the same thing.
        let text = r["content"][0]["text"].as_str().unwrap();
        let reparsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(reparsed["hwnd"], 65576);
    }

    /// A failed wait has no window to lift, and must pass through untouched.
    #[test]
    fn lifting_a_result_without_a_window_changes_nothing() {
        let e = tool_error("timed out");
        let r = lift_window(e);
        assert_eq!(r["isError"], true);
    }

    #[test]
    fn output_under_the_limit_is_untouched() {
        let (s, trunc, total) = clamp(b"hello");
        assert_eq!(s, "hello");
        assert!(!trunc);
        assert_eq!(total, 5);
    }

    /// Truncation keeps both ends and says how much went missing — never silent.
    #[test]
    fn oversized_output_is_truncated_visibly() {
        let big = vec![b'x'; MAX_STREAM_BYTES + 5000];
        let (s, trunc, total) = clamp(&big);
        assert!(trunc);
        assert_eq!(total, MAX_STREAM_BYTES + 5000);
        assert!(s.contains("bytes omitted by WinQuick"));
        assert!(s.len() < big.len());
    }

    #[test]
    fn a_missing_session_is_a_readable_tool_error() {
        // With no session running these must explain themselves rather than panic.
        if desktop::running().is_none() {
            let v = bridge("windows", vec![]);
            assert_eq!(v["isError"], true);
            assert!(v["content"][0]["text"].as_str().unwrap().contains("desktop_start"));
        }
    }
}
