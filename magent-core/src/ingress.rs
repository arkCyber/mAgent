//! Application-layer ingress gateway.
//!
//! Owns a fixed pool of [`LinkAdapter`]s, polls each in turn, tags the
//! resulting bytes with their [`IngressSource`], and — depending on the
//! configured [`IngressMode`] — either passes them through verbatim
//! (`Transparent`) or wraps them in a [`crate::web3::SignedMessage`]
//! signed by a device [`crate::web3::Identity`] (`Signed`).
//!
//! ## Why split this from `LinkAdapter`?
//!
//! The link layer is byte plumbing (PHY + protocol). Routing, source
//! tagging, and crypto-envelope binding are application concerns; they
//! belong above the link. Keeping them separate means:
//!
//! * Adding RS232 / SPI / BLE adapters never touches the gateway or
//!   the agent loop.
//! * Flipping the mode from `Transparent` to `Signed` (and back) is a
//!   one-line config change at the gateway level, not a per-adapter
//!   rebuild.
//! * Embedded builds can use `LinkAdapter` directly without pulling in
//!   `web3` / `Identity`.
//!
//! ## Mode semantics
//!
//! * [`IngressMode::Transparent`] — `ingest` returns the raw bytes plus
//!   the [`IngressSource`]. No signing is performed. Use this for
//!   trusted, on-device sources (e.g. an SPI temperature sensor) or
//!   when the upper layer signs the payload itself.
//!
//! * [`IngressMode::Signed`] — every payload is wrapped in a
//!   [`crate::web3::SignedMessage`] whose payload field is the raw
//!   bytes and whose signature is produced by the device
//!   [`crate::web3::Identity`]. The upper layer / blockchain tools
//!   receive a fully self-contained, verifiable envelope. Use this
//!   for external / untrusted inputs (MQTT, RS232, manual CLI) that
//!   need to be tied to the agent's DID for downstream audit /
//!   on-chain anchoring.
//!
//! ## Memory budget
//!
//! `IngressGateway` uses a single internal scratch buffer of
//! [`MAX_PAYLOAD`] bytes (`heapless::Vec<u8, MAX_PAYLOAD>`) — same
//! upper bound as the rest of `magent-core`. The constant is
//! deliberately the same as `crate::MAX_BUFFER_SIZE` for consistency.

use crate::communication::link::{IngressSource, LinkAdapter};
use crate::error::Web3ErrorKind;
use crate::web3::{Identity, SignedMessage};

/// Maximum payload size the gateway will accept in a single frame.
///
/// Mirrors `crate::MAX_BUFFER_SIZE` so the agent's other buffers can
/// accept an ingress envelope without re-allocating.
pub const MAX_PAYLOAD: usize = crate::MAX_BUFFER_SIZE;

/// Maximum number of adapters a single gateway can manage. Eight is a
/// practical upper bound for a single embedded device (one BLE, one
/// MQTT, two UARTs, two SPI chip-selects, one manual, one spare).
pub const MAX_ADAPTERS: usize = 8;

/// How the gateway wraps incoming payloads before handing them to the
/// agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressMode {
    /// Pass raw bytes + source through. No signing.
    Transparent,
    /// Wrap every payload in a [`SignedMessage`] signed by the
    /// configured device identity.
    Signed,
}

/// Result of a single `ingest` round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressFrame {
    /// Where the frame came from. Always populated.
    pub source: IngressSource,
    /// The payload. In `Transparent` mode these are the raw bytes from
    /// the adapter. In `Signed` mode these are the
    /// `SignedMessage.payload` bytes (the envelope itself is reachable
    /// via [`Self::envelope_json`]).
    pub payload: heapless::Vec<u8, MAX_PAYLOAD>,
    /// The full signed envelope as JSON, present only in
    /// `Signed` mode. `None` in `Transparent` mode.
    pub envelope_json: Option<heapless::String<MAX_PAYLOAD>>,
}

