//! Generic signed envelope machinery for `magent_core::web3_app`.
//!
//! ## Why this module exists
//!
//! The `web3_app` module started life with a single payload type:
//! [`crate::web3_app::SignedRunReport`]. Every concrete envelope
//! shares the same shape:
//!
//! ```json
//! {
//!   "payload_type": "magent/<kind>:vN",
//!   "issued_at_unix": 1700000000,
//!   "not_before_unix": null,
//!   "not_after_unix": 1700000900,
//!   "payload": { ...kind-specific fields... },
//!   "signer": "did:key:z6Mk...",
//!   "signature_hex": "9a1f..."
//! }
//! ```
//!
//! The only things that differ between payload types are:
//!
//! 1. The `payload_type` string (domain separation).
//! 2. The canonical-bytes prefix (defence-in-depth against an
//!    attacker who re-serialises the JSON correctly under a
//!    different prefix).
//! 3. The payload's own length / structure validation.
//!
//! Everything else — sign / verify / JSON serialise / clock-window
//! check — is identical. Lifting it into a generic [`Envelope<P>`]
//! means a new payload type (e.g. `SignedPrompt`,
//! `SignedAuditEntry`) is a 5-line type shell plus an
//! [`EnvelopePayload`] impl, instead of a 200-line copy of the
//! `SignedRunReport` machinery.
//!
//! ## Adding a new payload type
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! pub struct MyPayload { /* fields */ }
//!
//! impl EnvelopePayload for MyPayload {
//!     const PAYLOAD_TYPE: &'static str = "magent/my_payload:v1";
//!     const DOMAIN_PREFIX: &'static str = "MAGENT_MP_V1\n";
//!     fn validate(&self) -> Result<(), Web3ErrorKind> { /* … */ }
//! }
//!
//! pub type SignedMyPayload = Envelope<MyPayload>;
//! ```
//!
//! And then `sign_my_payload(identity, …, payload)`, `verify`, etc.
//! are available on `Envelope<MyPayload>` for free, plus from /
//! to JSON and the convenience helpers.
//!
//! ## Cargo feature
//!
//! Gated on the same `web3_app` feature as the rest of the
//! module. Embedded (`no_std`) builds do not pull this in.

use std::string::{String, ToString};
use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::{ParseFailureKind, Web3ErrorKind};
use crate::web3 as core_web3;

// ============================================================================
// EnvelopePayload trait
// ============================================================================

/// The contract every payload type must satisfy to ride in an
/// [`Envelope`].
///
/// ## Why a trait and not a struct with a `Tag` enum?
///
/// An enum-driven dispatch would force every payload type to live
/// in this module, which would tie [`magent_core`] to the CLI
/// crate's `PromptRecord` (and to whatever future records appear).
/// A trait lets each payload type live wherever it makes sense
/// (today: `RunReportFields` in `web3_app::mod.rs`; future:
/// `PromptRecordFields` in `web3_app::prompt.rs`).
///
/// ## Why `Serialize + Deserialize` and not just `Serialize`?
///
/// Verifiers must be able to round-trip the envelope through
/// JSON, and serde's derive macros only work when the payload
/// type is `Deserialize<'de>`. Pinning the bound at the trait
/// level means a future maintainer can't accidentally skip
/// `Deserialize` and break verify-on-the-wire.
///
/// ## Why `validate()`?
///
/// Length caps and per-field invariants belong to the payload
/// type, not the envelope. The envelope enforces *cross-cutting*
/// invariants (domain separation, clock window, signature); the
/// payload enforces *self-contained* invariants (a too-long
/// `answer`, a malformed URL, …). Layering the two means a
/// future payload type that's strict about its own contents
/// doesn't need to touch the envelope machinery.
pub trait EnvelopePayload: Serialize + for<'de> Deserialize<'de> + Sized {
    /// `magent/<kind>:vN` discriminant. Verifiers compare this
    /// field byte-for-byte. Bump the `:vN` suffix when the
    /// canonical encoding changes in an incompatible way.
    const PAYLOAD_TYPE: &'static str;

    /// Domain-separation prefix prepended to the canonical-bytes
    /// form. MUST be unique across every payload type this crate
    /// emits (e.g. `MAGENT_SRR_V1\n` for run reports,
    /// `MAGENT_PR_V1\n` for prompts). See [`Envelope::canonical_bytes`]
    /// for the rationale.
    const DOMAIN_PREFIX: &'static str;

