//! `subscribe_topic` — register interest in a topic.
//!
//! The MCP wire shape can't easily stream events back to the
//! client, so this tool returns a one-shot acknowledgment that
//! the subscription has been registered. A future P-iteration
//! could add SSE-style notification streaming; for now operators
//! wire `mosquitto_sub` alongside to capture the actual messages.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;

#[derive(Debug, Deserialize)]
struct Args {
    /// Topic filter, including wildcards (`+`, `#`).
    topic: String,
    /// Optional QoS override.
    #[serde(default)]
    qos: Option<u8>,
}

pub struct SubscribeTopicTool;

impl Tool for SubscribeTopicTool {
    fn name(&self) -> &'static str {
        "subscribe_topic"
    }

    fn description(&self) -> &'static str {
        "Subscribe to an MQTT topic. Returns a one-shot acknowledgment; \
         use a dedicated subscriber (mosquitto_sub, etc.) to capture the stream."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "Topic filter. Wildcards `+` and `#` are supported."
                },
                "qos": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2,
                    "description": "QoS level (0, 1, or 2). Defaults to `qos_default`."
                }
            },
            "required": ["topic"]
        })
    }

    fn dispatch<'a>(
        &'a self,
        client: &'a crate::mqtt_client::MqttClient,
        args: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            let args: Args = serde_json::from_value(args)?;
            let qos = client
                .subscribe(&args.topic, args.qos)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            Ok(json!({
                "subscribed": true,
                "topic": args.topic,
                "qos": qos,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    #[test]
    fn topic_is_required() {
        // `subscribe_topic` has no default topic — refusing a
        // subscribe without a target protects against accidentally
        // registering an empty filter (which would be a protocol
        // violation).
        let schema = SubscribeTopicTool.input_schema();
        let required = schema["required"].as_array().expect("required must be an array");
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "topic");
    }

    #[test]
    fn schema_clamps_qos_to_mqtt_range() {
        let schema = SubscribeTopicTool.input_schema();
        let qos = &schema["properties"]["qos"];
        assert_eq!(qos["minimum"], 0);
        assert_eq!(qos["maximum"], 2);
    }
}
