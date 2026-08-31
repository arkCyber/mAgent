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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AgentError, ParseFailureKind, Web3ErrorKind};
    use alloc::vec::Vec;

    #[test]
    fn into_agent_wraps_ok_value() {
        let r: Result<u32, Web3ErrorKind> = Ok(42);
        assert_eq!(r.into_agent().unwrap(), 42);
    }

    #[test]
    fn into_agent_wraps_error_in_web3_error() {
        let r: Result<(), Web3ErrorKind> = Err(Web3ErrorKind::RngError("boom".into()));
        let err = r.into_agent().unwrap_err();
        assert!(
            matches!(err, AgentError::Web3Error { kind: Web3ErrorKind::RngError(ref m) } if m == "boom")
        );
    }

    #[test]
    fn with_did_passthrough_ok() {
        let r: Result<Vec<u8>, Web3ErrorKind> = Ok(vec![1, 2, 3]);
        assert_eq!(r.with_did("did:key:z6Mkx").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn with_did_retags_did_key_mismatch() {
        let r: Result<(), Web3ErrorKind> = Err(Web3ErrorKind::DidKeyMismatch { did: "old".into() });
        let err = r.with_did("did:key:z6Mknew").unwrap_err();
        match err {
            AgentError::Web3Error {
                kind: Web3ErrorKind::DidKeyMismatch { did },
            } => assert_eq!(did, "did:key:z6Mknew"),
            other => panic!("expected DidKeyMismatch, got {other:?}"),
        }
    }

    #[test]
    fn with_did_keeps_other_variants_unchanged() {
        // A non-DidKeyMismatch variant must keep its original cause —
        // the DID is only embedded where the variant has a slot for it.
        let r: Result<(), Web3ErrorKind> = Err(Web3ErrorKind::HexDecode("bad digit".into()));
        let err = r.with_did("did:key:z6Mkx").unwrap_err();
        assert!(
            matches!(err, AgentError::Web3Error { kind: Web3ErrorKind::HexDecode(ref m) } if m == "bad digit")
        );
    }

    #[test]
    fn from_web3_error_kind_converts() {
        let err: AgentError = Web3ErrorKind::SignatureVerificationFailed.into();
        assert!(matches!(
            err,
            AgentError::Web3Error {
                kind: Web3ErrorKind::SignatureVerificationFailed
            }
        ));
    }

    #[test]
    fn helper_constructors_build_expected_variants() {
        assert!(
            matches!(invalid_did("z6Mkfoo"), Web3ErrorKind::InvalidDid { raw } if raw == "z6Mkfoo")
        );
        assert!(matches!(base58_err("decode"), Web3ErrorKind::Base58Decode(m) if m == "decode"));
        assert!(matches!(hex_err("odd"), Web3ErrorKind::HexDecode(m) if m == "odd"));
        assert!(matches!(rng_err("rng"), Web3ErrorKind::RngError(m) if m == "rng"));
        assert!(matches!(
            invalid_pk(31),
            Web3ErrorKind::InvalidPublicKey { actual_len: 31 }
        ));
        assert!(matches!(
            invalid_sig(63),
            Web3ErrorKind::InvalidSignature { actual_len: 63 }
        ));
        assert!(matches!(
            invalid_sk(16),
            Web3ErrorKind::InvalidSecretKeyLength { actual: 16 }
        ));
        assert!(matches!(
            parse_err(ParseFailureKind::InvalidJson, "x"),
            Web3ErrorKind::Parse { kind: ParseFailureKind::InvalidJson, message } if message == "x"
        ));
    }

    #[test]
    fn helpers_accept_owned_strings() {
        // The `impl Into<String>` helpers must accept both &str and String.
        assert!(
            matches!(base58_err(String::from("owned")), Web3ErrorKind::Base58Decode(m) if m == "owned")
        );
        assert!(
            matches!(hex_err(String::from("owned")), Web3ErrorKind::HexDecode(m) if m == "owned")
        );
        assert!(
            matches!(rng_err(String::from("owned")), Web3ErrorKind::RngError(m) if m == "owned")
        );
        assert!(matches!(
            parse_err(ParseFailureKind::SchemaMismatch, String::from("owned")),
            Web3ErrorKind::Parse {
                kind: ParseFailureKind::SchemaMismatch,
                ..
            }
        ));
    }
}
