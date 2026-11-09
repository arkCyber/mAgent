//! Link-layer adapter trait.
//!
//! A [`LinkAdapter`] is anything that can produce and consume raw byte
//! buffers on behalf of the agent: a BLE radio, an MQTT subscription, an
//! RS232 UART, an SPI peripheral, a stdin reader, … Each transport
//! implements this trait and hands its bytes to an
//! [`crate::ingress::IngressGateway`] which is responsible for routing
//! and signing.
//!
//! ## Design choices
//!
//! * **Synchronous, byte-oriented API.** No `async`, no async traits —
//!   this keeps the trait implementable on bare-metal targets without
//!   pulling in `embedded-io-async` and works with `std::net` on the
//!   host. Higher layers can wrap it in a task / thread.
//! * **Bounded buffer sizes** via [`heapless::Vec`]. The trait method
//!   takes `&mut [u8]` from the caller and returns the byte count. That
//!   way the gateway controls its own memory budget.
//! * **`source_kind` is mandatory.** The gateway needs to know where a
//!   payload came from for logging, audit, and (in `Signed` mode)
//!   binding the source into the envelope.
//! * **Trait is `no_std` safe.** Only `core::fmt::Debug` + `Error` type
//!   constraints. This means `LinkAdapter` works on nRF52 / ESP32 firmwares.
//!
//! ## Implementing a custom adapter
//!
//! ```ignore
//! use magent_core::communication::link::{LinkAdapter, IngressSource};
//!
//! pub struct MyRs232 { /* uart handle */ }
//!
//! #[derive(Debug)]
//! pub enum MyError { Busy, Overrun }
//! impl core::fmt::Display for MyError {
//!     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//!         match self {
//!             Self::Busy => f.write_str("busy"),
//!             Self::Overrun => f.write_str("overrun"),
//!         }
//!     }
//! }
//! impl std::error::Error for MyError {} // host-only; on embedded, omit.
//!
//! impl LinkAdapter for MyRs232 {
//!     type Error = MyError;
//!     fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
//!         // read up to buf.len() bytes from the UART
//!         todo!()
//!     }
//!     fn send(&mut self, _buf: &[u8]) -> Result<(), Self::Error> { Ok(()) }
//!     fn source_kind(&self) -> IngressSource { IngressSource::Rs232 { port: heapless::String::try_from("COM1").unwrap() } }
//!     fn is_connected(&self) -> bool { true }
//! }
//! ```
//!
//! The concrete `ManualAdapter` (CLI / stdin) and `MqttAdapter` (host
//! MQTT-over-TCP via a worker thread) implementations live in
//! `communication::manual` and `communication::mqtt` respectively, both
//! gated behind their own Cargo features.

use heapless::String;

/// Where a payload originated from. Embedded in logs, audit records,
/// and (optionally) the `SignedMessage` envelope metadata.
///
/// The variants are deliberately *not* a 1:1 mirror of the concrete
/// adapters — `IngressSource` describes a logical source kind, while a
/// single adapter may produce multiple sources (e.g. one MQTT
/// subscription per topic, one RS232 port per device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressSource {
    /// MQTT topic. The `topic` is the literal subscription string the
    /// adapter was bound to.
    Mqtt {
        /// MQTT subscription topic (literal string the adapter bound to).
        topic: String<128>,
    },
    /// RS232 / UART serial port.
    Rs232 {
        /// Human-readable port identifier (e.g. `"UART0"`, `"COM1"`).
        port: String<16>,
    },
    /// SPI bus. `cs_pin` identifies the chip-select line, which usually
    /// maps 1:1 to the peripheral.
    Spi {
        /// Chip-select pin index (logical identifier, not the GPIO number).
        cs_pin: u8,
    },
    /// Manual / CLI / button input from a human operator.
    Manual,
    /// Generic catch-all for adapters that don't fit the above (BLE,
    /// LoRa, …). `label` is a short human-readable identifier.
    Other {
        /// Short human-readable label describing this source (e.g. `"ble:adv"`).
        label: String<32>,
    },
}

impl IngressSource {
    /// Stable short identifier used in logs and audit entries. Keep it
    /// deterministic so log scrapers can group events.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Mqtt { .. } => "mqtt",
            Self::Rs232 { .. } => "rs232",
            Self::Spi { .. } => "spi",
            Self::Manual => "manual",
            Self::Other { .. } => "other",
        }
    }
}

/// Synchronous, byte-oriented transport trait.
///
/// Adapters are owned (`Box<dyn LinkAdapter<Error = E>>` or generic
/// over a concrete type) by the [`crate::ingress::IngressGateway`].
/// Implementations must be cheap to `poll` — the gateway may call it in
/// a tight loop.
pub trait LinkAdapter {
    /// Adapter-specific error type. Must be `Debug` so the gateway can
    /// log adapter failures without pulling in `Display`.
    type Error: core::fmt::Debug;

