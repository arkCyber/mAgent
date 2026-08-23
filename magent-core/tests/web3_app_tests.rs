//! Integration tests for the `web3_app` module — end-to-end sign /
//! verify on real `Identity` instances, including tamper detection,
//! cross-key rejection, domain-separation replay, expiry-window
//! handling, JSON round-trips, and batch verification.
//!
//! These tests need the OS RNG (via `Identity::generate()`), so they
//! live in `tests/` rather than inside the module.

#![cfg(feature = "web3_app")]

use std::string::String;

use magent_core::error::Web3ErrorKind;
use magent_core::web3::{Identity, SignedMessage};
use magent_core::web3_app::{
    canonical_bytes_for_test, parse_and_verify_signed_run_report,
    sign_run_report, verify_signed_run_report, RunReportFields, SignedRunReport,
    CANONICAL_PAYLOAD_TYPE,
};

// ---------------------------------------------------------------------------
// Sign + verify round-trip
// ---------------------------------------------------------------------------

#[test]
fn sign_then_verify_round_trip() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new(
        "the answer is 42",
        3,
        1,
        "ollama",
        true,
        "Finished",
        7,
        800,
    );
    let now = 1_700_000_000u64;
    let signed =
        sign_run_report(&id, now, None, None, r.clone()).expect("sign should not fail");
    verify_signed_run_report(&signed, now + 60).expect("verify should succeed");
}

#[test]
fn sign_then_verify_with_explicit_window() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let issued = 1_700_000_000u64;
    let nb = Some(issued - 60);
    let na = Some(issued + 3600);
    let signed = sign_run_report(&id, issued, nb, na, r).unwrap();
    // Both bounds pass.
    assert!(verify_signed_run_report(&signed, issued).is_ok());
    assert!(verify_signed_run_report(&signed, issued - 30).is_ok());
    assert!(verify_signed_run_report(&signed, issued + 3500).is_ok());
    // Out of bounds fails.
    assert!(verify_signed_run_report(&signed, issued - 120).is_err());
    assert!(verify_signed_run_report(&signed, issued + 4000).is_err());
}

// ---------------------------------------------------------------------------
// Tamper detection
// ---------------------------------------------------------------------------

#[test]
fn verify_rejects_tampered_answer() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("honest", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    signed.payload.answer = "tampered".to_string();
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    // The signature covers the canonical bytes; any change to the
    // report field changes the bytes and breaks the signature.
    assert!(
        matches!(err, Web3ErrorKind::SignatureVerificationFailed),
        "expected SignatureVerificationFailed, got {:?}",
        err
    );
}

#[test]
fn verify_rejects_tampered_issued_at() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    signed.issued_at_unix = 1_700_000_001;
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::SignatureVerificationFailed));
}

#[test]
fn verify_rejects_tampered_signature() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    // Replace the first hex char with '0' (still valid hex).
    let mut first = signed.signature_hex.remove(0);
    first = if first == '0' { '1' } else { '0' };
    signed.signature_hex.insert(0, first);
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::SignatureVerificationFailed));
}

#[test]
fn verify_rejects_wrong_signer_did() {
    // Alice signs; bob's `did:key` is on the envelope — must fail.
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let r = RunReportFields::new("from alice", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&alice, 1_700_000_000, None, None, r).unwrap();
    // Swap the signer DID to bob's. This is the most adversarial
    // tamper — the envelope now claims to be signed by bob, but
    // bob never saw the report.
    signed.signer = bob.did_key().as_str();
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    // The signature is valid for alice's public key, but the
    // envelope claims bob. So the verifier extracts bob's public
    // key from the swapped DID, attempts to verify alice's
    // signature against bob's key, and fails with
    // SignatureVerificationFailed.
    assert!(matches!(err, Web3ErrorKind::SignatureVerificationFailed));
}

// ---------------------------------------------------------------------------
// Domain separation
// ---------------------------------------------------------------------------

#[test]
fn verify_rejects_unknown_payload_type() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    signed.payload_type = "magent/run_report:v999".to_string();
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
}

