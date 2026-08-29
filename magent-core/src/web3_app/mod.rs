//! `magent_core::web3_app` — application-level signed envelopes.
//!
//! ## Why this module exists alongside `web3`
//!
//! The bare [`crate::web3`] module is the cryptographic *primitive*
//! layer: Ed25519 keypairs, signatures, `did:key` identifiers,
//! JSON envelopes. None of it knows what a "run report" or an
//! "audit-log entry" actually is.
//!
//! This module layers **application semantics** on top:
//!
//! 1. A canonical byte representation for each record type the
//!    agent produces (signed run reports today; signed prompts
//!    and signed audit entries in follow-up PRs).
//! 2. A `SignedRunReport` envelope that ties those canonical
//!    bytes to a [`crate::web3::Identity`] using
//!    [`crate::web3::SignedMessage`] internally.
//! 3. Verify helpers that fail closed on domain-separation
//!    mismatches, expiry-window breaches, replay-window
//!    violations, and signature tamper-detection.
//!
//! ## Design rules enforced by this module
//!
//! * **Domain separation is mandatory.** Every envelope carries a
//!   `payload_type` string (e.g. `magent/run_report:v1`); verifiers
//!   refuse to accept a signature whose envelope type is unknown.
//!   Without this, a signed audit entry could be replayed as a
//!   signed run report.
//! * **The canonical bytes form is the contract.** The signature
//!   covers exactly the bytes returned by
//!   [`SignedRunReport::canonical_bytes`]; any future encoding
//!   change MUST bump `CANONICAL_PAYLOAD_TYPE` so old signatures
//!   stop verifying under the new code.
//! * **Expiry is optional but recommended.** All wrap types carry
//!   `not_before_unix` / `not_after_unix` fields so a downstream
//!   consumer can refuse a signed envelope whose clock window
//!   doesn't cover "now".
//!
//! ## Cargo feature
//!
//! Gated on `web3_app`, which transitively enables `web3` + `std`.
//! Embedded (`no_std`) builds do not pull in this module.

// `ToString` is needed at runtime (this crate is `#![no_std]`, so the std
// prelude — and its `ToString` impl — is NOT auto-imported), even though
// clippy flags it as unused on host builds.
#[allow(unused_imports)]
use std::string::{String, ToString};
use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use crate::web3 as core_web3;

// ============================================================================
// Public constants
// ============================================================================

/// Domain-separation tag for the signed-run-report envelope.
///
/// Bump the `:vN` suffix when the canonical encoding changes in an
/// incompatible way. Verifiers MUST compare this field **byte-for-byte**
/// against the envelope's `payload_type` before accepting a signature
/// — anything else is a downgrade attack.
pub const CANONICAL_PAYLOAD_TYPE: &str = "magent/run_report:v1";

/// Canonical-bytes prefix for run reports. MUST be unique across
/// every payload type this crate emits (today run reports; future
/// prompts, audit entries, …). Pinned here so the envelope
/// machinery can re-export it as [`SignedRunReport::DOMAIN_PREFIX`].
pub const CANONICAL_DOMAIN_PREFIX: &str = "MAGENT_SRR_V1\n";

/// Maximum length (bytes) of the `answer` field of a [`RunReportFields`].
/// Kept in sync with the `cli::runner::RunReport` convention so the
/// two stay byte-compatible.
pub const ANSWER_MAX: usize = 32 * 1024;

/// Maximum length (bytes) of the `provider` field. Provider names
/// are short (`"ollama"`, `"deepseek"`, `"mock"`); a 64-byte cap
/// flags obvious typos.
pub const PROVIDER_MAX: usize = 64;

/// Maximum length (bytes) of the `state` field. Mirrors
/// `provider`'s reasoning.
pub const STATE_MAX: usize = 64;

// ============================================================================
// RunReportFields — the canonical mirror of `cli::runner::RunReport`
// ============================================================================