    /// Try to read up to `buf.len()` bytes into `buf`. Returns the
    /// number of bytes actually read.
    ///
    /// Contract:
    /// * `Ok(0)` means *no data available right now* — the gateway
    ///   should move on to the next adapter rather than spinning.
    /// * `Ok(n)` with `n <= buf.len()` is a valid frame; the gateway
    ///   will treat the bytes as opaque payload.
    /// * `Ok(n)` with `n > buf.len()` violates this contract: nothing
    ///   beyond `buf` was written, so the gateway discards the frame and
    ///   logs a warning rather than slicing out of range.
    /// * `Err(_)` means the adapter is in a failure state. The gateway
    ///   will log the error and skip the adapter for this round.
    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Send a buffer back over the same transport. Used for command
    /// responses, ACKs, etc. Adapters that are receive-only may return
    /// `Ok(())` as a no-op.
    fn send(&mut self, buf: &[u8]) -> Result<(), Self::Error>;

    /// Logical source identifier. Called once at registration and on
    /// demand by the gateway. Must be cheap.
    fn source_kind(&self) -> IngressSource;

    /// Whether the transport is currently usable. The gateway skips
    /// disconnected adapters during `ingest`.
    fn is_connected(&self) -> bool;
}

/// Async, byte-oriented transport trait for embedded environments.
///
/// Mirrors [`LinkAdapter`] but uses `core::future::Future` so it can be
/// driven by an embassy (or any other cooperative) executor. The two
/// traits are intentionally separate rather than a single `async-trait`
/// variant because:
///
/// 1. **Sync adapters are easier to unit-test on a host.** The host
///    integration tests in `tests/ingress_tests.rs` use the synchronous
///    `LinkAdapter` so they don't have to pull in an async runtime.
/// 2. **Sync adapters don't pay for a state machine.** On a Cortex-M0
///    with no MPU, hand-rolled async via `core::future::poll_fn` is
///    noticeably larger than a tight `poll()` loop.
/// 3. **The gateway can drive both.** `IngressGateway` offers
///    [`crate::ingress::IngressGateway::ingest`] (sync) and
///    [`crate::ingress::IngressGateway::ingest_async`] (async); a
///    caller chooses which side of the gateway's adapter pool to use.
///
/// ## Contract for `poll`
///
/// Same as [`LinkAdapter::poll`], except `poll` is `async`:
///
/// * `Ok(0)` future — *no data available right now*. Implementations
///   should resolve immediately (or after a tiny delay) rather than
///   block forever; the gateway treats `Ok(0)` as "move on".
/// * `Ok(n)` future with `n <= buf.len()` — a valid frame.
/// * `Ok(n)` future with `n > buf.len()` — contract violation; the
///   gateway discards the frame (nothing past `buf` was written).
/// * `Err(_)` future — adapter in a failure state; gateway logs + skips.
pub trait AsyncLinkAdapter {
    /// Adapter-specific error type. Must be `Debug` so the gateway can
    /// log adapter failures without pulling in `Display`.
    type Error: core::fmt::Debug;

    /// Async equivalent of [`LinkAdapter::poll`].
    fn poll<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>>;

    /// Async equivalent of [`LinkAdapter::send`].
    fn send<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl core::future::Future<Output = Result<(), Self::Error>>;

    /// Logical source identifier. Same semantics as
    /// [`LinkAdapter::source_kind`].
    fn source_kind(&self) -> IngressSource;

    /// Connection liveness, same as [`LinkAdapter::is_connected`].
    fn is_connected(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `IngressSource` variant must map to its stable `tag()`
    /// string — log scrapers group events on these, so a rename here
    /// (or a missing arm) is a breaking change.
    #[test]
    fn every_source_variant_has_a_stable_tag() {
        let mqtt = IngressSource::Mqtt {
            topic: String::try_from("sensors/temp").expect("fits"),
        };
        let rs232 = IngressSource::Rs232 {
            port: String::try_from("UART0").expect("fits"),
        };
        let spi = IngressSource::Spi { cs_pin: 7 };
        let manual = IngressSource::Manual;
        let other = IngressSource::Other {
            label: String::try_from("ble:adv").expect("fits"),
        };

        assert_eq!(mqtt.tag(), "mqtt");
        assert_eq!(rs232.tag(), "rs232");
        assert_eq!(spi.tag(), "spi");
        assert_eq!(manual.tag(), "manual");
        assert_eq!(other.tag(), "other");
    }

    /// `tag()` ignores the payload: two different MQTT topics still
    /// share the `"mqtt"` tag (the source *kind*, not the instance).
    #[test]
    fn tag_is_independent_of_the_payload() {
        let a = IngressSource::Mqtt {
            topic: String::try_from("a").expect("fits"),
        };
        let b = IngressSource::Mqtt {
            topic: String::try_from("a/very/different/topic").expect("fits"),
        };
        assert_eq!(a.tag(), b.tag());

        let p1 = IngressSource::Spi { cs_pin: 0 };
        let p2 = IngressSource::Spi { cs_pin: 255 };
        assert_eq!(p1.tag(), p2.tag());
    }

    /// The enum derives `PartialEq`/`Eq`/`Clone`; equal payloads must
    /// compare equal and clone to an equal value.
    #[test]
    fn sources_compare_and_clone_by_value() {
        let src = IngressSource::Other {
            label: String::try_from("lora").expect("fits"),
        };
        let copy = src.clone();
        assert_eq!(src, copy);
        assert_ne!(src, IngressSource::Manual);
    }
}