    /// Self-validation. Called by [`Envelope::sign`] (so a
    /// too-long field doesn't produce an envelope that the verifier
    /// will silently reject) and by [`Envelope::verify`] (so a
    /// future relaxation of [`EnvelopePayload::validate`] doesn't
    /// silently accept pre-relaxation envelopes).
    fn validate(&self) -> Result<(), Web3ErrorKind>;
}

// ============================================================================
// Envelope<P>
// ============================================================================

/// A signed envelope for any [`EnvelopePayload`].
///
/// The seven fields are the cross-cutting envelope metadata; the
/// actual record (run report, prompt, audit entry, …) lives in
/// `payload`. Every field is `pub` so serde can round-trip the
/// envelope without a custom `Deserialize` impl; tampering with
/// any field after signing invalidates the signature because the
/// verifier rebuilds the canonical bytes from the received
/// fields, so the public-ness doesn't add a real attack surface.
///
/// The serde `#[serde(bound)]` attributes are load-bearing —
/// the default `Deserialize` bound would be `P: Deserialize<'de>`,
/// which is what we want at the trait level (we pin it via
/// [`EnvelopePayload`]) but the derive macro can't see that. We
/// point it at the trait so the impl satisfies the macro's
/// inference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(bound = "P: EnvelopePayload")]
pub struct Envelope<P: EnvelopePayload> {
    /// MUST equal [`EnvelopePayload::PAYLOAD_TYPE`]. Verifiers
    /// reject on mismatch.
    pub payload_type: String,
    /// Unix seconds when the signer signed.
    pub issued_at_unix: u64,
    /// Optional start of the validity window (inclusive). `None`
    /// means "valid from the moment of issuance".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before_unix: Option<u64>,
    /// Optional end of the validity window (inclusive). `None`
    /// means "valid forever".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after_unix: Option<u64>,
    /// The typed payload.
    pub payload: P,
    /// DID of the signing identity.
    pub signer: String,
    /// Hex signature. 128 hex chars (64 raw bytes of Ed25519-R).
    pub signature_hex: String,
}

impl<P: EnvelopePayload> Envelope<P> {
    /// Sign `payload` with `identity`. The envelope wrapper
    /// fields default to the values supplied here; pass `None` for
    /// both `not_before_unix` and `not_after_unix` to produce an
    /// open-ended (no expiry) envelope.
    ///
    /// Errors:
    ///
    /// * [`Web3ErrorKind::SignatureVerificationFailed`] — the
    ///   underlying `Identity::sign` failed (it can only fail on
    ///   RNG, which shouldn't happen on supported platforms; we
    ///   map the error for consistency with [`crate::web3`]).
    /// * `P::validate(…)` errors — propagated from
    ///   [`EnvelopePayload::validate`].
    pub fn sign(
        identity: &core_web3::Identity,
        issued_at_unix: u64,
        not_before_unix: Option<u64>,
        not_after_unix: Option<u64>,
        payload: P,
    ) -> Result<Self, Web3ErrorKind> {
        // Validate the payload first so a too-long field
        // doesn't produce an envelope that's silently rejected
        // by the verifier (which would mean the work was wasted).
        payload.validate()?;

        // Build the canonical bytes — domain-separation prefix
        // + cross-cutting fields + payload, in declaration
        // order. See [`Self::canonical_bytes_for`] for the design.
        let canonical =
            Self::canonical_bytes_for(&payload, issued_at_unix, not_before_unix, not_after_unix)?;

        // Sign the canonical bytes directly. We use
        // `Identity::sign` for the Ed25519-R signing itself,
        // then keep only the hex signature — the resulting
        // envelope is a flattened custom form, not a
        // [`crate::web3::SignedMessage`].
        let scratch = identity.sign(&canonical)?;
        let signature_hex = scratch.signature_hex;

        Ok(Self {
            payload_type: P::PAYLOAD_TYPE.to_string(),
            issued_at_unix,
            not_before_unix,
            not_after_unix,
            payload,
            signer: identity.did_key().as_str(),
            signature_hex,
        })
    }

