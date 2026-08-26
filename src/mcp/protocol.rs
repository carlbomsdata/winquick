//! The JSON-RPC 2.0 subset the Model Context Protocol actually uses.
//!
//! This is hand-written rather than taken from an SDK. The reasoning is in
//! docs/mcp.md, but briefly: WinQuick is a synchronous, six-dependency binary,
//! and the official Rust SDK brings an async runtime with it. The part of the
//! protocol a stdio tool server needs — request, response, error, notification
//! — is small enough that owning it costs less than the dependency would, and
//! keeps `winquick` a single self-contained executable.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The protocol revision this server implements.
///
/// A client that asks for a different revision is answered with this one, which
/// is what the specification requires of a server that cannot speak the
/// requested version: state your own and let the client decide.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Revisions we recognise and will echo back verbatim when asked for them.
pub const SUPPORTED_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

// JSON-RPC 2.0 reserved codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// One incoming message.
///
/// `id` is absent on notifications, and that absence is the whole difference:
/// a notification must never be answered, even when it fails.
#[derive(Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// The spec requires `"jsonrpc": "2.0"`. Absent is tolerated, because some
    /// clients omit it and refusing would help nobody; a *different* value is a
    /// real disagreement about the protocol and is refused.
    pub fn version_ok(&self) -> bool {
        self.jsonrpc.is_empty() || self.jsonrpc == "2.0"
    }
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// One outgoing message. Exactly one of `result` or `error` is present.
#[derive(Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response { jsonrpc: "2.0", id, result: Some(result), error: None }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorBody { code, message: message.into(), data: None }),
        }
    }
}

/// A tool result that succeeded. `structured` is sent as `structuredContent`
/// for clients that read it, and always mirrored as deterministic JSON text so
/// that a client which only reads `content` loses nothing.
pub fn tool_result(structured: Value) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false
    })
}

/// A tool result that failed *inside the tool*.
///
/// This is not a JSON-RPC error. The call reached the tool and the tool has an
/// answer: the environment was not ready, a selector matched nothing, a window
/// never appeared. Reporting it as `isError` lets the agent read the reason and
/// act, rather than seeing a transport failure and assuming the server broke.
pub fn tool_error(message: impl Into<String>) -> Value {
    let message = message.into();
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": { "ok": false, "error": message },
        "isError": true
    })
}

/// A tool result carrying an image, plus its own structured description.
pub fn tool_image(mime: &str, base64_data: String, structured: Value) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [
            { "type": "image", "data": base64_data, "mimeType": mime },
            { "type": "text", "text": text }
        ],
        "structuredContent": structured,
        "isError": false
    })
}

/// Base64, without pulling in a crate for eighteen lines of table lookup.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_an_id_is_a_notification() {
        let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(r.is_notification());
        let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(!r.is_notification());
    }

    /// An id of `null` is still an id as far as the wire is concerned, and the
    /// response has to carry it back unchanged.
    #[test]
    fn responses_echo_the_id_they_were_given() {
        for id in [json!(1), json!("abc"), json!(null)] {
            let r = Response::ok(id.clone(), json!({}));
            let v = serde_json::to_value(&r).unwrap();
            assert_eq!(v["id"], id);
            assert_eq!(v["jsonrpc"], "2.0");
        }
    }

    #[test]
    fn a_response_carries_result_or_error_but_never_both() {
        let okv = serde_json::to_value(Response::ok(json!(1), json!({"a":1}))).unwrap();
        assert!(okv.get("result").is_some() && okv.get("error").is_none());
        let ev = serde_json::to_value(Response::err(json!(1), METHOD_NOT_FOUND, "nope")).unwrap();
        assert!(ev.get("error").is_some() && ev.get("result").is_none());
    }

    /// The structured payload must also be readable by a client that only looks
    /// at `content`, so the two always agree.
    #[test]
    fn tool_results_mirror_structured_content_as_text() {
        let v = tool_result(json!({"exitCode": 0, "stdout": "hi"}));
        assert_eq!(v["isError"], false);
        assert_eq!(v["structuredContent"]["exitCode"], 0);
        let text = v["content"][0]["text"].as_str().unwrap();
        let reparsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(reparsed["exitCode"], 0);
    }

    /// A tool-level failure is a *result*, not a JSON-RPC error, and says why.
    #[test]
    fn tool_errors_are_results_with_a_reason() {
        let v = tool_error("no desktop session is running");
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"].as_str().unwrap().contains("no desktop session"));
        assert_eq!(v["structuredContent"]["ok"], false);
    }

    #[test]
    fn images_carry_both_the_png_and_a_description() {
        let v = tool_image("image/png", "AAAA".into(), json!({"width": 10}));
        assert_eq!(v["content"][0]["type"], "image");
        assert_eq!(v["content"][0]["mimeType"], "image/png");
        assert_eq!(v["content"][1]["type"], "text");
        assert_eq!(v["structuredContent"]["width"], 10);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // A PNG signature round-trips, since that is what this is actually for.
        assert_eq!(base64(&[0x89, 0x50, 0x4E, 0x47]), "iVBORw==");
    }
}
