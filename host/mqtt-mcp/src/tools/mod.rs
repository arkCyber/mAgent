//! Tool registry — the set of MCP tools the server exposes.
//!
//! Each tool is a single struct implementing the `dispatch` method
//! on a shared `MqttClient`. The registry holds them as a `Vec`
//! so `tools/list` returns the schema and `tools/call` can find
//! the right handler in one linear pass.
//!
//! ## Tools
//!
//! | Tool              | Purpose                                          |
//! |-------------------|--------------------------------------------------|
//! | `publish_event`   | Publish a UTF-8 payload to a topic.              |
//! | `subscribe_topic` | Subscribe to a topic and (best-effort) tail.     |
//! | `broker_status`   | Return connection state for diagnostics.         |

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};

use crate::mqtt_client::MqttClient;

mod broker_status;
mod publish_event;
mod subscribe_topic;

pub use broker_status::BrokerStatusTool;
pub use publish_event::PublishEventTool;
pub use subscribe_topic::SubscribeTopicTool;

/// Envelope returned by every tool call. We wrap the tool's
/// payload in a struct so we can add metadata (broker endpoint,
/// resolved QoS) without changing each tool's signature.
#[derive(Debug, Serialize)]
pub struct ToolOutput {
    /// Tool-specific result. Format depends on the tool.
    pub result: Value,
    /// Diagnostics: broker endpoint we talked to.
    pub broker: String,
}

impl ToolOutput {
    fn ok(result: Value, client: &MqttClient) -> Self {
        Self {
            result,
            broker: client.config().broker_endpoint(),
        }
    }
}

/// Trait every MCP tool implements. The body returns a JSON
/// value that the dispatcher wraps in a `text` content block.
pub trait Tool: Send + Sync {
    /// Stable name used in `tools/call`'s `params.name`.
    fn name(&self) -> &'static str;
    /// One-line human description; surfaces in `tools/list`.
    fn description(&self) -> &'static str;
    /// JSON Schema fragment for the tool's `params.arguments`.
    /// Should be a complete schema object (`{"type": "object",
    /// "properties": {...}, "required": [...]}`).
    fn input_schema(&self) -> Value;
    /// Run the tool. `args` is the `arguments` field of the
    /// `tools/call` request.
    fn dispatch<'a>(
        &'a self,
        client: &'a MqttClient,
        args: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>>;
}/// Bundle of all tools the server exposes. Cheap to clone —
/// each tool is wrapped in an `Arc` so the registry itself can
/// be shared across tasks without per-call locks.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: std::sync::Arc<Vec<Box<dyn Tool>>>,
}

impl ToolRegistry {
    /// Build the production registry with all tools wired up.
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(vec![
                Box::new(PublishEventTool),
                Box::new(SubscribeTopicTool),
                Box::new(BrokerStatusTool),
            ]),
        }
    }

    /// Empty registry for tests that only exercise the dispatcher
    /// surface.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            inner: std::sync::Arc::new(Vec::new()),
        }
    }

    /// MCP `tools/list` payload: `[{name, description, inputSchema}, …]`.
    pub fn list(&self) -> Vec<Value> {
        self.inner
            .iter()
            .map(|t| {
                json!({
                    "name": t.name(),
                    "description": t.description(),
                    "inputSchema": t.input_schema(),
                })
            })
            .collect()
    }

    /// MCP `tools/call` dispatch. Errors propagate as JSON-RPC
    /// error envelopes (see `jsonrpc::dispatch`).
    pub async fn call(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        let tool = self
            .inner
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
        // Read the global client. If `install_client` wasn't called
        // (e.g. someone constructed a `ToolRegistry::new()` for
        // unit tests without a broker), surface a clean error
        // instead of panicking — the caller can distinguish a
        // misconfigured server from a real tool failure.
        let client = CLIENT
            .get()
            .ok_or_else(|| anyhow::anyhow!("mqtt client not initialised; call install_client() in main()"))?;
        let result = tool.dispatch(client, args).await?;
        Ok(serde_json::to_value(ToolOutput::ok(result, client))?)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global MQTT client handle. Populated in `main` after the
/// broker handshake; tools read it via `ToolRegistry::call`.
/// We use a `OnceLock` rather than passing the client into every
/// tool call because the registry itself is shared via `Arc`
/// across tasks, and threading the client through it would
/// double the `Arc` count.
static CLIENT: std::sync::OnceLock<MqttClient> = std::sync::OnceLock::new();

/// Initialise the global client. Must be called exactly once at
/// startup, before any `tools/call` dispatch.
pub fn install_client(client: MqttClient) {
    if CLIENT.set(client).is_err() {
        log::warn!("mqtt-mcp: client installed more than once");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_three_tools() {
        // Smoke-test that `tools/list` returns the three known
        // tools in the documented order. The MCP client uses
        // these names verbatim, so renaming a tool is a breaking
        // protocol change.
        let r = ToolRegistry::new();
        let list = r.list();
        let names: Vec<&str> = list
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["publish_event", "subscribe_topic", "broker_status"]
        );
    }

    #[test]
    fn list_includes_input_schema() {
        // Every tool entry must carry a complete JSON Schema so
        // MCP clients can render an argument form. We only check
        // that the field is present and typed as `object` — the
        // tool-specific schema lives in each `*_tool.rs` and is
        // covered by the per-tool test module.
        let r = ToolRegistry::new();
        for entry in r.list() {
            assert_eq!(entry["inputSchema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_error() {
        // No client installed — but the unknown-name branch must
        // be hit first so we don't get the "client not initialised"
        // message instead.
        let r = ToolRegistry::new();
        let err = r.call("does_not_exist", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool"));
    }

    /// Test-only helper: replace the global client with a stub.
    /// Production code never calls this. Kept `pub(crate)` so future
    /// integration tests can install a stub without going through
    /// `main`.
    #[allow(dead_code)]
    pub(crate) fn test_install_client(client: MqttClient) {
        let _ = CLIENT.set(client);
    }
}
