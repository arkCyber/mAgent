//! `publish_event` — publish a UTF-8 payload to a topic.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;

#[derive(Debug, Deserialize)]
struct Args {
    /// Topic to publish to. Falls back to the configured default
    /// when omitted.
    #[serde(default)]
    topic: Option<String>,
    /// UTF-8 payload. Either `payload` or `payload_json` may be
    /// supplied; `payload_json` is serialised to a compact JSON
    /// string before publishing.
    #[serde(default)]
    payload: Option<String>,
    /// JSON value to serialise and publish. Convenience for
    /// LLMs that already have structured data on hand.
    #[serde(default)]
    payload_json: Option<Value>,
    /// Optional QoS override. Must be 0, 1, or 2.
    #[serde(default)]
    qos: Option<u8>,
    /// Whether to set the RETAIN flag. Defaults to `false`.
    #[serde(default)]
    retain: bool,
}

pub struct PublishEventTool;

impl Tool for PublishEventTool {
    fn name(&self) -> &'static str {
        "publish_event"
    }

    fn description(&self) -> &'static str {
        "Publish a payload to an MQTT topic. Either `payload` (UTF-8 string) \
         or `payload_json` (any JSON value, serialised compactly) must be supplied."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "MQTT topic to publish to. Falls back to \
                                    the configured `default_topic` when omitted."
                },
                "payload": {
                    "type": "string",
                    "description": "UTF-8 payload to publish."
                },
                "payload_json": {
                    "description": "JSON value to serialise and publish. \
                                    Mutually exclusive with `payload`."
                },
                "qos": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2,
                    "description": "QoS level (0, 1, or 2). Defaults to `qos_default`."
                },
                "retain": {
                    "type": "boolean",
                    "default": false,
                    "description": "Set the RETAIN flag so late subscribers receive the last value."
                }
            },
            "anyOf": [
                { "required": ["payload"] },
                { "required": ["payload_json"] }
            ]
        })
    }

    fn dispatch<'a>(
        &'a self,
        client: &'a crate::mqtt_client::MqttClient,
        args: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            let args: Args = serde_json::from_value(args)?;
            let topic = args
                .topic
                .clone()
                .unwrap_or_else(|| client.config().default_topic.clone());

            // Resolve the byte payload. `payload_json` wins over
            // `payload` if both are supplied; that's the more useful
            // behaviour for structured events.
            let bytes: Vec<u8> = if let Some(v) = args.payload_json {
                serde_json::to_vec(&v)?
            } else if let Some(s) = args.payload {
                s.into_bytes()
            } else {
                anyhow::bail!("publish_event: either `payload` or `payload_json` is required");
            };

            let qos = client
                .publish(&topic, &bytes, args.qos, args.retain)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            Ok(json!({
                "published": true,
                "topic": topic,
                "bytes": bytes.len(),
                "qos": qos,
                "retain": args.retain,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    #[test]
    fn schema_requires_payload_or_payload_json() {
        // The schema uses `anyOf` so the LLM can pass either.
        // Make sure both branches are still present after the
        // last edit — silently dropping one would surface as
        // "tool call rejected" errors deep inside the model loop.
        let schema = PublishEventTool.input_schema();
        let branches = schema["anyOf"].as_array().expect("anyOf must be an array");
        assert_eq!(branches.len(), 2);
        let names: Vec<&str> = branches
            .iter()
            .flat_map(|b| b["required"].as_array().unwrap())
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(names.contains(&"payload"));
        assert!(names.contains(&"payload_json"));
    }

    #[test]
    fn schema_clamps_qos_to_mqtt_range() {
        // If someone removes the bounds, an out-of-range QoS
        // would propagate to the broker and surface as a
        // confusing protocol error. Lock the contract.
        let schema = PublishEventTool.input_schema();
        let qos = &schema["properties"]["qos"];
        assert_eq!(qos["minimum"], 0);
        assert_eq!(qos["maximum"], 2);
    }

    #[test]
    fn retain_field_is_default_false() {
        // `retain` defaults to false on the wire; the schema
        // documents this so the LLM doesn't have to guess.
        let schema = PublishEventTool.input_schema();
        assert_eq!(schema["properties"]["retain"]["default"], false);
        assert_eq!(schema["properties"]["retain"]["type"], "boolean");
    }
}