/// Errors that can come out of the gateway itself (as opposed to the
/// underlying adapters).
#[derive(Debug)]
pub enum IngressError {
    /// Tried to register more than [`MAX_ADAPTERS`] adapters.
    AdapterPoolFull,
    /// The adapter pool is empty.
    NoAdapters,
    /// Payload exceeded [`MAX_PAYLOAD`].
    PayloadTooLarge {
        /// Actual byte size of the offending payload.
        size: usize,
    },
    /// The signed envelope would not fit [`MAX_PAYLOAD`] even though the raw
    /// payload did. Hex encoding roughly doubles the payload and the signer
    /// DID / signature add a fixed overhead.
    EnvelopeTooLarge {
        /// Raw payload size that was submitted.
        size: usize,
        /// Largest raw payload that fits the envelope buffer.
        max: usize,
    },
    /// The adapter's [`LinkAdapter::send`] reported a failure while we were
    /// trying to reply over that link.
    AdapterSendFailed,
    /// Underlying web3 / signing failure.
    Web3(Web3ErrorKind),
}

impl core::fmt::Display for IngressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AdapterPoolFull => f.write_str("ingress gateway adapter pool full"),
            Self::NoAdapters => f.write_str("ingress gateway has no adapters registered"),
            Self::PayloadTooLarge { size } => {
                write!(f, "ingress payload {size}B exceeds {MAX_PAYLOAD}B cap")
            }
            Self::EnvelopeTooLarge { size, max } => write!(
                f,
                "ingress payload {size}B exceeds the {max}B that fits a signed envelope"
            ),
            Self::AdapterSendFailed => f.write_str("ingress adapter send failed"),
            Self::Web3(e) => write!(f, "ingress web3 error: {e:?}"),
        }
    }
}

// `std::error::Error` is a host-only trait (it relies on `std`'s
// richer error infra like `Backtrace`). On embedded (`no_std`) builds
// the gateway is debugged via `core::fmt::Display` + the variant's
// `Debug` impl, which is enough for `esp_println::println!`-style
// logging. Gating the `std::error::Error` impl on `std` means the same
// `IngressError` type works on both targets without forcing the
// embedded build to pull in anything from `std`.
#[cfg(feature = "std")]
impl std::error::Error for IngressError {}

impl From<Web3ErrorKind> for IngressError {
    fn from(e: Web3ErrorKind) -> Self {
        Self::Web3(e)
    }
}

/// Storage for adapters inside the gateway. We can't keep a
/// `Vec<Box<dyn LinkAdapter<Error = ...>>>` because `Error` is an
/// associated type — so we keep concrete adapter slots and the gateway
/// is generic over the concrete adapter type. In practice the firmware
/// uses a single adapter kind, or wraps heterogeneous adapters behind a
/// sum type / boxed-dyn glue. This trade-off is documented in the
/// module-level comment.
#[derive(Debug)]
pub struct IngressGateway<A: LinkAdapter> {
    adapters: heapless::Vec<A, MAX_ADAPTERS>,
    mode: IngressMode,
    /// Device identity used in `Signed` mode. `None` until
    /// [`Self::set_signer`] is called.
    signer: Option<Identity>,
}

impl<A: LinkAdapter> IngressGateway<A> {
    /// Create an empty gateway in the given mode. Use [`Self::register`]
    /// to add adapters and [`Self::set_signer`] to install the device
    /// identity used for `Signed` mode.
    pub fn new(mode: IngressMode) -> Self {
        Self {
            adapters: heapless::Vec::new(),
            mode,
            signer: None,
        }
    }

    /// Register a new adapter. Returns [`IngressError::AdapterPoolFull`]
    /// if the pool is exhausted.
    pub fn register(&mut self, adapter: A) -> Result<(), IngressError> {
        self.adapters
            .push(adapter)
            .map_err(|_| IngressError::AdapterPoolFull)
    }

    /// Install (or replace) the device identity used to sign payloads in
    /// `Signed` mode. Has no effect in `Transparent` mode but is still
    /// safe to call.
    pub fn set_signer(&mut self, id: Identity) {
        self.signer = Some(id);
    }

    /// Currently configured mode.
    pub fn mode(&self) -> IngressMode {
        self.mode
    }

    /// Switch mode at runtime.
    pub fn set_mode(&mut self, mode: IngressMode) {
        self.mode = mode;
    }

    /// Number of registered adapters.
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    /// Send `data` back out through the adapter at `index`, e.g. to reply to a
    /// received command over the same link (bidirectional UART/MQTT/BLE).
    ///
    /// Returns `Ok(())` if the index is out of range (nothing to send) or the
    /// send succeeded; `Err(IngressError::AdapterSendFailed)` if the adapter's
    /// [`LinkAdapter::send`] failed.
    pub fn send_to_adapter(&mut self, index: usize, data: &[u8]) -> Result<(), IngressError> {
        match self.adapters.get_mut(index) {
            Some(adapter) => adapter
                .send(data)
                .map_err(|_| IngressError::AdapterSendFailed),
            None => Ok(()),
        }
    }

