//! Detached Ed25519 signatures + signed-message envelopes.
//!
//! A [`Signature`] is just 64 bytes of raw Ed25519 signature
//! (RFC 8032). It can be transported as raw bytes, hex, or
//! base64; the `to_hex` / `from_hex` accessors are the canonical
//! ones because they match what every other toolchain uses
//! (Python, JS, Go).
//!
//! A [`SignedMessage`] bundles the signer's [`DidKey`], the
//! payload bytes, and the [`Signature`]. The payload is
//! **opaque** — callers decide whether it's UTF-8 text, JSON, a
//! CID, or anything else. The envelope is what gets shipped over
//! the wire; the payload stays in `Vec<u8>` so binary-safe
//! transports (mailbox attachments, audit-log binary records)
//! don't have to round-trip through `String`.
//!
//! ## JSON wire format
//!
//! When the agent serialises a [`SignedMessage`] as JSON (via
//! [`SignedMessage::to_json`] / [`SignedMessage::from_json`]),
//! the payload is hex-encoded. This is the same convention
//! used by `did:key`-style envelopes in the IPFS / libp2p
//! world; hex is the lowest-common-denominator encoder that
//! every language can decode without pulling in a base64 crate.
//!
//! ```json
//! {
//!   "signer": "did:key:z6Mk…",
//!   "payload_hex": "68656c6c6f",
//!   "signature_hex": "9a1f…"
//! }
//! ```

use core::fmt;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;

use super::did::DidKey;
use super::error::{hex_err, invalid_sig, parse_err};
use crate::error::ParseFailureKind;

/// Number of bytes in an Ed25519 signature (two curve points: `R`
/// + `S`, each 32 bytes).
pub const SIGNATURE_LEN: usize = 64;

/// A raw 64-byte Ed25519 signature.
///
/// Construct via [`Signature::from_bytes`] (raw) or
/// [`Signature::from_hex`] (hex-encoded). Serialise back with
/// [`Signature::to_bytes`] or [`Signature::to_hex`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Wrap a raw 64-byte signature. The input is copied into an
    /// internal array so the caller doesn't have to keep the
    /// slice alive.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Web3ErrorKind> {
        if bytes.len() != SIGNATURE_LEN {
            return Err(invalid_sig(bytes.len()));
        }
        let mut out = [0u8; SIGNATURE_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Parse a hex-encoded signature. Accepts both upper- and
    /// lower-case hex, with or without a leading `0x`.
    ///
    /// The returned `Web3ErrorKind::InvalidSignature { actual_len }`
    /// field carries the **byte length** (not the hex-character
    /// length) of the decoded payload, so a 130-character hex
    /// string (which would decode to 65 bytes, not the expected
    /// 64) reports `actual_len: 65` rather than `65` confusingly
    /// mixed with a length in characters.
    pub fn from_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        let bytes = hex_decode(s)?;
        // hex_decode guarantees the input was an even number of
        // hex digits, so `bytes.len()` is the byte count.
        if bytes.len() != SIGNATURE_LEN {
            return Err(invalid_sig(bytes.len()));
        }
        let mut out = [0u8; SIGNATURE_LEN];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }

    /// Borrow the raw signature bytes.
    pub fn to_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }

    /// Lower-case hex of the signature bytes. Always 128 chars,
    /// no `0x` prefix.
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only print the first 8 hex chars so debug logs don't
        // dump 128 bytes of signature material.
        let hex = self.to_hex();
        write!(f, "Signature({}…)", &hex[..8])
    }
}

