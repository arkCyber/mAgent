//! Web3-specific error helpers.
//!
//! This module is a thin adapter that converts the various failure
//! modes of the underlying crypto crates (`ed25519-dalek`, `bs58`,
//! `rand_core`) into a single [`crate::error::Web3ErrorKind`] so the
//! rest of the codebase only has to match one error type.

use alloc::string::{String, ToString};

use crate::error::{AgentError, ParseFailureKind, Web3ErrorKind};

/// Extension trait that lifts a `Result<_, Web3ErrorKind>` into a
/// `Result<_, AgentError>` and centralises the conversion.
///
/// Implemented as an extension trait (rather than a `From` impl)
/// because [`Web3ErrorKind`] lives in `crate::error` and the impl
/// would create a cycle in the orphan-rule sense — `From` is
/// perfectly legal here actually, but the trait form lets us add
/// conversion methods that take context (e.g. `with_did`).
pub trait Web3ErrorExt<T> {
    /// Convert into a `Result<_, AgentError>`. Sugar over the
    /// `From` impl below.
    fn into_agent(self) -> Result<T, AgentError>;

    /// Convert into a `Result<_, AgentError>`, attaching a `did:key`
    /// identifier to the error for diagnostics. The DID is only
    /// embedded when the error variant carries a public-key
    /// reference; for purely local errors (RngError, HexDecode, …)
    /// it's discarded.
    fn with_did(self, did: &str) -> Result<T, AgentError>;
}

impl<T> Web3ErrorExt<T> for Result<T, Web3ErrorKind> {
    fn into_agent(self) -> Result<T, AgentError> {
        self.map_err(|kind| AgentError::Web3Error { kind })
    }

    fn with_did(self, did: &str) -> Result<T, AgentError> {
        self.map_err(|kind| {
            // Re-tag any variant that has a place to embed the DID.
            // Variants without one (RngError, HexDecode, …) keep
            // their original cause and surface as-is.
            let kind = match kind {
                Web3ErrorKind::DidKeyMismatch { .. } => Web3ErrorKind::DidKeyMismatch {
                    did: did.to_string(),
                },
                other => other,
            };
            AgentError::Web3Error { kind }
        })
    }
}

impl From<Web3ErrorKind> for AgentError {
    fn from(kind: Web3ErrorKind) -> Self {
        AgentError::Web3Error { kind }
    }
}

/// Helper: build a `Web3ErrorKind::InvalidDid` from a borrowed string
/// without forcing every call site to allocate.
pub fn invalid_did(raw: &str) -> Web3ErrorKind {
    Web3ErrorKind::InvalidDid {
        raw: raw.to_string(),
    }
}

/// Helper: build a `Web3ErrorKind::Base58Decode` with a borrowed
/// message.
pub fn base58_err(msg: impl Into<String>) -> Web3ErrorKind {
    Web3ErrorKind::Base58Decode(msg.into())
}

/// Helper: build a `Web3ErrorKind::HexDecode` with a borrowed
/// message.
pub fn hex_err(msg: impl Into<String>) -> Web3ErrorKind {
    Web3ErrorKind::HexDecode(msg.into())
}

/// Helper: build a `Web3ErrorKind::RngError` with a borrowed message.
pub fn rng_err(msg: impl Into<String>) -> Web3ErrorKind {
    Web3ErrorKind::RngError(msg.into())
}

/// Helper: build a `Web3ErrorKind::InvalidPublicKey` from a
/// decoded byte length.
pub fn invalid_pk(actual_len: usize) -> Web3ErrorKind {
    Web3ErrorKind::InvalidPublicKey { actual_len }
}

/// Helper: build a `Web3ErrorKind::InvalidSignature` from a
/// decoded byte length.
pub fn invalid_sig(actual_len: usize) -> Web3ErrorKind {
    Web3ErrorKind::InvalidSignature { actual_len }
}

/// Helper: build a `Web3ErrorKind::InvalidSecretKeyLength` from a
/// byte length.
pub fn invalid_sk(actual: usize) -> Web3ErrorKind {
    Web3ErrorKind::InvalidSecretKeyLength { actual }
}

/// Helper: build a `Web3ErrorKind::Parse` for a wire-format
/// envelope (e.g. `SignedMessage`) that failed to deserialise.
///
/// `kind` distinguishes "input wasn't JSON" from "input was JSON
/// but didn't match the schema" from "a length field was wrong";
/// `message` is the underlying `serde_json` (or similar) error
/// message, retained for diagnostics.
pub fn parse_err(kind: ParseFailureKind, message: impl Into<String>) -> Web3ErrorKind {
    Web3ErrorKind::Parse {
        kind,
        message: message.into(),
    }
}