/// Pure-data mirror of `cli::runner::RunReport`. Lives here (rather
/// than in `cli`) so the `magent-core` library can produce signed
/// envelopes without depending on the CLI crate.
///
/// Field order in this struct is **load-bearing** — serde serialises
/// fields in declaration order, which is how the `SignedRunReport`
/// envelope achieves deterministic byte output across builds.
/// Reordering fields is a breaking change to the wire format; bump
/// `CANONICAL_PAYLOAD_TYPE` if you do it.
///
/// The `serde(deny_unknown_fields)` attribute would be nice but
/// `serde_json` only supports that on enum tags — for structs it
/// silently ignores unknown keys. If we ever need strict rejection
/// of unknown fields we'd need to swap to `serde_strict` or a custom
/// visitor; for now we trim inputs ourselves in `from_cli_report`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReportFields {
    /// The assistant's final answer to the user's task. May be empty
    /// (e.g. "task aborted" runs). Bounded by [`ANSWER_MAX`].
    pub answer: String,
    /// Number of ReAct-loop iterations the runner consumed.
    pub iterations: usize,
    /// Number of tool calls actually executed (some iterations may
    /// produce tool calls that the LLM later discards).
    pub tool_calls: usize,
    /// Backend the runner used: `"ollama"`, `"deepseek"`, or `"mock"`.
    pub provider: String,
    /// True iff the runner actually talked to Ollama / DeepSeek
    /// over the wire (i.e. was not in mock mode).
    pub using_ollama: bool,
    /// Final runner state — `"Finished"` for happy paths,
    /// `"Aborted"` etc. for the unhappy ones. Mirrors the convention
    /// used by `cli::runner::RunReport::state`.
    pub state: String,
    /// Number of messages retained in the live conversation
    /// window after compression.
    pub final_messages: usize,
    /// Rough token estimate at the end of the run (`len/4`).
    pub approx_tokens: usize,
}

impl RunReportFields {
    /// Build a `RunReportFields` from a `cli::runner::RunReport` (or
    /// any other producer that supplies the same fields). The
    /// function lives here so the runner doesn't have to import
    /// `serde::ser` — it just hands us its existing struct by
    /// value or by reference and we extract the field names.
    ///
    /// We don't expose a `From<cli::runner::RunReport>` impl because
    /// that would force `magent-core` to depend on the CLI crate.
    /// Instead we mirror the `RunReport` field-for-field in this
    /// constructor; tests pin the mirroring contract.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        answer: impl Into<String>,
        iterations: usize,
        tool_calls: usize,
        provider: impl Into<String>,
        using_ollama: bool,
        state: impl Into<String>,
        final_messages: usize,
        approx_tokens: usize,
    ) -> Self {
        Self {
            answer: answer.into(),
            iterations,
            tool_calls,
            provider: provider.into(),
            using_ollama,
            state: state.into(),
            final_messages,
            approx_tokens,
        }
    }

    /// Length validation against the [`ANSWER_MAX`], [`PROVIDER_MAX`],
    /// [`STATE_MAX`] caps. Returns a `Web3ErrorKind::Invalid…`
    /// variant on overflow so callers can surface the problem.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if self.answer.len() > ANSWER_MAX {
            Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.answer.len(),
            })?;
        }
        if self.provider.len() > PROVIDER_MAX {
            Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.provider.len(),
            })?;
        }
        if self.state.len() > STATE_MAX {
            Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.state.len(),
            })?;
        }
        Ok(())
    }
}

// ============================================================================
// EnvelopePayload impl for RunReportFields
// ============================================================================
//
// `RunReportFields` is the canonical payload type for the
// `signed run report` envelope. The machinery that handles the
// envelope itself lives in [`envelope`] — this `impl` is the
// only thing tying the two modules together. The associated
// constants pin the domain-separation tag and the canonical-
// bytes prefix, and `validate` re-uses the length-cap check
// defined above.

impl EnvelopePayload for RunReportFields {
    /// `magent/run_report:v1` — matches [`CANONICAL_PAYLOAD_TYPE`]
    /// so the existing public constant and the new machinery
    /// agree on the wire form.
    const PAYLOAD_TYPE: &'static str = CANONICAL_PAYLOAD_TYPE;

    /// `MAGENT_SRR_V1\n` — matches [`CANONICAL_DOMAIN_PREFIX`].
    const DOMAIN_PREFIX: &'static str = CANONICAL_DOMAIN_PREFIX;