/// A signed payload, ready to be shipped over the wire.
///
/// `signer` is the public `did:key` of the identity that signed
/// the `payload`. `signature` is the raw Ed25519 signature over
/// `payload`. Verification is **not** automatic — call
/// [`crate::web3::Identity::verify`] (or
/// [`crate::web3::identity::verify_signature`]) with the expected
/// payload to check that the signature is valid for `signer`.
///
/// ## Field visibility — `payload` is private, not `pub`
///
/// The raw payload bytes are stored internally and exposed via
/// [`SignedMessage::payload_bytes`]. We deliberately do **not**
/// make `payload` a public field because callers could otherwise
/// desynchronise it from the canonical [`SignedMessage::payload_hex`]
/// representation (the one that goes over the wire) by direct
/// field assignment, and verification — which uses the bytes the
/// caller passes in, **not** the embedded `payload` — would then
/// silently accept an envelope whose `payload_hex` does not match
/// the bytes actually signed. The test
/// `signed_message_invariant_payload_matches_payload_hex` pins
/// this property.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedMessage {
    /// `did:key` of the signer. Public so it can be deserialised
    /// directly into a `SignedMessage` from JSON (serde requires
    /// public fields) and so callers can read it without a
    /// getter for the common "who signed this?" check.
    pub signer: String,
    /// Raw payload bytes, set by [`SignedMessage::new`] (from the
    /// caller) or [`SignedMessage::from_json`] (re-derived from
    /// `payload_hex`). **Private** — read via
    /// [`SignedMessage::payload_bytes`], write only via the
    /// constructor / deserialiser.
    #[serde(skip)]
    payload: Vec<u8>,
    /// Hex-encoded payload, the canonical wire form. Mirrors
    /// `payload` so the JSON form is self-contained.
    #[serde(rename = "payload_hex")]
    pub payload_hex: String,
    /// Hex-encoded signature.
    #[serde(rename = "signature_hex")]
    pub signature_hex: String,
}

impl SignedMessage {
    /// Build a [`SignedMessage`] from raw parts. The payload and
    /// signature are stored verbatim; `payload_hex` is
    /// re-derived from `payload` on every call so they cannot
    /// drift out of sync.
    pub fn new(signer: DidKey, payload: Vec<u8>, signature: Signature) -> Self {
        let payload_hex = hex_encode(&payload);
        Self {
            signer: signer.to_string(),
            payload,
            payload_hex,
            signature_hex: signature.to_hex(),
        }
    }

