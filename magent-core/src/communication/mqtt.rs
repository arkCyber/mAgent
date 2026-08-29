//! MQTT adapter (host-only, in-memory frame queue).
//!
//! Provides the [`MqttAdapter`] type that implements [`LinkAdapter`]
//! over a tiny in-memory inbox. Bytes are pushed in by *something*
//! (a real MQTT client, an integration test, a CLI script) and
//! drained by [`LinkAdapter::poll`].
//!
//! The adapter itself does **not** speak the MQTT wire protocol.
//! Doing that properly in `magent-core` would require pulling
//! `rumqttc` (or a hand-rolled equivalent) into the host test
//! build, which transitively requires `tokio` / `bytes` /
//! `futures-sink`. None of those are appropriate for a crate that
//! also targets `no_std` firmware (ESP32, nRF52).
//!
//! ## How to wire a real MQTT source
//!
//! Wrap the adapter in an `Arc<Mutex<…>>` and feed it from whatever
//! MQTT client your environment provides. Two production-ready
//! templates live in the workspace:
//!
//! 1. **`examples/magent-mqtt-ingest/`** — `rumqttc::AsyncClient`
//!    subscribed on a tokio task, with the resulting `Publish`
//!    payloads pushed into the adapter behind a `Mutex`. The same
//!    example wires the adapter into an [`IngressGateway`] in
//!    `Signed` mode and round-trips the envelope back through
//!    `web3::verify_signed_message`, so it doubles as an end-to-end
//!    smoke test for the gateway.
//!
//! 2. **`examples/email-mqtt-bridge/`** — the original reverse
//!    pattern: an IMAP mailbox reader publishes its decoded
//!    payloads to an outbound topic. Useful when integrating with
//!    brokers that fan out agent commands instead of fan in agent
//!    telemetry.
//!
//! In both cases the gateway interaction is unchanged: the adapter
//! reports its source as [`IngressSource::Mqtt`] with the
//! subscription topic embedded.
//!
//! ## Threading model
//!
//! The adapter stores its inbox in a plain
//! `heapless::Vec<u8, 1024>`. Concurrent pushes from a network
//! thread therefore require the caller to wrap the adapter in a
//! `Mutex` (or `RwLock` when readers are exclusive). The
//! [`LinkAdapter::poll`] contract is *synchronous and non-blocking*:
//! the gateway must be able to call it in a tight loop, so the
//! producer side must never hold the lock across an `await`. Use a
//! short critical section: clone the payload bytes, release, then
//! push.
//!
//! [`IngressGateway`]: crate::ingress::IngressGateway

use super::link::{IngressSource, LinkAdapter};
use heapless::Vec;

/// Stub MQTT adapter. Receives frames pushed in by an external MQTT
/// worker (see the module docs).
#[derive(Debug)]
pub struct MqttAdapter {
    topic: heapless::String<128>,
    /// Inbound frames pushed by the worker thread / external client.
    /// `poll` drains this queue FIFO.
    inbox: Vec<u8, 1024>,
    connected: bool,
}

impl MqttAdapter {
    /// Create a stub adapter bound to the given MQTT topic. The actual
    /// SUBSCRIBE handshake must be performed by the caller's MQTT
    /// client — push received PUBLISH payloads in via [`Self::push_frame`].
    pub fn new(topic: &str) -> Self {
        Self {
            topic: heapless::String::try_from(topic).unwrap_or_default(),
            inbox: Vec::new(),
            connected: true,
        }
    }

    /// Push a received MQTT PUBLISH payload into the adapter's inbox.
    /// The next call to [`LinkAdapter::poll`] will drain it.
    ///
    /// ## Capacity
    ///
    /// The inbox holds a single frame at a time (`heapless::Vec<u8, 1024>`).
    /// If a frame is still pending when a new `push_frame` arrives, the
    /// pending one is replaced. This matches the gateway's
    /// "one frame per poll" semantics: the gateway is expected to
    /// drain on every poll before a new frame would be needed.
    pub fn push_frame(&mut self, payload: &[u8]) -> Result<(), MqttError> {
        if payload.len() > self.inbox.capacity() {
            return Err(MqttError::FrameTooLarge {
                size: payload.len(),
                capacity: self.inbox.capacity(),
            });
        }
        // Compact: drop the old frame if we never drained it.
        self.inbox.clear();
        self.inbox
            .extend_from_slice(payload)
            .map_err(|_| MqttError::FrameTooLarge {
                size: payload.len(),
                capacity: self.inbox.capacity(),
            })?;
        Ok(())
    }

    /// Topic this adapter reports as its source. Useful for log
    /// filtering and for matching against [`Self::new`]'s argument at
    /// runtime.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Number of bytes currently sitting in the inbox. Mainly useful
    /// for integration tests and metrics; production code reads via
    /// [`LinkAdapter::poll`].
    pub fn pending_len(&self) -> usize {
        self.inbox.len()
    }

    /// Maximum number of bytes a single frame can hold.
    pub fn capacity(&self) -> usize {
        self.inbox.capacity()
    }
}

/// Errors that can come out of a [`MqttAdapter`].
#[derive(Debug)]
pub enum MqttError {
    /// The PUBLISH frame exceeded the adapter's inbox capacity.
    FrameTooLarge {
        /// Actual byte size of the frame that was rejected.
        size: usize,
        /// Adapter's inbox capacity in bytes (the upper bound that was exceeded).
        capacity: usize,
    },
}

impl core::fmt::Display for MqttError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FrameTooLarge { size, capacity } => {
                write!(f, "MQTT frame {size}B exceeds adapter capacity {capacity}B")
            }
        }
    }
}

// `std::error::Error` is host-only. On embedded targets the trait is
// omitted and consumers fall back to `core::fmt::Debug` for logging,
// which is what `esp_println::println!` already requires anyway.
#[cfg(feature = "std")]
impl std::error::Error for MqttError {}

impl LinkAdapter for MqttAdapter {
    type Error = MqttError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, MqttError> {
        if self.inbox.is_empty() {
            return Ok(0);
        }
        let n = self.inbox.len().min(buf.len());
        buf[..n].copy_from_slice(&self.inbox[..n]);
        // Drop what we copied. We deliberately do NOT preserve the
        // tail — callers are expected to call `push_frame` again for
        // the next frame.
        self.inbox.clear();
        Ok(n)
    }

    fn send(&mut self, _buf: &[u8]) -> Result<(), MqttError> {
        // Stub: outbound MQTT PUBLISH is the caller's responsibility.
        Ok(())
    }

    fn source_kind(&self) -> IngressSource {
        IngressSource::Mqtt {
            topic: self.topic.clone(),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}
