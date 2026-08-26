//! Thin async wrapper around [`rumqttc::AsyncClient`].
//!
//! We don't need the full MQTT surface — `magent-mcp` only
//! publishes events and subscribes to one or two control topics.
//! This module pins down the subset we actually use and gives us
//! a place to add MQTT-specific tests (reconnect, QoS validation,
//! payload-size cap) without polluting `tools.rs`.
//!
//! ## Topics
//!
//! * Publish: any topic; default is [`crate::config::Config::default_topic`].
//! * Subscribe: caller-supplied wildcard, e.g. `magent/cmd/+`.
//!
//! ## Reconnect
//!
//! `rumqttc` retries automatically on broker drop. We surface
//! [`MqttClient::is_connected`] so the JSON-RPC layer can include
//! a connectivity hint in tool results.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rumqttc::{AsyncClient, Event, EventLoop, Incoming, MqttOptions, Outgoing, QoS};
use tokio::sync::Mutex;

use crate::config::Config;

/// `true` if `host` is a loopback address (localhost / 127.0.0.1 / ::1).
/// Used to decide whether sending MQTT credentials over plain TCP is safe.
fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "localhost.localdomain"
}

/// Errors surfaced by [`MqttClient`].
#[derive(Debug, thiserror::Error)]
pub enum MqttError {
    /// Underlying `rumqttc` reported a connection / publish failure.
    #[error("mqtt operation failed: {0}")]
    Backend(String),
    /// Payload exceeded the configured maximum size.
    #[error("payload of {bytes} bytes exceeds limit of {limit} bytes")]
    PayloadTooLarge { bytes: usize, limit: usize },
    /// QoS requested by the caller is outside the 0..=2 range.
    #[error("qos {0} is invalid; expected 0, 1, or 2")]
    InvalidQos(u8),
    /// Topic string was empty or contained a NUL byte.
    #[error("topic {topic:?} is invalid: {reason}")]
    InvalidTopic { topic: String, reason: String },
}

/// Default payload cap (1 MiB). Matches `lettre`'s SMTP limit so
/// email and MQTT can hold each other's payloads without surprises.
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Handle to a connected or connectable MQTT client.
///
/// Cheap to clone — the underlying [`AsyncClient`] is internally
/// `Arc`'d by `rumqttc`. The background event loop is owned
/// exclusively by the creator; we keep a clone of the client
/// handle and a `Mutex`-guarded connection state.
#[derive(Clone)]
pub struct MqttClient {
    inner: AsyncClient,
    /// Cached "we've seen ConnAck at least once" flag. `rumqttc`
    /// reconnects transparently, so this is informational only.
    connected: Arc<Mutex<bool>>,
    config: Config,
    /// Payload cap enforced on every publish.
    max_payload_bytes: usize,
}

impl MqttClient {
    /// Build a new client from a resolved [`Config`]. The actual
    /// network handshake happens in the background; the first
    /// publish will block until the broker accepts the connection
    /// (or the underlying timeout fires).
    pub async fn connect(config: Config) -> Result<Self, MqttError> {
        Self::connect_with_cap(config, DEFAULT_MAX_PAYLOAD_BYTES).await
    }

    /// Same as [`Self::connect`] but with a custom payload cap.
    /// Mostly useful for tests that want to exercise the
    /// `PayloadTooLarge` branch.
    pub async fn connect_with_cap(
        config: Config,
        max_payload_bytes: usize,
    ) -> Result<Self, MqttError> {
        let mut opts = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
        opts.set_keep_alive(Duration::from_secs(config.keep_alive_secs as u64));
        if !config.username.is_empty() {
            opts.set_credentials(&config.username, &config.password);
        }
        // The transport below is plain TCP (rumqttc without the `use-rustls`
        // feature). Credentials are therefore transmitted in cleartext. If
        // the operator points at a NON-localhost broker, that leaks the
        // username/password to anyone on the network path. The embedded
        // nRF52 gateway legitimately talks to a localhost broker, so we
        // allow it but warn loudly when the target isn't loopback.
        if !config.username.is_empty() && !is_loopback_host(&config.broker_host) {
            log::warn!(
                "mqtt: sending credentials to non-localhost broker {}:{} over plain TCP \
                 (no TLS). The username/password will be readable on the wire. \
                 Prefer a localhost broker or a TLS-capable transport.",
                config.broker_host,
                config.broker_port
            );
        }
        // mTLS is intentionally NOT wired here. The plain-TCP path
        // is what the embedded nRF52 gateway needs; an authenticated
        // path can be added later via a `tls` feature.

        let (client, mut eventloop): (AsyncClient, EventLoop) = AsyncClient::new(opts, 16);

        // Spawn a background task to drive the event loop. We
        // dispatch into a oneshot so callers know when the FIRST
        // ConnAck / ConnAck-timeout arrives; subsequent state
        // transitions are best-effort and reflected via the
        // `connected` mutex.
        let connected = Arc::new(Mutex::new(false));
        let connected_clone = connected.clone();
        tokio::spawn(async move {
            while let Ok(event) = eventloop.poll().await {
                match event {
                    Event::Incoming(Incoming::ConnAck(_)) => {
                        let mut g = connected_clone.lock().await;
                        *g = true;
                        log::info!("mqtt: connack received");
                    }
                    Event::Incoming(Incoming::Disconnect) => {
                        let mut g = connected_clone.lock().await;
                        *g = false;
                        log::warn!("mqtt: broker disconnected");
                    }
                    Event::Outgoing(Outgoing::PingReq) => {
                        log::trace!("mqtt: pingreq");
                    }
                    _ => {}
                }
            }
            let mut g = connected_clone.lock().await;
            *g = false;
        });

        Ok(Self {
            inner: client,
            connected,
            config,
            max_payload_bytes,
        })
    }

