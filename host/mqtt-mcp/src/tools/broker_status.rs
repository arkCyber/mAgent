//! `broker_status` — diagnostic probe.
//!
//! Returns the resolved broker endpoint, the current connection
//! state (best-effort), and the configured defaults. Used by
//! the CLI's `doctor` flow to confirm the MCP server can reach
//! its broker.

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use super::Tool;

pub struct BrokerStatusTool;

impl Tool for BrokerStatusTool {
    fn name(&self) -> &'static str {
        "broker_status"
    }

    fn description(&self) -> &'static str {
        "Return broker endpoint, current connection state, and configured defaults. \
         Useful for `magent doctor`-style diagnostics."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn dispatch<'a>(
        &'a self,
        client: &'a crate::mqtt_client::MqttClient,
        _args: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            let cfg = client.config();
            Ok(json!({
                "broker": cfg.broker_endpoint(),
                "client_id": cfg.client_id,
                "keep_alive_secs": cfg.keep_alive_secs,
                "default_topic": cfg.default_topic,
                "qos_default": cfg.qos_default,
                "connected": client.is_connected().await,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    #[test]
    fn schema_accepts_no_arguments() {
        // `broker_status` is a diagnostics probe — it must not
        // require any args. The schema also explicitly forbids
        // extra keys (`additionalProperties: false`) so a typo
        // surfaces as a validation error instead of being
        // silently ignored.
        let schema = BrokerStatusTool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("required").is_none() || schema["required"].as_array().unwrap().is_empty());
        assert_eq!(schema["additionalProperties"], false);
    }
}