    /// Verify this envelope. Returns `Ok(())` on success and an
    /// error describing **which** check failed.
    ///
    /// Checks, in order:
    ///
    /// 1. `payload_type == P::PAYLOAD_TYPE` — domain separation.
    /// 2. `P::validate(…)` — per-payload invariants.
    /// 3. Clock window (if `not_before_unix` / `not_after_unix`
    ///    are `Some`) — `now_secs` must be within
    ///    `[not_before_unix, not_after_unix]`.
    /// 4. `signer` decodes to a `did:key` whose embedded public
    ///    key verifies the signature over the canonical bytes
    ///    for the payload.
    pub fn verify(&self, now_secs: u64) -> Result<(), Web3ErrorKind> {
        // (1) Domain separation. We compare against the
        // associated constant byte-for-byte rather than parsing
        // it out of the URL — the entire point is to refuse a
        // wrong-prefix replay.
        if self.payload_type != P::PAYLOAD_TYPE {
            return Err(Web3ErrorKind::InvalidDid {
                raw: self.payload_type.clone(),
            });
        }
        // (2) Per-payload invariants.
        self.payload.validate()?;
        // (3) Clock window. Both bounds are inclusive. An
        // open-ended envelope (both `None`) passes this check
        // by virtue of skipping it.
        if let Some(start) = self.not_before_unix {
            if now_secs < start {
                return Err(Web3ErrorKind::InvalidDid {
                    raw: format!(
                        "envelope not yet valid: now={} < not_before={}",
                        now_secs, start
                    ),
                });
            }
        }
        if let Some(end) = self.not_after_unix {
            if now_secs > end {
                return Err(Web3ErrorKind::InvalidDid {
                    raw: format!("envelope expired: now={} > not_after={}", now_secs, end),
                });
            }
        }
        // (4) Signature verification. We rebuild the canonical
        // bytes from the received fields and pass them — and the
        // signer's public key, extracted from the `did:key` —
        // through `verify_signature_detailed`. Any tampering in
        // *any* of the bound fields changes the canonical bytes
        // and so breaks the signature.
        let canonical = Self::canonical_bytes_for(
            &self.payload,
            self.issued_at_unix,
            self.not_before_unix,
            self.not_after_unix,
        )?;
        let signer_did = core_web3::DidKey::from_string(&self.signer)?;
        let pk_bytes = signer_did.ed25519_public_key()?;
        let pk = core_web3::PublicKey::from_bytes(pk_bytes)?;
        core_web3::verify_signature_detailed(&pk, &self.signature_hex, &canonical)?;
        Ok(())
    }

