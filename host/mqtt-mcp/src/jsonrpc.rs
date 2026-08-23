//! Minimal JSON-RPC 2.0 dispatch for MCP stdio transport.
//!
//! Mirrors `email-mcp`'s hand-rolled dispatcher: the on-the-wire
//! shape is small enough that pulling in a full MCP SDK would
//! inflate the binary. Three method kinds:
//!
//! * `initialize` — handshake; we return server info.
//! * `tools/list` — client asks "what tools do you expose?"
//! * `tools/call` — client invokes one of our tools.
//!
//! Notifications (no `id`) are accepted but produce no reply —
//! we return `Ok(None)` so the caller knows not to write a
//! response.

use serde_json::{json, Value};

use crate::tools::ToolRegistry;

/// A parsed JSON-RPC 2.0 request. We keep this loose — only the
/// fields we actually use are typed.
#[derive(Debug)]
pub struct Request {
    /// Request id. `None` means this is a notification (no reply).
    pub id: Option<Value>,
    /// Method name (e.g. `"tools/call"`).
    pub method: String,
    /// Method parameters (arbitrary JSON object).
    pub params: Value,
}

impl Request {
    fn parse(raw: &str) -> Result<Self, anyhow::Error> {
        let v: Value = serde_json::from_str(raw)?;
        Ok(Self {
            id: v.get("id").cloned(),
            method: v
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing method"))?
                .to_string(),
            params: v.get("params").cloned().unwrap_or(Value::Null),
        })
    }
}

/// Dispatch a single JSON-RPC request line. Returns:
///
/// * `Ok(Some(reply))` — write this JSON line on stdout.
/// * `Ok(None)` — notification; no reply.
/// * `Err(_)` — parse or dispatch failure; the caller will
///   serialise it as a JSON-RPC error response.
pub async fn dispatch(
    raw: &str,
    registry: &ToolRegistry,
) -> Result<Option<Value>, anyhow::Error> {
    let req = Request::parse(raw)?;
    let id = req.id.clone();

    let result = match req.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": "magent-mqtt-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": { "listChanged": false }
            }
        })),
        "notifications/initialized" => {
            // Handshake notification — no reply.
            return Ok(None);
        }
        "tools/list" => {
            let tools = registry.list();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tools/call: missing `name`"))?;
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Null);
            // Per the MCP spec, `tools/call` ALWAYS returns a 200-
            // shaped envelope — failures are flagged with
            // `"isError": true` instead of a JSON-RPC error. This
            // matches `email-mcp` and lets the model distinguish
            // "tool rejected your args" from "the server itself
            // crashed".
            let (text, is_error) = match registry.call(name, args).await {
                Ok(body) => (serde_json::to_string(&body)?, false),
                Err(e) => (e.to_string(), true),
            };
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": text,
                }],
                "isError": is_error,
            }))
        }
        other => Err(anyhow::anyhow!("unknown method: {}", other)),
    };

    // Wrap the result (or error) in a JSON-RPC envelope. We
    // always include `jsonrpc: "2.0"` even for failures.
    let mut response = json!({ "jsonrpc": "2.0" });
    match (id, result) {
        (Some(id), Ok(value)) => {
            response["id"] = id;
            response["result"] = value;
        }
        (Some(id), Err(e)) => {
            response["id"] = id;
            response["error"] = json!({
                "code": -32_000,
                "message": e.to_string(),
            });
        }
        (None, Err(e)) => {
            // Notification with an error: nothing to send back.
            // We log the error so operators can debug.
            log::warn!("mqtt-mcp: notification error: {}", e);
            return Ok(None);
        }
        (None, Ok(_)) => {
            // Notification that succeeded: still no reply.
            return Ok(None);
        }
    }
    Ok(Some(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        // Use an empty registry; these tests only exercise the
        // dispatcher surface.
        ToolRegistry::empty()
    }

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            &registry(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["result"]["serverInfo"]["name"], "magent-mqtt-mcp");
    }

    #[tokio::test]
    async fn notification_produces_no_reply() {
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &registry(),
        )
        .await
        .unwrap();
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_error_envelope() {
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","id":7,"method":"does/not/exist"}"#,
            &registry(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp["error"]["code"], -32_000);
    }

    #[tokio::test]
    async fn missing_id_in_response_for_error() {
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","method":"tools/list"}"#,
            &registry(),
        )
        .await
        .unwrap();
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn tools_call_failure_marks_iserror_true() {
        // MCP spec: `tools/call` must ALWAYS return a 200-shaped
        // envelope. Failures are signalled by `"isError": true`,
        // NOT by a JSON-RPC error code. Verify we honour that —
        // the empty registry will produce an "unknown tool" error
        // and we want it surfaced as `isError`, not as a
        // protocol-level error.
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"ghost","arguments":{}}}"#,
            &registry(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp["id"], 99);
        assert_eq!(resp["result"]["isError"], true);
        // No top-level JSON-RPC `error` field — failure is
        // entirely contained in the result body.
        assert!(resp.get("error").is_none());
    }

    #[tokio::test]
    async fn tools_call_missing_name_returns_protocol_error() {
        // Protocol-level errors (malformed request) must remain
        // JSON-RPC errors, NOT `isError`. The MCP spec only
        // reserves `isError` for tool-level failures (the request
        // was well-formed but the tool rejected it). A missing
        // `name` field is the former.
        let err = dispatch(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"arguments":{}}}"#,
            &registry(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("missing"));
        assert!(err.to_string().contains("name"));
    }
}
