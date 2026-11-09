//! Minimal JSON-RPC 2.0 dispatch for MCP stdio transport.
//!
//! We don't pull in a full MCP client SDK — the on-the-wire shape
//! is small enough that hand-rolling it keeps the dependency
//! footprint low (which matters for embedded-adjacent tools).
//!
//! Three method kinds we care about:
//!
//! * `initialize`         — handshake; we return server info.
//! * `tools/list`         — client asks "what tools do you expose?"
//! * `tools/call`         — client invokes one of our tools.
//!
//! Notifications (no `id`) are accepted but produce no reply — we
//! return `Ok(None)` so the caller knows not to write a response.
//!
//! See <https://spec.modelcontextprotocol.io/specification/basic/>.

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
/// * `Ok(None)`        — notification; nothing to write.
/// * `Err(_)`          — fatal dispatch error; caller writes a
///   parse-error response and keeps serving.
pub async fn dispatch(raw: &str, registry: &ToolRegistry) -> Result<Option<Value>, anyhow::Error> {
    let req = Request::parse(raw)?;

    // Notifications get no reply. We log them at debug level so
    // operators can trace the protocol without spamming info logs.
    // Binding the `Some` case here (rather than checking `is_none` and
    // re-`expect`ing below) keeps the reply path from ever panicking if
    // this guard is moved.
    let Some(id) = req.id.clone() else {
        log::debug!("notification: method={} params={}", req.method, req.params);
        return Ok(None);
    };

    let result = match req.method.as_str() {
        "initialize" => handle_initialize(req.params),
        "tools/list" => handle_tools_list(registry),
        "tools/call" => handle_tools_call(req.params, registry).await,
        "ping" => Ok(json!({})),
        other => Err(anyhow::anyhow!("unknown method: {other}")),
    };

    Ok(Some(match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        }),
        Err(err) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": format!("{err:#}"),
            }
        }),
    }))
}

/// `initialize` — return server identity + protocol version.
///
/// The MCP spec lets us declare a server name/version and the
/// protocol version we speak. We deliberately *don't* declare any
/// `capabilities` other than `"tools"` because that's all we expose.
fn handle_initialize(_params: Value) -> Result<Value, anyhow::Error> {
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": {
            "name": "magent-email-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": {}
        }
    }))
}

/// `tools/list` — return the static tool catalogue.
fn handle_tools_list(registry: &ToolRegistry) -> Result<Value, anyhow::Error> {
    Ok(json!({ "tools": registry.tool_descriptors() }))
}

/// `tools/call` — invoke a single tool by name.
///
/// `params` shape: `{ "name": "list_inbox", "arguments": { ... } }`.
///
/// Per the MCP specification, a *tool-level* failure (bad arguments,
/// IMAP/SMTP error) is a normal `tools/call` result carrying
/// `"isError": true`, not a JSON-RPC error — the latter is reserved
/// for protocol faults such as an unknown method or tool. The model
/// needs to see the failure text so it can correct its call, and a
/// strict client treats a JSON-RPC error as a broken session.
async fn handle_tools_call(params: Value, registry: &ToolRegistry) -> Result<Value, anyhow::Error> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("tools/call: missing `name`"))?;
    if !registry.contains(name) {
        return Err(anyhow::anyhow!("unknown tool: {name}"));
    }
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let (text, is_error) = match registry.call(name, args).await {
        Ok(content) => (content, false),
        Err(err) => (format!("{err:#}"), true),
    };
    Ok(json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "isError": is_error,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// A registry whose sessions are never opened: every test below
    /// fails (or is rejected) before any network call.
    fn registry() -> ToolRegistry {
        let config = Config {
            imap_host: "imap.invalid".into(),
            imap_port: 993,
            smtp_host: "smtp.invalid".into(),
            smtp_port: 587,
            user: "user@example.com".into(),
            password: "secret".into(),
        };
        ToolRegistry::new(crate::AppState::new(config))
    }

    async fn call(params: Value) -> Value {
        let raw = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": params,
        })
        .to_string();
        dispatch(&raw, &registry()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn unknown_tool_is_a_protocol_error() {
        let resp = call(json!({ "name": "no_such_tool", "arguments": {} })).await;
        assert!(
            resp.get("result").is_none(),
            "unknown tool must not look like a tool result: {resp}"
        );
        let msg = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("unknown tool"), "got: {resp}");
    }

    #[tokio::test]
    async fn a_handler_failure_is_an_mcp_tool_error_not_a_protocol_error() {
        // `search_emails` rejects an empty query before touching IMAP.
        let resp = call(json!({
            "name": "search_emails",
            "arguments": { "query": "" },
        }))
        .await;
        assert!(resp.get("error").is_none(), "got protocol error: {resp}");
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("must not be empty"), "got: {resp}");
    }

    #[tokio::test]
    async fn unknown_method_is_a_protocol_error() {
        let raw = r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#;
        let resp = dispatch(raw, &registry()).await.unwrap().unwrap();
        assert_eq!(resp["id"], 7);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown method"));
    }
}