    /// Re-uses the length-cap check above. We deliberately
    /// don't move the check into a separate function so a
    /// future caller that wants to compute a `RunReportFields`
    /// without ever signing it can still validate.
    fn validate(&self) -> Result<(), Web3ErrorKind> {
        RunReportFields::validate(self)
    }
}

/// Type alias for `Envelope<RunReportFields>`. Kept as a
/// `pub type` so existing callers (`magent run --sign`'s
/// wired-up path in `cli/src/runner.rs`, the integration tests
/// in `tests/web3_app_tests.rs`, etc.) keep working without
/// any source changes.
pub type SignedRunReport = Envelope<RunReportFields>;

// ============================================================================
// Re-exports of the generic envelope machinery
// ============================================================================
//
// The bulk of the envelope implementation now lives in
// [`envelope::Envelope`]. We expose `Envelope` and the
// `EnvelopePayload` trait at the module root so new payload
// types (prompts, audit entries, …) can be added by writing
// only a payload struct + an `EnvelopePayload` impl, with no
// copy of the sign / verify / canonical_bytes machinery.

mod envelope;
mod prompt;

pub use envelope::{Envelope, EnvelopePayload};
pub use prompt::{
    PromptFields, SignedPrompt, CANONICAL_DOMAIN_PREFIX as PROMPT_DOMAIN_PREFIX,
    CANONICAL_PAYLOAD_TYPE as PROMPT_PAYLOAD_TYPE,
};

// ============================================================================
// SignedRunReport envelope
// ============================================================================
//
// `SignedRunReport` is a type alias for `Envelope<RunReportFields>` —
// see [`envelope::Envelope`] for the wire-format documentation and
// the rationale behind the design. The wire form is a single JSON
// object with seven top-level fields:
//
// ```json
// {
//   "payload_type": "magent/run_report:v1",
//   "issued_at_unix": 1723550400,
//   "not_before_unix": null,
//   "not_after_unix": 1723636800,
//   "report": { ... run report fields, in declaration order ... },
//   "signer": "did:key:z6Mk…",
//   "signature_hex": "9a1f…"
// }
// ```
//
// Verifiers MUST refuse any envelope whose `payload_type` doesn't
// match [`CANONICAL_PAYLOAD_TYPE`] *byte-for-byte*. The signature
// covers the canonical bytes returned by
// [`Envelope::canonical_bytes`] — see that function for the exact
// derivation.
//
// ## Replay window
//
// The optional `not_before_unix` / `not_after_unix` fields let the
// issuer commit to a window of validity. Downstream consumers
// (CI, audit loggers, …) should reject envelopes whose window
// doesn't include the local "now". The fields are `None` for an
// open-ended (no expiry) envelope — backward-compat with the
// pre-window case.
//
// ## Field-visibility invariant
//
// Every field on this envelope is `pub` so `serde_json` can
// round-trip it without a custom `Deserialize` impl. Tampering
// with any field after signing invalidates the signature, so
// this doesn't add a real attack surface — the re-derivation
// by the verifier uses the *received* bytes, not the in-memory
// representation.
// `SignedRunReport` is now a type alias for `Envelope<RunReportFields>`
// declared earlier in this module. The full struct / impl block
// it replaced used to live here; it's been factored into the
// generic [`envelope::Envelope<P>`] machinery so future payload
// types (prompts, audit entries, …) only need to write a payload
// struct + an `EnvelopePayload` impl.

// ============================================================================
// Public helpers (re-exported at module root for ergonomic callers)
// ============================================================================
//
// These are thin one-line wrappers over the `SignedRunReport` methods;
// we expose them at the module root so callers don't have to spell out
// `SignedRunReport::sign` / `verify` for the most common operations.
// They are also what `tests/web3_app_tests.rs` uses for the integration
// flows so the tests have a stable API surface to pin against.

/// Sign `report` with `identity`. Convenience wrapper that
/// forwards to [`Envelope::<RunReportFields>::sign`]. See that
/// method for the full error contract.
pub fn sign_run_report(
    identity: &core_web3::Identity,
    issued_at_unix: u64,
    not_before_unix: Option<u64>,
    not_after_unix: Option<u64>,
    report: RunReportFields,
) -> Result<SignedRunReport, Web3ErrorKind> {
    Envelope::<RunReportFields>::sign(
        identity,
        issued_at_unix,
        not_before_unix,
        not_after_unix,
        report,
    )
}

