//! `magent_core::web3_app::prompt` — signed-prompt envelope.
//!
//! This is the second payload type built on top of the generic
//! [`crate::web3_app::Envelope<P>`] machinery. It exercises the
//! abstraction by giving it a structurally different payload
//! (the [`PromptFields`] mirrors `cli::prompt::PromptRecord`)
//! and a different domain-separation tag, so a verifier that
//! accidentally treats a signed prompt as a signed run report
//! (or vice versa) fails closed at the first byte of the
//! canonical-bytes prefix.
//!
//! ## Why this lives in `magent-core` rather than `cli`
//!
//! The CLI's `magent set-prompt` subcommand already owns the
//! `PromptRecord` *storage* type (filesystem layout, JSON
//! schema, metadata validation). What it does NOT own is the
//! *cryptographic identity* — that belongs to `magent-core` so
//! `magent-core` doesn't need to depend on the CLI crate for
//! verification. We mirror the storage fields into a
//! [`PromptFields`] struct that's `Serialize + Deserialize` and
//! `EnvelopePayload`-compatible, then convert at the CLI
//! boundary (the runner / `set-prompt` action does the
//! `PromptRecord::from(PromptFields)` and back).
//!
//! ## Adding a new payload type
//!
//! Future envelope payload types follow exactly the shape you
//! see here: a `Fields` struct, a `validate()` impl, an
//! `EnvelopePayload` impl with the two associated constants.
//! The bulk of the sign / verify / canonical_bytes machinery
//! is shared with `SignedRunReport` via the [`Envelope`]
//! generic.

use std::string::String;
use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use crate::web3_app::envelope::{Envelope, EnvelopePayload};

// ============================================================================
// Constants
// ============================================================================

/// Domain-separation tag for the signed-prompt envelope.
///
/// Bump the `:vN` suffix when the canonical encoding changes in
/// an incompatible way. Verifiers MUST compare this field
/// byte-for-byte against the envelope's `payload_type` before
/// accepting a signature — anything else is a downgrade attack.
pub const CANONICAL_PAYLOAD_TYPE: &str = "magent/prompt:v1";

/// Canonical-bytes prefix for prompts. MUST be unique across
/// every payload type this crate emits (today: run reports and
/// prompts; future: audit entries, …). The constant is exposed
/// here so the per-payload `EnvelopePayload` impl can pin it
/// without diverging from the cross-cutting helper.
pub const CANONICAL_DOMAIN_PREFIX: &str = "MAGENT_PR_V1\n";

// ============================================================================
// Length caps
// ============================================================================

/// Maximum length (bytes) of the `prompt` field. Mirrors the
/// `cli::prompt::PromptRecord` convention so the two stores
/// reject the same too-large inputs.
pub const PROMPT_MAX: usize = 32 * 1024;

/// Maximum length (bytes) of the `name` field. Mirrors the
/// prompt store's `PROMPT_NAME_MAX`.
pub const PROMPT_NAME_MAX: usize = 128;

/// Maximum length (bytes) of the `provider` field.
pub const PROMPT_PROVIDER_MAX: usize = 64;

/// Maximum length (bytes) of the `model` field.
pub const PROMPT_MODEL_MAX: usize = 128;

// ============================================================================
// PromptFields
// ============================================================================

/// Pure-data mirror of `cli::prompt::PromptRecord`. Lives here
/// so the envelope machinery doesn't have to reach into the CLI
/// crate for the canonical payload fields.
///
/// Field order in this struct is **load-bearing** — serde emits
/// fields in declaration order, and the canonical-bytes form
/// relies on a deterministic byte stream. Reordering fields is
/// a breaking change to the wire format; bump
/// [`CANONICAL_PAYLOAD_TYPE`] if you do it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptFields {
    /// Lower-case, filesystem-safe identifier. The signed
    /// envelope's `payload_type` field is the **discriminator**
    /// — the `name` here is the *content* identifier, not the
    /// domain-separation tag.
    pub name: String,
    /// The actual system prompt text. Bounded by [`PROMPT_MAX`].
    pub prompt: String,
    /// Provider name (`"ollama"`, `"deepseek"`). Empty means
    /// "any provider".
    #[serde(default)]
    pub provider: String,
    /// Model name. Empty means "use the provider's default".
    #[serde(default)]
    pub model: String,
    /// Unix seconds the prompt was created. Optional in the
    /// wire form so a hand-written JSON file can omit it; the
    /// CLI fills it in on first write.
    #[serde(default)]
    pub created_at: u64,
    /// Unix seconds the prompt was last updated. Same default
    /// behaviour as `created_at`.
    #[serde(default)]
    pub updated_at: u64,
}

