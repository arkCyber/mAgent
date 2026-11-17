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

/// Recover the `id` from a request line that failed to parse.
///
/// A request with a valid JSON body but no `method` is *well-formed JSON
/// with a missing field*, so its error reply must carry the caller's id —
/// the same rule the `tools/call` path follows. Without this the outer loop
/// emitted `{"id":null,"error":{"code":-32700}}`, which both blamed the
/// syntax of valid JSON and left the client unable to match the reply.
fn recover_id(raw: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("id").cloned())
}

/// JSON-RPC "invalid params" (-32602) envelope that preserves the request id.
///
/// A notification (no id) gets no reply, per the spec.
fn invalid_params(id: Option<Value>, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": -32_602, "message": message },
    })
}

/// Dispatch a single JSON-RPC request line. Returns:
///
/// * `Ok(Some(reply))` — write this JSON line on stdout.
/// * `Ok(None)` — notification; no reply.
/// * `Err(_)` — parse or dispatch failure; the caller will
///   serialise it as a JSON-RPC error response.
pub async fn dispatch(raw: &str, registry: &ToolRegistry) -> Result<Option<Value>, anyhow::Error> {
    let req = match Request::parse(raw) {
        Ok(req) => req,
        // A parse failure on a body that still carries an `id` is a
        // *bad request*, not a transport fault. Answering it here keeps the
        // id on the reply; only an unparseable line reaches the caller's
        // generic id-less parse-error path.
        Err(e) => {
            return match recover_id(raw) {
                Some(id) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32_600, "message": e.to_string() },
                }))),
                None => Err(e),
            }
        }
    };
    let id = req.id.clone();
    // A notification (no `id`) gets no reply — including for an error. The
    // `tools/call` arm below used to answer one unconditionally when `name`
    // was missing, which this function's own "notifications get no reply"
    // contract forbids: a reply to a notification is a protocol violation,
    // and `{"id": null, ...}` is ambiguous to the client. Notifications are
    // logged and dropped.
    if id.is_none() {
        log::debug!("notification: method={} params={}", req.method, req.params);
        return Ok(None);
    }

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
            // A missing `name` is a *well-formed JSON-RPC request with bad
            // parameters*, so it must come back as an error envelope that
            // still carries the caller's `id`. Returning `Err` from here used
            // to skip the id back-fill below, and `main` then emitted
            // `{"id": null, "error": {"code": -32700}}` — a client that sent
            // id 42 could never associate the reply, and a parse-error code
            // blamed the syntax of a perfectly valid request.
            let Some(name) = req.params.get("name").and_then(Value::as_str) else {
                // `id` is known to be `Some` here: notifications returned
                // above.
                return Ok(Some(invalid_params(id, "tools/call: missing `name`")));
            };
            let args = req.params.get("arguments").cloned().unwrap_or(Value::Null);
            // An unknown tool name is a *invalid params* mistake, not a tool
            // execution failure: `tools/call` was well-formed and the value of
            // one parameter names nothing this server offers. MCP keeps
            // protocol errors (JSON-RPC error) distinct from tool execution
            // errors (`isError: true`), and this is the former. It used to
            // fall into the `isError` envelope below, so a strict client saw a
            // tool result where the spec calls for an error — and `email-mcp`
            // answered the same condition with JSON-RPC -32602, so the two
            // servers disagreed about what a typo means.
            if !registry.contains(name) {
                return Ok(Some(invalid_params(
                    id,
                    &format!("tools/call: unknown tool: {name}"),
                )));
            }
            // Per the MCP spec, a *tool execution* failure (bad arguments,
            // broker/session error) is a 200-shaped envelope flagged with
            // `"isError": true` instead of a JSON-RPC error.
            let (text, is_error) = match registry.call(name, args).await {
                Ok(body) => match serde_json::to_string(&body) {
                    Ok(t) => (t, false),
                    // Serialisation failing is a server fault: report it in
                    // the tool envelope rather than dropping the reply.
                    Err(e) => (format!("internal: cannot serialise tool result: {e}"), true),
                },
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

    /// A notification must get no reply even when the request is malformed.
    ///
    /// The `tools/call` "missing `name`" arm answered one unconditionally,
    /// producing a `{"id":null,...}` envelope for a message that must not be
    /// answered at all.
    #[tokio::test]
    async fn a_malformed_notification_still_gets_no_reply() {
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{}}"#,
            &registry(),
        )
        .await
        .expect("dispatch must not fail");
        assert!(resp.is_none(), "a notification got a reply: {resp:?}");
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
        let resp = dispatch(r#"{"jsonrpc":"2.0","method":"tools/list"}"#, &registry())
            .await
            .unwrap();
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_invalid_params_error_with_its_id() {
        // MCP separates protocol errors (JSON-RPC `error`) from *tool
        // execution* failures (`isError: true`). A name that is not in the
        // catalogue is the former: the request was well-formed and the value
        // of `params.name` names nothing. This used to be folded into the
        // tool-result envelope, which `email-mcp` (and the spec) call
        // -32602.
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"ghost","arguments":{}}}"#,
            &registry(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp["id"], 99);
        assert!(resp.get("result").is_none(), "not a tool result: {resp}");
        assert_eq!(resp["error"]["code"], -32_602);
        assert!(
            resp["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("unknown tool")),
            "got: {resp}"
        );
    }

    #[tokio::test]
    async fn tools_call_missing_name_returns_protocol_error_with_its_id() {
        // Protocol-level errors (malformed parameters) must remain
        // JSON-RPC errors, NOT `isError`. The MCP spec only reserves
        // `isError` for tool-level failures (the request was well-formed but
        // the tool rejected it).
        //
        // The reply must keep the caller's `id`: this used to return `Err`,
        // which the stdio loop turned into `{"id":null,"error":{"code":-32700}}`
        // — blaming the *syntax* of a valid request and leaving the client
        // unable to match the reply to request 2.
        let resp = dispatch(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"arguments":{}}}"#,
            &registry(),
        )
        .await
        .expect("a bad-parameter request is not a transport failure")
        .expect("an id-carrying request gets a reply");
        assert_eq!(resp["id"], 2, "the request id must survive: {resp}");
        assert_eq!(resp["error"]["code"], -32_602);
        let msg = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("missing") && msg.contains("name"), "got {msg}");
    }

    /// A request that is valid JSON but has no `method` is a *bad request*,
    /// not a transport fault: its reply must keep the caller's id and use
    /// -32600, not come back as `{"id":null,"error":{"code":-32700}}`.
    #[tokio::test]
    async fn a_request_without_a_method_keeps_its_id() {
        let resp = dispatch(r#"{"jsonrpc":"2.0","id":7,"params":{}}"#, &registry())
            .await
            .expect("a bad-field request is not a transport failure")
            .expect("an id-carrying request gets a reply");
        assert_eq!(resp["id"], 7, "the id must survive: {resp}");
        assert_eq!(resp["error"]["code"], -32_600);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("method"));

        // A genuinely unparseable line has no id to recover and still
        // reports a transport error (which the loop turns into -32700).
        assert!(dispatch("not json", &registry()).await.is_err());
    }
}