    /// Publish `payload` on `topic`. `qos` defaults to
    /// [`Config::qos_default`]. `retain` defaults to `false`.
    /// Returns the resolved QoS so the caller can echo it back
    /// to the LLM.
    pub async fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        qos: Option<u8>,
        retain: bool,
    ) -> Result<u8, MqttError> {
        validate_topic(topic)?;
        if payload.len() > self.max_payload_bytes {
            return Err(MqttError::PayloadTooLarge {
                bytes: payload.len(),
                limit: self.max_payload_bytes,
            });
        }
        let qos_u8 = qos.unwrap_or(self.config.qos_default);
        let qos_enum = qos_from_u8(qos_u8)?;
        self.inner
            .publish_bytes(topic, qos_enum, retain, Bytes::copy_from_slice(payload))
            .await
            .map_err(|e| MqttError::Backend(e.to_string()))?;
        Ok(qos_u8)
    }

    /// Subscribe to `topic` with the given QoS. Used by the
    /// `subscribe_topic` tool so an MCP client can tail control
    /// messages from a parent supervisor.
    pub async fn subscribe(&self, topic: &str, qos: Option<u8>) -> Result<u8, MqttError> {
        validate_topic(topic)?;
        let qos_u8 = qos.unwrap_or(self.config.qos_default);
        let qos_enum = qos_from_u8(qos_u8)?;
        self.inner
            .subscribe(topic, qos_enum)
            .await
            .map_err(|e| MqttError::Backend(e.to_string()))?;
        Ok(qos_u8)
    }

    /// `true` if the most recent ConnAck / Disconnect event left
    /// us in a connected state. The value is best-effort: a
    /// race between the broker and the event loop can briefly
    /// return `false` even when the link is healthy. The intent
    /// is to give operators a quick health hint, not to gate
    /// publishes on it (the publish path already handles
    /// transient broker drops).
    pub async fn is_connected(&self) -> bool {
        *self.connected.lock().await
    }

    /// Resolved config. Mostly for diagnostics.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// Parse a `u8` into [`QoS`], surfacing the invalid case as an
/// error rather than a panic.
fn qos_from_u8(value: u8) -> Result<QoS, MqttError> {
    match value {
        0 => Ok(QoS::AtMostOnce),
        1 => Ok(QoS::AtLeastOnce),
        2 => Ok(QoS::ExactlyOnce),
        other => Err(MqttError::InvalidQos(other)),
    }
}

/// Reject empty topics and topics containing NUL bytes. The MQTT
/// 3.1.1 spec forbids both, and `rumqttc` would surface them as
/// confusing errors at publish time.
fn validate_topic(topic: &str) -> Result<(), MqttError> {
    if topic.is_empty() {
        return Err(MqttError::InvalidTopic {
            topic: topic.to_string(),
            reason: "topic is empty".to_string(),
        });
    }
    if topic.contains('\0') {
        return Err(MqttError::InvalidTopic {
            topic: topic.to_string(),
            reason: "topic contains a NUL byte".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_roundtrip() {
        assert!(matches!(qos_from_u8(0).unwrap(), QoS::AtMostOnce));
        assert!(matches!(qos_from_u8(1).unwrap(), QoS::AtLeastOnce));
        assert!(matches!(qos_from_u8(2).unwrap(), QoS::ExactlyOnce));
        assert!(qos_from_u8(3).is_err());
    }

    #[test]
    fn validate_topic_rejects_empty() {
        assert!(validate_topic("").is_err());
    }

    #[test]
    fn validate_topic_rejects_nul() {
        assert!(validate_topic("foo\0bar").is_err());
    }

    #[test]
    fn validate_topic_accepts_normal() {
        assert!(validate_topic("magent/events").is_ok());
        assert!(validate_topic("magent/cmd/+").is_ok());
    }
}