    /// Borrow the raw payload bytes that were signed. The
    /// returned slice equals what the signature was computed
    /// over — `verify_signature(_, &signed.signature_hex,
    /// signed.payload_bytes())` is always the canonical
    /// verification path.
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload
    }

    /// Decode the embedded signature back into a [`Signature`].
    /// Useful when verifying a [`SignedMessage`] received over
    /// the wire: the wire form only carries the hex form, so the
    /// caller decodes on demand rather than carrying the raw
    /// bytes around.
    pub fn signature(&self) -> Result<Signature, Web3ErrorKind> {
        Signature::from_hex(&self.signature_hex)
    }

    /// Decode the embedded `did:key` back into a [`DidKey`] so
    /// the caller can extract the public key for verification.
    pub fn signer_did(&self) -> Result<DidKey, Web3ErrorKind> {
        DidKey::from_string(&self.signer)
    }

    /// Serialise to JSON. Uses the canonical serde
    /// representation. Note: `payload` (the raw `Vec<u8>`) is
    /// NOT serialised — only `payload_hex` and `signature_hex`
    /// are emitted, so the round-trip through JSON is lossless.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SignedMessage is always serialisable")
    }

    /// Serialise to JSON into a caller-provided bounded buffer, avoiding the
    /// per-frame heap allocation that [`SignedMessage::to_json`] makes. Used
    /// by the ingress gateway's hot path.
    ///
    /// The output is byte-for-byte the same canonical serde form
    /// (`{"signer":...,"payload_hex":...,"signature_hex":...}`), including
    /// string escaping, so the result round-trips through
    /// [`SignedMessage::from_json`] exactly like `to_json()`'s output.
    ///
    /// Returns `Err(())` if `out` is too small (no partial write is left
    /// behind — the buffer is cleared first).
    #[allow(clippy::result_unit_err)] // `()` is an intentional marker: the caller maps it to its own error code.
    pub fn to_json_into<const N: usize>(
        &self,
        out: &mut heapless::String<N>,
    ) -> Result<(), ()> {
        use core::fmt::Write as _;
        out.clear();
        out.push_str("{\"signer\":\"").map_err(|_| ())?;
        for c in self.signer.chars() {
            match c {
                '"' => out.push_str("\\\"").map_err(|_| ())?,
                '\\' => out.push_str("\\\\").map_err(|_| ())?,
                '\n' => out.push_str("\\n").map_err(|_| ())?,
                '\r' => out.push_str("\\r").map_err(|_| ())?,
                '\t' => out.push_str("\\t").map_err(|_| ())?,
                ctrl if (ctrl as u32) < 0x20 => {
                    // serde_json escapes other control chars as \u00XX.
                    let _ = write!(out, "\\u{:04x}", ctrl as u32);
                }
                other => out.push(other).map_err(|_| ())?,
            }
        }
        out.push_str("\",\"payload_hex\":\"").map_err(|_| ())?;
        out.push_str(&self.payload_hex).map_err(|_| ())?;
        out.push_str("\",\"signature_hex\":\"").map_err(|_| ())?;
        out.push_str(&self.signature_hex).map_err(|_| ())?;
        out.push_str("\"}").map_err(|_| ())?;
        Ok(())
    }

    /// Parse the JSON form back into a [`SignedMessage`].
    /// `payload` is re-derived from `payload_hex` so the caller
    /// doesn't have to manually decode.
    ///
    /// Errors are categorised:
    ///
    /// * `Web3ErrorKind::Parse { kind: InvalidJson }` — the input
    ///   wasn't valid JSON at all (serde_json rejected it).
    /// * `Web3ErrorKind::Parse { kind: SchemaMismatch }` — the
    ///   input parsed as JSON but didn't match the `SignedMessage`
    ///   schema (missing field, wrong type, …).
    /// * `Web3ErrorKind::HexDecode(_)` — the JSON parsed but
    ///   `payload_hex` contained a non-hex digit or had the
    ///   wrong length.
    pub fn from_json(s: &str) -> Result<Self, Web3ErrorKind> {
        // We can't distinguish "not JSON" from "JSON but wrong
        // shape" purely from serde_json's error category without
        // inspecting the message, but both are `Parse` failures
        // from the caller's perspective. Use `SchemaMismatch` as
        // the default and tag `InvalidJson` when the error
        // explicitly says "expected value" / "EOF" / "invalid
        // number" — i.e. the input wasn't even a JSON document.
        let mut msg: Self = serde_json::from_str(s).map_err(|e| {
            let kind = if e.is_syntax() || e.is_eof() || e.is_io() {
                ParseFailureKind::InvalidJson
            } else {
                ParseFailureKind::SchemaMismatch
            };
            parse_err(kind, format!("JSON parse failed: {}", e))
        })?;
        let payload = hex_decode(&msg.payload_hex)?;
        msg.payload = payload;
        Ok(msg)
    }
}

impl fmt::Debug for SignedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hand-rolled Debug to avoid leaking the payload length
        // and signed bytes through the auto-derived impl. The
        // signer DID and the two hex strings are sufficient for
        // most diagnostic purposes; for the raw payload the
        // caller must explicitly call [`SignedMessage::payload_bytes`].
        f.debug_struct("SignedMessage")
            .field("signer", &self.signer)
            .field("payload_hex", &self.payload_hex)
            .field("signature_hex", &self.signature_hex)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------
// We hand-roll hex encode/decode here instead of pulling in the
// `hex` crate. The crate has many transitive dependencies
// (`serde`, `arrayvec`, …) that we don't otherwise need, and the
// encoder is ~20 lines. If we ever need a streaming encoder, we
// can swap this out for `const-hex` without changing the public
// API.