#[test]
fn verify_rejects_empty_payload_type() {
    // Defence in depth: an empty `payload_type` is also a domain-
    // separation failure (it can't match the canonical type byte-
    // for-byte).
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let mut signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    signed.payload_type = String::new();
    let err = verify_signed_run_report(&signed, 1_700_000_100).unwrap_err();
    assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
}

// ---------------------------------------------------------------------------
// Expiry window
// ---------------------------------------------------------------------------

#[test]
fn verify_rejects_pre_window_now() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let issued = 1_700_000_000u64;
    // Open envelope starting in 60 seconds.
    let signed = sign_run_report(&id, issued, Some(issued + 60), None, r).unwrap();
    let err = verify_signed_run_report(&signed, issued + 30).unwrap_err();
    // The error is tagged as InvalidDid to flag the window
    // violation; semantically it's a "not yet valid" envelope.
    let msg = format!("{:?}", err);
    assert!(msg.contains("not yet valid"), "expected 'not yet valid' in: {}", msg);
}

#[test]
fn verify_rejects_post_window_now() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let issued = 1_700_000_000u64;
    let signed = sign_run_report(&id, issued, None, Some(issued + 60), r).unwrap();
    let err = verify_signed_run_report(&signed, issued + 120).unwrap_err();
    let msg = format!("{:?}", err);
    assert!(msg.contains("expired"), "expected 'expired' in: {}", msg);
}

// ---------------------------------------------------------------------------
// JSON round-trip
// ---------------------------------------------------------------------------

#[test]
fn envelope_json_round_trips() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new(
        "answer with \"quotes\" and \n newlines",
        5,
        2,
        "deepseek",
        true,
        "Finished",
        12,
        1500,
    );
    let issued = 1_700_000_000u64;
    let nb = Some(1_699_999_900);
    let na = Some(1_700_000_900);
    let signed = sign_run_report(&id, issued, nb, na, r.clone()).unwrap();
    let json = signed.to_json();
    let parsed = SignedRunReport::from_json(&json).unwrap();
    // Bit-for-bit equality on every field is the strongest test
    // of the round-trip — serde's deterministic field order plus
    // our hex-encoded signature and DID strings give us exactly
    // that.
    assert_eq!(parsed, signed);
    // The JSON should still verify.
    verify_signed_run_report(&parsed, issued + 10).unwrap();
}

#[test]
fn from_json_then_verify_convenience_helper_works() {
    let id = Identity::generate().unwrap();
    let r = RunReportFields::new("ok", 1, 0, "mock", false, "Finished", 0, 0);
    let signed = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
    let json = signed.to_json();
    let _parsed =
        parse_and_verify_signed_run_report(&json, 1_700_000_100).expect("parse+verify must succeed");
}

#[test]
fn canonical_payload_type_constant_is_stable() {
    // Pin the wire-format string so a refactor that changes the
    // magic string is caught immediately. The string is the
    // domain-separation tag and appears in every envelope — any
    // change here is a breaking change to the wire format.
    assert_eq!(CANONICAL_PAYLOAD_TYPE, "magent/run_report:v1");
}

// ---------------------------------------------------------------------------
// Cross-protocol replay
// ---------------------------------------------------------------------------

#[test]
fn cross_protocol_replay_is_rejected() {
    // An attacker takes a SignedMessage (the lower-level web3
    // envelope used for arbitrary blobs) and wraps it in a
    // `SignedRunReport` claiming it came from the agent. The
    // signatures were produced for different canonical byte
    // sequences (one covers the SignedMessage payload, the other
    // covers the run-report prefix + fields), so the verifier
    // MUST reject the cross-protocol forged envelope.
    let id = Identity::generate().unwrap();
    // Original low-level SignedMessage (covers b"hello").
    let original: SignedMessage = id.sign(b"hello").unwrap();
    // Take its raw fields and stuff them into a SignedRunReport.
    let forged = SignedRunReport {
        payload_type: CANONICAL_PAYLOAD_TYPE.to_string(),
        issued_at_unix: 1_700_000_000,
        not_before_unix: None,
        not_after_unix: None,
        payload: RunReportFields::new("hello", 0, 0, "mock", false, "Finished", 0, 0),
        signer: original.signer.clone(),
        signature_hex: original.signature_hex.clone(),
    };
    // The forgery must fail — the verifier recomputes the
    // canonical bytes ("MAGENT_SRR_V1\n{...}\n") and feeds them
    // to ed25519-dalek, which finds that the signature actually
    // covered a different byte sequence ("hello").
    let err =
        verify_signed_run_report(&forged, 1_700_000_100).unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::SignatureVerificationFailed),
        "cross-protocol replay must be rejected: {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// Determinism across runs