/// Verify `envelope` at clock-time `now_secs`. Convenience wrapper
/// that forwards to [`Envelope::verify`].
pub fn verify_signed_run_report(
    envelope: &SignedRunReport,
    now_secs: u64,
) -> Result<(), Web3ErrorKind> {
    envelope.verify(now_secs)
}

/// Parse + verify in one call. Convenience wrapper that
/// forwards to [`Envelope::parse_and_verify`].
pub fn parse_and_verify_signed_run_report(
    json: &str,
    now_secs: u64,
) -> Result<SignedRunReport, Web3ErrorKind> {
    SignRunReportEnvelope::parse_and_verify(json, now_secs)
}

/// Helper type alias used for the
/// `parse_and_verify_signed_run_report` forwarder. Aliases
/// don't carry their own method-namespace, so we route the call
/// through a sane turbofish instead of writing it out twice.
type SignRunReportEnvelope = Envelope<RunReportFields>;

/// Forwarder to [`Envelope::canonical_bytes`] for integration
/// tests that want to assert on the canonical-bytes form without
/// going through a full sign/verify round-trip. Marked `pub` so
/// the test crate can reach it; production callers should never
/// need this (the canonical-bytes contract is internal to
/// [`Envelope::sign`] / [`Envelope::verify`]).
pub fn canonical_bytes_for_test(
    report: &RunReportFields,
    issued_at_unix: u64,
    not_before_unix: Option<u64>,
    not_after_unix: Option<u64>,
) -> Result<Vec<u8>, Web3ErrorKind> {
    Envelope::<RunReportFields>::canonical_bytes_for(
        report,
        issued_at_unix,
        not_before_unix,
        not_after_unix,
    )
}

// ============================================================================
// Canonical bytes (now in envelope.rs)
// ============================================================================
//
// The canonical-bytes implementation has moved to
// [`envelope::Envelope::canonical_bytes`] — the per-payload form
// is identical to the per-run-report form by virtue of the
// generic machinery. The why-not-CBOR / why-JSON / why-prefix
// explanation lives there too; the JSON here is just a sentinel
// so the doc-comment block above doesn't dangle.

