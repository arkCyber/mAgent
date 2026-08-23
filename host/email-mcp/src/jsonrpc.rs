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
    let id = req.id.clone();

    // Notifications get no reply. We log them at debug level so
    // operators can trace the protocol without spamming info logs.
    if id.is_none() {
        log::debug!("notification: method={} params={}", req.method, req.params);
        return Ok(None);
    }

    let result = match req.method.as_str() {
        "initialize" => handle_initialize(req.params),
        "tools/list" => handle_tools_list(registry),
        "tools/call" => handle_tools_call(req.params, registry).await,
        "ping" => Ok(json!({})),
        other => Err(anyhow::anyhow!("unknown method: {other}")),
    };

    let id = id.expect("id was Some above");
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
async fn handle_tools_call(params: Value, registry: &ToolRegistry) -> Result<Value, anyhow::Error> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("tools/call: missing `name`"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let content = registry.call(name, args).await?;
    Ok(json!({
        "content": [{
            "type": "text",
            "text": content,
        }],
        "isError": false,
    }))
}
