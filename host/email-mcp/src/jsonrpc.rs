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

/// Recover the `id` from a request line that failed to parse.
///
/// A body that is valid JSON but lacks `method` must get an error reply
/// carrying the caller's id; otherwise the outer loop emits
/// `{"id":null,"error":{"code":-32700}}`, blaming valid JSON's *syntax* and
/// leaving the client unable to match the reply.
fn recover_id(raw: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("id").cloned())
}

/// JSON-RPC "invalid params" (-32602) envelope that preserves the request id.
///
/// Well-formed JSON with a bad or missing parameter is *invalid params*,
/// not a server fault: the previous generic arm returned -32000 for a
/// `tools/call` with no `name`, the same defect mqtt-mcp had. -32000 is the
/// "application error" code and tells a client the server broke, when in
/// fact the request did.
fn invalid_params(id: &Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.clone(),
        "error": { "code": -32_602, "message": message },
    })
}

/// Dispatch a single JSON-RPC request line. Returns:
///
/// * `Ok(Some(reply))` — write this JSON line on stdout.
/// * `Ok(None)`        — notification; nothing to write.
/// * `Err(_)`          — fatal dispatch error; caller writes a
///   parse-error response and keeps serving.
pub async fn dispatch(raw: &str, registry: &ToolRegistry) -> Result<Option<Value>, anyhow::Error> {
    let req = match Request::parse(raw) {
        Ok(req) => req,
        Err(e) => {
            // Distinguish the two failure classes, because JSON-RPC gives
            // them different codes:
            //
            //   * the line is not JSON at all → -32700 (Parse error);
            //   * it *is* JSON but not a request object (`"hello"`, `[1,2]`,
            //     a trailing-comma-free batch array) → -32600 (Invalid
            //     Request).
            //
            // Both used to surface as -32700, which told the client "the
            // server cannot parse JSON" when the bytes parsed fine. A batch
            // array is *valid* JSON-RPC 2.0 — this server does not implement
            // batching, so -32600 is the honest answer, not a syntax error.
            return match recover_id(raw) {
                Some(id) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32_600, "message": e.to_string() },
                }))),
                None if serde_json::from_str::<Value>(raw).is_ok() => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32_600,
                        "message": format!(
                            "invalid request: expected a JSON object carrying `method` ({e})"
                        ),
                    },
                }))),
                // Not JSON at all: let the caller emit -32700.
                None => Err(e),
            };
        }
    };

    // Notifications get no reply. We log them at debug level so
    // operators can trace the protocol without spamming info logs.
    // Binding the `Some` case here (rather than checking `is_none` and
    // re-`expect`ing below) keeps the reply path from ever panicking if
    // this guard is moved.
    let Some(id) = req.id.clone() else {
        log::debug!("notification: method={} params={}", req.method, req.params);
        return Ok(None);
    };

    // A `tools/call` with no usable `name` is *invalid params* (-32602), not
    // a server error. It is decided before dispatch so the code is exact; the
    // generic arm below remains the "application error" (-32000) path for a
    // genuine server fault.
    if req.method == "tools/call" && req.params.get("name").and_then(Value::as_str).is_none() {
        return Ok(Some(invalid_params(&id, "tools/call: missing `name`")));
    }
    // A name that is present but not in the catalogue is the *same class* of
    // mistake: the method `tools/call` is valid and well-formed, and the
    // parameter value names nothing this server offers. It used to fall
    // through to -32000 ("application error"), which tells a strict client the
    // server is broken and may abort the session over what is really a typo.
    // `mqtt-mcp` answers the same condition with `isError`, so this also makes
    // the two servers agree about what "unknown tool" means.
    if req.method == "tools/call" {
        match req.params.get("name").and_then(Value::as_str) {
            Some(name) if !registry.contains(name) => {
                return Ok(Some(invalid_params(
                    &id,
                    &format!("tools/call: unknown tool: {name}"),
                )));
            }
            _ => {}
        }
    }

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
        // A name that is simply absent from the catalogue is *invalid params*
        // (-32602), not an application error (-32000): the request was
        // well-formed and only the parameter value is wrong. -32000 tells a
        // strict client the server broke and may abort the session over a
        // typo, and `mqtt-mcp` already answered the same condition this way.
        assert_eq!(
            resp["error"]["code"], -32_602,
            "unknown tool must be invalid-params, not a server fault: {resp}"
        );
    }

    /// Valid JSON that is not a request object is -32600, not -32700.
    ///
    /// `"hello"` / `[1,2]` / `[]` are syntactically valid JSON; reporting
    /// -32700 (Parse error) blamed the syntax and told the client the server
    /// could not read JSON at all. A batch array is valid JSON-RPC 2.0; this
    /// server does not batch, so -32600 (Invalid Request) is the honest
    /// answer.
    #[tokio::test]
    async fn valid_json_that_is_not_a_request_is_invalid_request_not_parse_error() {
        for raw in [r#""hello""#, "[1,2,3]", "[]"] {
            let resp = dispatch(raw, &registry())
                .await
                .expect("handled, not an unparseable line")
                .expect("a reply, even without an id");
            assert_eq!(
                resp["error"]["code"], -32_600,
                "{raw} is valid JSON, so this is an invalid request: {resp}"
            );
            assert_eq!(resp["id"], Value::Null, "{resp}");
        }
        // Genuinely malformed bytes still take the -32700 path (the caller
        // emits it).
        assert!(dispatch("{not json", &registry()).await.is_err());
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

    /// `tools/call` with no `name` is *invalid params* (-32602), not -32000.
    ///
    /// -32000 is the application-error code: it tells the client the server
    /// failed, when in fact the request was malformed. mqtt-mcp already
    /// answered -32602 here.
    #[tokio::test]
    async fn a_tools_call_without_a_name_is_invalid_params() {
        let resp = call(json!({})).await;
        assert_eq!(resp["id"], 1, "the id must survive: {resp}");
        assert_eq!(
            resp["error"]["code"], -32_602,
            "malformed params must not be reported as a server fault: {resp}"
        );
        // A non-object `params` takes the same path.
        let resp = call(json!("not-an-object")).await;
        assert_eq!(resp["error"]["code"], -32_602, "got {resp}");
        assert_eq!(resp["id"], 1);
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

    /// A request that is valid JSON but has no `method` must keep its id in
    /// the error reply. Previously the outer loop emitted `id: null` with
    /// -32700, blaming the syntax of a perfectly valid body.
    #[tokio::test]
    async fn a_request_without_a_method_keeps_its_id() {
        let resp = dispatch(r#"{"jsonrpc":"2.0","id":9,"params":{}}"#, &registry())
            .await
            .expect("a bad-field request is not a transport failure")
            .expect("an id-carrying request gets a reply");
        assert_eq!(resp["id"], 9, "the id must survive: {resp}");
        assert_eq!(resp["error"]["code"], -32_600);

        // Garbage with no recoverable id still surfaces as a transport
        // error for the loop to answer with -32700.
        assert!(dispatch("not json", &registry()).await.is_err());
    }
}