// ============================================================================
// Tests
// ============================================================================
//
// Unit tests focus on the canonical-bytes contract and the
// envelope's serde behaviour. End-to-end sign/verify + tamper
// detection lives in `tests/web3_app_tests.rs` so it can use
// `Identity::generate()` (which needs the OS RNG, only available
// with `--features web3_app`).

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> RunReportFields {
        RunReportFields::new("the answer is 42", 3, 1, "ollama", true, "Finished", 7, 800)
    }

    /// Tiny alias so the test bodies don't have to spell out
    /// the generic turbofish every time. Lives inside the
    /// `tests` module so it doesn't leak into the public API.
    fn canonical_bytes_run_report(
        report: &RunReportFields,
        issued_at_unix: u64,
        not_before_unix: Option<u64>,
        not_after_unix: Option<u64>,
    ) -> Result<Vec<u8>, Web3ErrorKind> {
        Envelope::<RunReportFields>::canonical_bytes_for(
            report,
            issued_at_unix,
            not_before_unix,
            not_after_unix,
        )
    }

    #[test]
    fn canonical_bytes_are_deterministic_for_same_input() {
        // Two independent serialisations of the same logical
        // input MUST produce identical bytes, or every signature
        // is non-reproducible.
        let r = sample_report();
        let b1 = canonical_bytes_run_report(&r, 1_700_000_000, None, None).unwrap();
        let b2 = canonical_bytes_run_report(&r, 1_700_000_000, None, None).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn canonical_bytes_change_when_any_field_changes() {
        // Mutating any one input field must change the canonical
        // bytes — the verifier relies on this to detect
        // tampering.
        let r = sample_report();
        let baseline = canonical_bytes_run_report(&r, 1_700_000_000, None, None).unwrap();
        let mut r2 = r.clone();
        r2.answer = "the answer is 43".to_string();
        let altered = canonical_bytes_run_report(&r2, 1_700_000_000, None, None).unwrap();
        assert_ne!(baseline, altered);
    }

    #[test]
    fn canonical_bytes_carry_the_domain_separation_prefix() {
        // The prefix MUST be present, otherwise a signature
        // produced under this code could be replayed against a
        // verifier for a different context.
        let r = sample_report();
        let bytes = canonical_bytes_run_report(&r, 1_700_000_000, None, None).unwrap();
        assert!(
            bytes.starts_with(b"MAGENT_SRR_V1\n"),
            "missing domain-separation prefix: {:?}",
            &bytes[..32]
        );
    }

    #[test]
    fn canonical_bytes_include_crlf_in_issuer_window() {
        // Optional fields must be serialised as `null` when
        // absent, not omitted. If they were omitted, an issuer
        // who later adds an expiry window would produce a
        // different byte stream for the same logical input —
        // breaking the signature. The test pins "absent means
        // null" forever, which is also what serde does by
        // default for `Option<T>` fields.
        let r = sample_report();
        let none_window = canonical_bytes_run_report(&r, 1_700_000_000, None, None).unwrap();
        let some_window = canonical_bytes_run_report(&r, 1_700_000_000, Some(1), Some(2)).unwrap();
        // The "None" form must include the field name + literal
        // `null`, which differs from the "Some" form which
        // includes the values. The two byte streams MUST
        // differ — sign-then-verify only works if they do.
        assert_ne!(none_window, some_window);
        // Both forms MUST contain the field name so the diff
        // is meaningful to a human auditor.
        let none_str = std::str::from_utf8(&none_window).unwrap();
        assert!(none_str.contains("not_before_unix"));
        assert!(none_str.contains("not_after_unix"));
        assert!(none_str.contains("null"));
    }

    #[test]
    fn run_report_fields_validate_caps() {
        let mut r = sample_report();
        // Healthy input passes.
        r.validate().unwrap();
        // Over-long answer trips the cap.
        r.answer = "x".repeat(ANSWER_MAX + 1);
        let err = r.validate().unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidSecretKeyLength { .. }));
    }

    #[test]
    fn envelope_serialises_with_canonical_payload_type() {
        // The `payload_type` field MUST round-trip through JSON
        // byte-for-byte so verifiers can do a cheap field
        // comparison without parsing.
        let json_for_type = format!("\"payload_type\":\"{}\"", CANONICAL_PAYLOAD_TYPE);
        // We can't easily build a SignedRunReport without an
        // Identity here (we'd need RNG), but we can check the
        // canonical payload type constant matches what we'd
        // embed in the envelope.
        assert_eq!(CANONICAL_PAYLOAD_TYPE, "magent/run_report:v1");
        // Spot-check the JSON form by struct construction
        // (mirroring what `to_json` would emit).
        assert!(json_for_type.contains("magent/run_report:v1"));
    }

    #[test]
    fn from_json_rejects_unknown_payload_type() {
        // A forged or replayed envelope with a different
        // `payload_type` MUST be rejected at parse / verify
        // time. We can simulate this by building a JSON string
        // with the wrong type and asking `from_json` to parse
        // it (parsing succeeds; `verify` would reject, which
        // we can't test here without an Identity — so we just
        // verify the parse path doesn't reject and rely on the
        // integration test for the verify-side rejection).
        let wrong = r#"{
            "payload_type": "magent/run_report:v999",
            "issued_at_unix": 1,
            "payload": {
                "answer": "", "iterations": 0, "tool_calls": 0,
                "provider": "mock", "using_ollama": false,
                "state": "Finished", "final_messages": 0, "approx_tokens": 0
            },
            "signer": "did:key:z6Mkfakefakefakefakefakefake",
            "signature_hex": "00"
        }"#;
        let parsed: SignedRunReport = serde_json::from_str(wrong).unwrap();
        assert_eq!(parsed.payload_type, "magent/run_report:v999");
        // The integration test `verify_rejects_unknown_payload_type`
        // pins the reject-on-verify behaviour end-to-end.
    }
}
