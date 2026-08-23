//! Integration tests for the `SignedPrompt` envelope.
//!
//! The sign / verify / canonical_bytes machinery lives in
//! [`magent_core::web3_app::envelope`] and is shared between
//! `SignedRunReport` (covered by `tests/web3_app_tests.rs`)
//! and `SignedPrompt`. These tests pin the per-payload
//! behaviour:
//!
//! * Domain separation — a `SignedPrompt` MUST NOT verify
//!   against the `SignedRunReport` discriminant and vice versa.
//! * Length-cap validation — a too-long `prompt` field is
//!   rejected at sign time so the signer doesn't get a
//!   "wasted" signature.
//! * Determinism — signing the same payload with the same
//!   identity produces the same signature.
//! * JSON round-trip — `to_json` / `from_json` is lossless
//!   and the wire form starts with the per-payload
//!   canonical-bytes prefix.

use magent_core::error::Web3ErrorKind;
use magent_core::web3::Identity;
use magent_core::web3_app::{
    PromptFields, SignedPrompt, PROMPT_DOMAIN_PREFIX, PROMPT_PAYLOAD_TYPE,
};

fn sample_prompt() -> PromptFields {
    PromptFields::new(
        "agent-1-system-prompt",
        "You are a helpful agent. Answer concisely.",
        "ollama",
        "llama3.2",
        1_700_000_000,
        1_700_000_000,
    )
}

#[test]
fn sign_then_verify_round_trip() {
    let id = Identity::generate().unwrap();
    let env = SignedPrompt::sign(&id, 1_700_000_000, None, None, sample_prompt()).unwrap();
    assert_eq!(env.payload_type, PROMPT_PAYLOAD_TYPE);
    assert_eq!(env.signer, id.did_key().as_str());
    env.verify(1_700_000_100).expect("verify should succeed");
}

#[test]
fn verify_rejects_tampered_prompt_body() {
    let id = Identity::generate().unwrap();
    let mut env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    env.payload.prompt = "tampered".to_string();
    let err = env.verify(1).unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::SignatureVerificationFailed),
        "expected SignatureVerificationFailed, got {:?}",
        err
    );
}

#[test]
fn verify_rejects_run_report_payload_type() {
    // Cross-payload replay: someone tampered with
    // `payload_type` to claim this is a signed run report.
    // Step (1) of `Envelope::verify` catches this.
    let id = Identity::generate().unwrap();
    let mut env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    env.payload_type = "magent/run_report:v1".to_string();
    let err = env.verify(1).unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::InvalidDid { .. }),
        "expected InvalidDid, got {:?}",
        err
    );
}

#[test]
fn verify_rejects_unknown_payload_type() {
    let id = Identity::generate().unwrap();
    let mut env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    env.payload_type = "magent/prompt:v999".to_string();
    let err = env.verify(1).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
}

#[test]
fn canonical_bytes_carry_the_prompt_domain_prefix() {
    let id = Identity::generate().unwrap();
    let env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    let bytes = env.canonical_bytes().unwrap();
    assert!(
        bytes.starts_with(b"MAGENT_PR_V1\n"),
        "missing prompt domain prefix; bytes start with: {:?}",
        &bytes[..32]
    );
    // Belt-and-braces: the prefix MUST be the prompt one,
    // not the run-report one. Otherwise a cross-payload
    // forgery would slip past the canonical-bytes contract.
    assert_eq!(
        std::str::from_utf8(&bytes[..PROMPT_DOMAIN_PREFIX.len()]).unwrap(),
        PROMPT_DOMAIN_PREFIX
    );
    assert_ne!(
        std::str::from_utf8(&bytes[..PROMPT_DOMAIN_PREFIX.len()]).unwrap(),
        "MAGENT_SRR_V1\n",
    );
}

#[test]
fn sign_rejects_oversize_prompt_text() {
    let id = Identity::generate().unwrap();
    let mut bad = sample_prompt();
    bad.prompt = "x".repeat(33 * 1024);
    let err = SignedPrompt::sign(&id, 1, None, None, bad).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::InvalidSecretKeyLength { .. }));
}

#[test]
fn signing_is_deterministic() {
    let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
    let env1 = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    let env2 = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    assert_eq!(env1, env2);
    assert_eq!(env1.signature_hex, env2.signature_hex);
}

#[test]
fn json_round_trip() {
    let id = Identity::generate().unwrap();
    let env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    let json = env.to_json();
    let parsed = SignedPrompt::from_json(&json).unwrap();
    assert_eq!(parsed, env);
    parsed.verify(1).unwrap();
}

#[test]
fn parse_and_verify_convenience() {
    let id = Identity::generate().unwrap();
    let env = SignedPrompt::sign(&id, 1, None, None, sample_prompt()).unwrap();
    let json = env.to_json();
    let parsed = SignedPrompt::parse_and_verify(&json, 1).unwrap();
    assert_eq!(parsed, env);
}

#[test]
fn expiry_window_is_enforced() {
    let id = Identity::generate().unwrap();
    let env = SignedPrompt::sign(
        &id,
        1_700_000_000,
        Some(1_700_000_500),
        Some(1_700_001_000),
        sample_prompt(),
    )
    .unwrap();

    // Before the window opens: rejected.
    let err = env.verify(1_700_000_100).unwrap_err();
    let msg = format!("{:?}", err);
    assert!(msg.contains("not yet valid"), "got: {}", msg);

    // Inside the window: accepted.
    env.verify(1_700_000_750).unwrap();

    // After the window closes: rejected.
    let err = env.verify(1_700_001_100).unwrap_err();
    let msg = format!("{:?}", err);
    assert!(msg.contains("expired"), "got: {}", msg);
}