/// Lower-case hex encode. Returns a `String` of length `2 * bytes.len()`.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode a hex string (with or without `0x` prefix, any case)
/// into bytes. Returns `Web3ErrorKind::HexDecode` on bad input.
pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(hex_err("odd number of hex digits"));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, Web3ErrorKind> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(hex_err(format!("invalid hex digit: '{}'", c as char))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let bytes = [0u8, 1, 2, 254, 255];
        let enc = hex_encode(&bytes);
        assert_eq!(enc, "000102feff");
        let dec = hex_decode(&enc).unwrap();
        assert_eq!(dec, bytes);
    }

    #[test]
    fn hex_decode_strips_0x_prefix() {
        assert_eq!(hex_decode("0xdeadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn hex_decode_rejects_bad_chars() {
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn signature_rejects_wrong_length() {
        assert!(matches!(
            Signature::from_bytes(&[0; 32]),
            Err(Web3ErrorKind::InvalidSignature { actual_len: 32 })
        ));
    }

    #[test]
    fn signature_hex_round_trip() {
        let bytes = [42u8; SIGNATURE_LEN];
        let sig = Signature::from_bytes(&bytes).unwrap();
        let hex = sig.to_hex();
        assert_eq!(hex.len(), SIGNATURE_LEN * 2);
        let parsed = Signature::from_hex(&hex).unwrap();
        assert_eq!(parsed.to_bytes(), &bytes);
    }

    #[test]
    fn signature_hex_accepts_uppercase_and_prefix() {
        let bytes = [7u8; SIGNATURE_LEN];
        let sig = Signature::from_bytes(&bytes).unwrap();
        let hex = sig.to_hex().to_uppercase();
        let parsed = Signature::from_hex(&format!("0x{}", hex)).unwrap();
        assert_eq!(parsed.to_bytes(), &bytes);
    }

    #[test]
    fn signed_message_json_round_trip() {
        // We can't easily construct a real DidKey without
        // hitting the rest of the crate, so build a minimal one
        // with the secret-key multicodec prefix (the encoder
        // doesn't care which prefix is in use).
        let sk_bytes = [1u8; 32];
        let did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        let sig_bytes = [2u8; SIGNATURE_LEN];
        let sig = Signature::from_bytes(&sig_bytes).unwrap();
        let payload = b"hello".to_vec();
        let msg = SignedMessage::new(did, payload.clone(), sig);
        let json = msg.to_json();
        let parsed = SignedMessage::from_json(&json).unwrap();
        // Use the public accessor rather than reading the
        // private field directly — this also tests that the
        // accessor surfaces what the JSON deserialiser wrote.
        assert_eq!(parsed.payload_bytes(), payload.as_slice());
        assert_eq!(parsed.signature_hex, sig.to_hex());
        assert_eq!(parsed.payload_hex, hex_encode(&payload));
    }

    /// Pin the invariant that `payload_hex` always equals
    /// `hex_encode(payload_bytes)`. A caller who somehow
    /// desynchronised the two (e.g. by JSON-tampering one without
    /// the other) would produce an envelope that *parses* but
    /// verifies differently depending on which form you check —
    /// a foot-gun we don't want.
    #[test]
    fn signed_message_invariant_payload_matches_payload_hex() {
        let sk_bytes = [3u8; 32];
        let did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        let sig = Signature::from_bytes(&[0u8; SIGNATURE_LEN]).unwrap();
        let payload = vec![0xAAu8, 0xBB, 0xCC, 0xDD];
        let msg = SignedMessage::new(did, payload.clone(), sig);
        assert_eq!(msg.payload_hex, hex_encode(&payload));
        assert_eq!(msg.payload_bytes(), payload.as_slice());

        // Same invariant must hold after a JSON round-trip.
        let json = msg.to_json();
        let parsed = SignedMessage::from_json(&json).unwrap();
        assert_eq!(parsed.payload_hex, hex_encode(parsed.payload_bytes()));
    }

    /// The Debug impl must not include the payload bytes. The
    /// existing integration test
    /// (`signed_message_debug_does_not_leak_payload`) covers the
    /// whole envelope; this unit test pins the same property for
    /// the type's own `{:?}` output (the other test uses the
    /// crate-public path through `magent_core::web3`).
    #[test]
    fn signed_message_debug_redacts_payload() {
        let sk_bytes = [4u8; 32];
        let did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        let sig = Signature::from_bytes(&[0u8; SIGNATURE_LEN]).unwrap();
        let payload = b"secret-message-contents".to_vec();
        let msg = SignedMessage::new(did, payload, sig);
        let dbg = format!("{:?}", msg);
        assert!(
            !dbg.contains("secret-message-contents"),
            "Debug leaked the payload bytes: {dbg}"
        );
        // The signer DID and both hex strings should be present.
        assert!(dbg.contains("signer"));
        assert!(dbg.contains("payload_hex"));
        assert!(dbg.contains("signature_hex"));
    }
}