    /// Serialise to JSON. The output uses canonical serde field
    /// order (declaration order in the struct).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Envelope<P> is always serialisable")
    }

    /// Pretty-print. Same byte-level guarantees as [`Self::to_json`]
    /// modulo whitespace — verifiers MUST use the compact form
    /// (`to_json`) for signature comparison.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("Envelope<P> is always serialisable")
    }

    /// Deserialise from JSON. **Does NOT verify** the signature —
    /// the caller must explicitly invoke [`Self::verify`] on the
    /// returned value. Splitting deserialise / verify keeps the
    /// parsing strict (a malformed payload can be inspected)
    /// without silently accepting a forged envelope.
    pub fn from_json(s: &str) -> Result<Self, Web3ErrorKind> {
        serde_json::from_str(s).map_err(|e| {
            let kind = if e.is_syntax() || e.is_eof() {
                ParseFailureKind::InvalidJson
            } else {
                ParseFailureKind::SchemaMismatch
            };
            Web3ErrorKind::Parse {
                kind,
                message: format!("JSON parse failed: {}", e),
            }
        })
    }

    /// Convenience: parse + verify in one call. Used by the
    /// CLI's `--verify-signed` paths and by audit pipelines.
    pub fn parse_and_verify(json: &str, now_secs: u64) -> Result<Self, Web3ErrorKind> {
        let env = Self::from_json(json)?;
        env.verify(now_secs)?;
        Ok(env)
    }

    /// Recompute the canonical bytes for this envelope (as the
    /// verifier does). Exposed so tests can assert "sign-then-
    /// verify round-trips byte-for-byte".
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        Self::canonical_bytes_for(
            &self.payload,
            self.issued_at_unix,
            self.not_before_unix,
            self.not_after_unix,
        )
    }

    /// Build the canonical byte sequence that the signature
    /// covers. This is the cross-payload form of the function —
    /// both [`Self::sign`] and [`Self::verify`] route through
    /// here so the contract is defined in exactly one place.
    ///
    /// The name has a `_for` suffix to avoid clashing with the
    /// [`Self::canonical_bytes`] instance method (which is a
    /// thin wrapper over this one but starting from an
    /// envelope). Callers that have an envelope should use the
    /// method; callers that have a bare payload (e.g. the
    /// `sign` path) use this associated function.
    ///
    /// ## What "canonical" means here
    ///
    /// We use **JSON serialisation with serde's deterministic
    /// field order** as the canonical form. This has three nice
    /// properties:
    ///
    /// 1. **Stable across machines**: serde emits fields in
    ///    declaration order, which is part of the source code,
    ///    so two binaries built from the same commit produce
    ///    the same byte stream.
    /// 2. **Diff-friendly**: missing fields show up as missing
    ///    lines in `git diff`, not as re-ordered noise.
    /// 3. **Tool-readable**: any tool that already understands
    ///    JSON can verify the envelope by re-serialising and
    ///    signing again.
    ///
    /// ## Domain-separation prefix
    ///
    /// The prefix `P::DOMAIN_PREFIX` is prepended to the JSON.
    /// This is **strictly stronger** than just relying on
    /// `payload_type` in the JSON, because a future attacker
    /// who re-serialises the JSON correctly still can't trick
    /// a verifier into accepting a signature that was actually
    /// produced for a different payload type (e.g. an audit
    /// entry signed under a different prefix). The cost is one
    /// extra constant-prefix allocation per sign / verify.
    pub fn canonical_bytes_for(
        payload: &P,
        issued_at_unix: u64,
        not_before_unix: Option<u64>,
        not_after_unix: Option<u64>,
    ) -> Result<Vec<u8>, Web3ErrorKind> {
        // Field order in this helper object is **load-bearing**:
        // serde emits fields in declaration order, and any
        // reordering would break every existing signature. The
        // test `canonical_bytes_for_<payload>_is_field_order_stable`
        // pins the order; do NOT edit this struct without
        // updating the test.
        //
        // We use a generic shadow struct here rather than
        // borrowing the envelope-shaped fields directly because
        // (a) the canonical form deliberately omits the
        // signature field, and (b) the order of the remaining
        // fields is independent of the envelope's JSON
        // serialisation order — we want the canonical-bytes
        // contract to be self-contained.
        #[derive(Serialize)]
        struct CanonicalForm<'a, T: EnvelopePayload> {
            issued_at_unix: u64,
            not_before_unix: Option<u64>,
            not_after_unix: Option<u64>,
            payload: &'a T,
        }

        let form = CanonicalForm::<P> {
            issued_at_unix,
            not_before_unix,
            not_after_unix,
            payload,
        };
        let mut out = Vec::with_capacity(64 + 256);
        out.extend_from_slice(P::DOMAIN_PREFIX.as_bytes());
        let json = serde_json::to_vec(&form).map_err(|e| Web3ErrorKind::Parse {
            kind: ParseFailureKind::SchemaMismatch,
            message: format!("canonical serialise: {}", e),
        })?;
        out.extend_from_slice(&json);
        Ok(out)
    }
}