impl PromptFields {
    /// Build a `PromptFields` from a `cli::prompt::PromptRecord`
    /// (or any other producer that supplies the same fields).
    /// The function lives here so the runner / `set-prompt`
    /// action doesn't have to spell out field-by-field
    /// construction at every call site.
    ///
    /// The number of arguments mirrors `PromptRecord` directly —
    /// 6 positional fields (name, prompt, provider, model, two
    /// timestamps) is the smallest shape that captures the
    /// schema without splitting into a builder struct.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        prompt: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        created_at: u64,
        updated_at: u64,
    ) -> Self {
        Self {
            name: name.into(),
            prompt: prompt.into(),
            provider: provider.into(),
            model: model.into(),
            created_at,
            updated_at,
        }
    }

    /// Length validation against the [`PROMPT_MAX`] /
    /// [`PROMPT_NAME_MAX`] / [`PROMPT_PROVIDER_MAX`] /
    /// [`PROMPT_MODEL_MAX`] caps. Returns a
    /// `Web3ErrorKind::Invalid…` variant on overflow so callers
    /// can surface the problem at sign time (before the
    /// verifier will silently reject the envelope).
    ///
    /// We re-use the `InvalidSecretKeyLength` variant because
    /// it's the only "field too long" variant in
    /// [`Web3ErrorKind`]; a future cleanup could introduce
    /// `InvalidField { name, actual, max }` and migrate these
    /// call sites.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if self.prompt.len() > PROMPT_MAX {
            return Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.prompt.len(),
            });
        }
        if self.name.len() > PROMPT_NAME_MAX {
            return Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.name.len(),
            });
        }
        if self.provider.len() > PROMPT_PROVIDER_MAX {
            return Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.provider.len(),
            });
        }
        if self.model.len() > PROMPT_MODEL_MAX {
            return Err(Web3ErrorKind::InvalidSecretKeyLength {
                actual: self.model.len(),
            });
        }
        Ok(())
    }
}

impl EnvelopePayload for PromptFields {
    /// `magent/prompt:v1` — the domain-separation tag.
    const PAYLOAD_TYPE: &'static str = CANONICAL_PAYLOAD_TYPE;

    /// `MAGENT_PR_V1\n` — the canonical-bytes prefix. Pinned
    /// to [`CANONICAL_DOMAIN_PREFIX`] so the const lives in
    /// exactly one place.
    const DOMAIN_PREFIX: &'static str = CANONICAL_DOMAIN_PREFIX;

    /// Re-uses the length-cap check above.
    fn validate(&self) -> Result<(), Web3ErrorKind> {
        PromptFields::validate(self)
    }
}

// ============================================================================
// Type alias
// ============================================================================

/// Type alias for `Envelope<PromptFields>`. Use this everywhere
/// the signed-prompt envelope is referenced.
pub type SignedPrompt = Envelope<PromptFields>;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_fields_validate_caps() {
        let p = PromptFields::new("ok", "hello", "mock", "llama3.2", 1, 2);
        p.validate().unwrap();

        let mut bad = p.clone();
        bad.prompt = "x".repeat(PROMPT_MAX + 1);
        let err = bad.validate().unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidSecretKeyLength { .. }));
    }

    #[test]
    fn canonical_payload_type_constant_is_stable() {
        // Pin the wire-format string so a refactor that changes
        // the magic string is caught immediately.
        assert_eq!(CANONICAL_PAYLOAD_TYPE, "magent/prompt:v1");
    }

    #[test]
    fn envelope_payload_impl_uses_correct_constants() {
        // The associated const values must agree with the
        // module-level constants so the canonical-bytes
        // contract is defined in one place.
        assert_eq!(PromptFields::PAYLOAD_TYPE, CANONICAL_PAYLOAD_TYPE);
        assert_eq!(PromptFields::DOMAIN_PREFIX, CANONICAL_DOMAIN_PREFIX);
    }

    #[test]
    fn domain_prefix_is_unique_across_payload_types() {
        // Domain-separation defence in depth: the prefix MUST
        // differ from the run-report one (`MAGENT_SRR_V1\n`).
        // If they were equal, a forgery could re-serialise a
        // signed prompt as a signed run report and the
        // canonical-bytes contract would not catch it.
        assert_ne!(PromptFields::DOMAIN_PREFIX, "MAGENT_SRR_V1\n");
        assert_ne!(PromptFields::PAYLOAD_TYPE, "magent/run_report:v1");
    }
}