    /// Run one round of polling across every registered adapter and
    /// return the first frame received (FIFO across adapters). Returns
    /// `Ok(None)` if every adapter reported "no data" — this is the
    /// normal idle signal and the caller can loop or yield.
    ///
    /// Adapter errors are logged via [`log::warn!`] and skipped for
    /// this round — they do NOT propagate. Only gateway-level errors
    /// (pool empty, payload too large, signing failure) surface as
    /// `Err`.
    pub fn ingest(&mut self) -> Result<Option<IngressFrame>, IngressError> {
        if self.adapters.is_empty() {
            return Err(IngressError::NoAdapters);
        }
        let mut scratch = [0u8; MAX_PAYLOAD];
        for adapter in self.adapters.iter_mut() {
            if !adapter.is_connected() {
                continue;
            }
            let n = match adapter.poll(&mut scratch) {
                Ok(0) => continue,
                // The `LinkAdapter::poll` contract caps the count at
                // `buf.len()`. A count above that is an adapter bug (e.g. a
                // HAL narrowing a negative C `int` into `usize::MAX`), and
                // trusting it would panic on the slice below — on the ESP32
                // that is the Core-1 real-time ingress thread. Drop the
                // frame instead of letting the violation take the core down.
                Ok(n) if n <= scratch.len() => n,
                Ok(n) => {
                    log::warn!(
                        "ingress adapter {:?} reported {n}B into a {}B buffer — discarding frame",
                        adapter.source_kind(),
                        scratch.len()
                    );
                    continue;
                }
                Err(e) => {
                    log::warn!(
                        "ingress adapter {:?} poll error: {:?}",
                        adapter.source_kind(),
                        e
                    );
                    continue;
                }
            };
            let bytes = &scratch[..n];
            let source = adapter.source_kind();
            return Ok(Some(self.build_frame(source, bytes)?));
        }
        Ok(None)
    }