// ============================================================================
// Tests
// ============================================================================
//
// Unit tests for the generic envelope contract: sign / verify
// round-trip, tamper detection, expiry, domain separation, JSON
// round-trip. Per-payload tests (e.g. `RunReportFields::validate`
// specifics) live in the payload's own module.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web3::Identity;

    /// A minimal `EnvelopePayload` for the tests. The fields
    /// here are intentionally trivial so the test focuses on the
    /// envelope machinery, not on any particular record type.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct TinyPayload {
        message: String,
    }

    impl EnvelopePayload for TinyPayload {
        const PAYLOAD_TYPE: &'static str = "magent/test:v1";
        const DOMAIN_PREFIX: &'static str = "MAGENT_TEST_V1\n";
        fn validate(&self) -> Result<(), Web3ErrorKind> {
            if self.message.len() > 4096 {
                return Err(Web3ErrorKind::HexDecode("too long".to_string()));
            }
            Ok(())
        }
    }

    type TinyEnvelope = Envelope<TinyPayload>;

    fn sample_envelope() -> (Identity, TinyEnvelope) {
        let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
        let env = TinyEnvelope::sign(
            &id,
            1_700_000_000,
            None,
            None,
            TinyPayload {
                message: "hello".to_string(),
            },
        )
        .unwrap();
        (id, env)
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let (id, env) = sample_envelope();
        assert_eq!(env.payload_type, "magent/test:v1");
        assert_eq!(env.signer, id.did_key().as_str());
        env.verify(1_700_000_100).expect("verify should succeed");
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let (_, mut env) = sample_envelope();
        env.payload.message = "tampered".to_string();
        let err = env.verify(1_700_000_100).unwrap_err();
        assert!(
            matches!(err, Web3ErrorKind::SignatureVerificationFailed),
            "expected SignatureVerificationFailed, got {:?}",
            err
        );
    }

    #[test]
    fn verify_rejects_wrong_payload_type() {
        let (_, mut env) = sample_envelope();
        env.payload_type = "magent/test:v999".to_string();
        let err = env.verify(1_700_000_100).unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
    }

    #[test]
    fn verify_rejects_payload_type_from_other_family() {
        // Cross-family replay: an envelope for TinyPayload
        // whose payload_type field has been changed to look
        // like a different payload type's discriminant. The
        // `Envelope::verify` step (1) catches this because
        // `payload_type != P::PAYLOAD_TYPE`.
        let (_, mut env) = sample_envelope();
        env.payload_type = "magent/run_report:v1".to_string();
        let err = env.verify(1_700_000_100).unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
    }

    #[test]
    fn verify_rejects_pre_window_now() {
        let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
        let env = TinyEnvelope::sign(
            &id,
            1_700_000_000,
            Some(1_700_000_500),
            None,
            TinyPayload {
                message: "later".to_string(),
            },
        )
        .unwrap();
        let err = env.verify(1_700_000_100).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("not yet valid"), "got: {}", msg);
    }

    #[test]
    fn verify_rejects_post_window_now() {
        let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
        let env = TinyEnvelope::sign(
            &id,
            1_700_000_000,
            None,
            Some(1_700_000_500),
            TinyPayload {
                message: "sooner".to_string(),
            },
        )
        .unwrap();
        let err = env.verify(1_700_000_600).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("expired"), "got: {}", msg);
    }

    #[test]
    fn canonical_bytes_carry_the_domain_prefix() {
        let (id, env) = sample_envelope();
        let bytes = env.canonical_bytes().unwrap();
        assert!(
            bytes.starts_with(b"MAGENT_TEST_V1\n"),
            "missing domain-separation prefix: {:?}",
            &bytes[..32]
        );
        // Sanity: the prefix MUST be the per-payload one,
        // not the run-report one — otherwise two payload types
        // would share a canonical-bytes domain and a
        // cross-family replay would fail closed.
        assert!(!bytes.starts_with(b"MAGENT_SRR_V1\n"));
        // Prevent the unused `id` warning when the test is
        // compiled in isolation.
        let _ = id;
    }

    #[test]
    fn json_round_trip() {
        let (_, env) = sample_envelope();
        let json = env.to_json();
        let parsed = TinyEnvelope::from_json(&json).unwrap();
        assert_eq!(parsed, env);
        parsed.verify(1_700_000_100).unwrap();
    }

    #[test]
    fn parse_and_verify_convenience() {
        let (_, env) = sample_envelope();
        let json = env.to_json();
        let parsed = TinyEnvelope::parse_and_verify(&json, 1_700_000_100).unwrap();
        assert_eq!(parsed, env);
    }

    #[test]
    fn sign_validates_payload_first() {
        // A too-long payload (`validate` returns `Err`) must
        // be caught at `sign` time so the caller doesn't get
        // a "wasted" signature that the verifier will reject.
        let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
        let bad = TinyPayload {
            message: "x".repeat(4097),
        };
        let err = TinyEnvelope::sign(&id, 1, None, None, bad).unwrap_err();
        assert!(matches!(err, Web3ErrorKind::HexDecode(_)));
    }

    #[test]
    fn signing_is_deterministic() {
        // Same payload + same key + same envelope metadata
        // produce the same signature on every call (Ed25519-R
        // is deterministic).
        let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
        let env1 = TinyEnvelope::sign(
            &id,
            1,
            None,
            None,
            TinyPayload {
                message: "deterministic".to_string(),
            },
        )
        .unwrap();
        let env2 = TinyEnvelope::sign(
            &id,
            1,
            None,
            None,
            TinyPayload {
                message: "deterministic".to_string(),
            },
        )
        .unwrap();
        assert_eq!(env1, env2);
        assert_eq!(env1.signature_hex, env2.signature_hex);
    }
}