// ---------------------------------------------------------------------------

#[test]
fn signing_is_deterministic_for_same_payload() {
    let id = Identity::from_secret_bytes(&[42u8; 32]).unwrap();
    let r = RunReportFields::new("deterministic", 1, 0, "mock", false, "Finished", 0, 0);
    // Same sign-call repeated must yield the same envelope — the
    // domain separator, the canonical-bytes form, and RFC 8032
    // deterministic Ed25519 signing combine to give us perfect
    // reproducibility.
    let s1 = sign_run_report(&id, 1, None, None, r.clone()).unwrap();
    let s2 = sign_run_report(&id, 1, None, None, r).unwrap();
    assert_eq!(s1, s2);
    assert_eq!(s1.signature_hex, s2.signature_hex);
}

// ---------------------------------------------------------------------------
// Batch verification
// ---------------------------------------------------------------------------

#[test]
fn batch_verify_handles_mixed_valid_and_tampered() {
    // 10 envelopes: 7 clean, 3 tampered. batch-verify should
    // surface exactly 3 failures and 7 successes.
    let id = Identity::generate().unwrap();
    let mut envelopes = Vec::new();
    for i in 0..10 {
        let r = RunReportFields::new(
            format!("batch item {}", i),
            i,
            i / 2,
            "mock",
            false,
            "Finished",
            0,
            0,
        );
        let s = sign_run_report(&id, 1_700_000_000, None, None, r).unwrap();
        envelopes.push(s);
    }
    // Tamper with #2, #5, #8. The `Envelope<P>` machinery
    // stores the typed payload under the generic field
    // `payload` (rather than the pre-refactor `report`),
    // matching the new wire format.
    envelopes[2].payload.answer = "tampered".to_string();
    envelopes[5].payload.iterations = 999;
    envelopes[8].payload.provider = "tampered".to_string();

    let now = 1_700_000_100;
    let mut ok = 0;
    let mut failed = 0;
    for env in &envelopes {
        match verify_signed_run_report(env, now) {
            Ok(()) => ok += 1,
            Err(Web3ErrorKind::SignatureVerificationFailed) => failed += 1,
            Err(other) => panic!("unexpected error: {:?}", other),
        }
    }
    assert_eq!(ok, 7, "expected 7 valid envelopes");
    assert_eq!(failed, 3, "expected 3 tampered envelopes");
}

// ---------------------------------------------------------------------------
// Public helpers we re-export for the convenience test
// ---------------------------------------------------------------------------

mod helpers {
    // The integration tests import the helper functions from the
    // module root (`magent_core::web3_app::…`); listed here so
    // future readers know which top-level entry points we expect
    // callers to use, but no actual re-exports are needed.
}

// ---------------------------------------------------------------------------
// A trivial smoke test for canonical_bytes_for_test (which is a
// window into the production helper), so a refactor that breaks
// the canonicalisation surfaces immediately.
// ---------------------------------------------------------------------------

#[test]
fn canonical_bytes_smoke() {
    let r = RunReportFields::new("smoke", 1, 0, "mock", false, "Finished", 0, 0);
    let bytes = canonical_bytes_for_test(&r, 1, None, None).unwrap();
    // Output MUST start with the domain-separation prefix.
    assert!(
        bytes.starts_with(b"MAGENT_SRR_V1\n"),
        "missing prefix: {:?}",
        &bytes[..32]
    );
    // And it MUST contain the `provider` field somewhere (sanity
    // check that the report got serialised, not just the prefix).
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("provider"));
    assert!(s.contains("mock"));
}