    fn build_frame(
        &self,
        source: IngressSource,
        bytes: &[u8],
    ) -> Result<IngressFrame, IngressError> {
        let mut payload: heapless::Vec<u8, MAX_PAYLOAD> = heapless::Vec::new();
        payload
            .extend_from_slice(bytes)
            .map_err(|_| IngressError::PayloadTooLarge { size: bytes.len() })?;

        match self.mode {
            IngressMode::Transparent => Ok(IngressFrame {
                source,
                payload,
                envelope_json: None,
            }),
            IngressMode::Signed => {
                let signer = self.signer.as_ref().ok_or({
                    IngressError::Web3(Web3ErrorKind::InvalidSignature { actual_len: 0 })
                })?;
                let signed: SignedMessage = signer.sign(bytes).map_err(IngressError::Web3)?;
                // The envelope is `{"signer":"<did>","payload_hex":"<2n>",
                // "signature_hex":"<128>"}`: 177 fixed bytes plus the DID
                // and twice the payload. Reject a raw payload that cannot fit
                // *before* serialising; the old code accepted up to
                // MAX_PAYLOAD and then failed with a misleading
                // `PayloadTooLarge { size }` (claiming the size it just
                // accepted) while dropping the frame.
                const ENVELOPE_FIXED_OVERHEAD: usize = 177;
                let max_raw = MAX_PAYLOAD
                    .saturating_sub(ENVELOPE_FIXED_OVERHEAD + signed.signer.len())
                    / 2;
                if bytes.len() > max_raw {
                    return Err(IngressError::EnvelopeTooLarge {
                        size: bytes.len(),
                        max: max_raw,
                    });
                }
                // PATCHED (MicroAgent): serialise into a bounded stack buffer
                // via `to_json_into` instead of the per-frame heap allocation
                // of `to_json()` (see `communication/mod.rs` TODO).
                let mut json_buf: heapless::String<MAX_PAYLOAD> = heapless::String::new();
                signed
                    .to_json_into(&mut json_buf)
                    .map_err(|_| IngressError::PayloadTooLarge { size: bytes.len() })?;
                Ok(IngressFrame {
                    source,
                    payload,
                    envelope_json: Some(json_buf),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::communication::link::{IngressSource, LinkAdapter};

    /// Minimal adapter for unit tests — always disconnected and empty.
    #[derive(Debug)]
    struct NullAdapter;
    impl LinkAdapter for NullAdapter {
        type Error = ();
        fn poll(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
            Ok(0)
        }
        fn send(&mut self, _buf: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn source_kind(&self) -> IngressSource {
            IngressSource::Other {
                label: heapless::String::try_from("null").unwrap(),
            }
        }
        fn is_connected(&self) -> bool {
            false
        }
    }

    #[test]
    fn display_adapter_pool_full() {
        let e = IngressError::AdapterPoolFull;
        assert_eq!(format!("{e}"), "ingress gateway adapter pool full");
    }

    #[test]
    fn display_no_adapters() {
        let e = IngressError::NoAdapters;
        assert_eq!(format!("{e}"), "ingress gateway has no adapters registered");
    }

    /// A raw payload that fits MAX_PAYLOAD can still overflow the signed
    /// envelope (hex doubles it); it must be rejected up front with a
    /// truthful error instead of failing during serialisation.
    #[test]
    fn signed_envelope_rejects_a_payload_that_cannot_fit() {
        let mut gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Signed);
        gw.set_signer(Identity::from_secret_bytes(&[7u8; 32]).unwrap());
        let source = IngressSource::Other {
            label: heapless::String::try_from("test").unwrap(),
        };

        let big = alloc::vec![b'x'; 1_200];
        assert!(matches!(
            gw.build_frame(source.clone(), &big),
            Err(IngressError::EnvelopeTooLarge { .. })
        ));

        let small = alloc::vec![b'x'; 300];
        let frame = gw.build_frame(source, &small).expect("small frame fits");
        assert!(frame.envelope_json.is_some());
    }

    #[test]
    fn display_payload_too_large() {
        let e = IngressError::PayloadTooLarge { size: 9999 };
        let msg = format!("{e}");
        assert!(msg.contains("9999"), "Display must include the byte size: {msg}");
        assert!(msg.contains(&format!("{MAX_PAYLOAD}")), "Display must include the cap");
    }

    #[test]
    fn display_adapter_send_failed() {
        let e = IngressError::AdapterSendFailed;
        assert_eq!(format!("{e}"), "ingress adapter send failed");
    }

    #[test]
    fn mode_accessor_returns_configured_mode() {
        let gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Transparent);
        assert_eq!(gw.mode(), IngressMode::Transparent);

        let gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Signed);
        assert_eq!(gw.mode(), IngressMode::Signed);
    }

    #[test]
    fn new_gateway_is_empty() {
        let gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Transparent);
        assert_eq!(gw.adapter_count(), 0);
    }

    #[test]
    fn set_mode_updates_mode() {
        let mut gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Transparent);
        assert_eq!(gw.mode(), IngressMode::Transparent);
        gw.set_mode(IngressMode::Signed);
        assert_eq!(gw.mode(), IngressMode::Signed);
        gw.set_mode(IngressMode::Transparent);
        assert_eq!(gw.mode(), IngressMode::Transparent);
    }

    #[test]
    fn register_increases_count() {
        let mut gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Transparent);
        gw.register(NullAdapter).unwrap();
        assert_eq!(gw.adapter_count(), 1);
        gw.register(NullAdapter).unwrap();
        assert_eq!(gw.adapter_count(), 2);
    }

    #[cfg(feature = "std")]
    #[test]
    fn set_signer_does_not_crash_in_transparent_mode() {
        let id = crate::web3::Identity::generate().unwrap();
        let mut gw: IngressGateway<NullAdapter> = IngressGateway::new(IngressMode::Transparent);
        gw.set_signer(id);
        // Transparent mode ignores the signer; no-op is safe.
        assert_eq!(gw.mode(), IngressMode::Transparent);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn from_web3_error_preserves_kind() {
        // IngressError::Web3 round-trips a Web3ErrorKind.
        let kind = crate::error::Web3ErrorKind::InvalidSignature { actual_len: 0 };
        let err: IngressError = kind.into();
        match err {
            IngressError::Web3(_) => {} // expected
            other => panic!("expected Web3 variant, got {other:?}"),
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn ingress_error_implements_std_error() {
        // This validates that IngressError: std::error::Error, which is
        // only compiled when `std` is active.
        fn _assert_std_error<E: std::error::Error>() {}
        _assert_std_error::<IngressError>();
    }
}
