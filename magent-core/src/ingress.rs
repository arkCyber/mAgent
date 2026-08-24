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
                Ok(n) => n,
                Err(e) => {
                    log::warn!("ingress adapter {:?} poll error: {:?}", adapter.source_kind(), e);
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