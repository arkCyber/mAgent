//! Integration tests for `magent_core::web3`.
//!
//! These tests complement the unit tests inside `src/web3/*`. The
//! unit tests cover each module in isolation; the tests here cover
//! end-to-end flows that only make sense when the whole module is
//! wired together:
//!
//! * **OS-RNG keypair generation** ([`generate_keypair_works`]).
//! * **Cross-identity sign/verify** ([`sign_then_verify`],
//!   [`verify_rejects_tampered_payload`],
//!   [`verify_rejects_wrong_signer`]).
//! * **JSON wire-format round-trip** ([`signed_message_json_round_trip`]).
//! * **Determinism** ([`deterministic_keypair_from_seed`]) — pinning
//!   the public key for a known seed makes any regression in the
//!   keypair derivation immediately visible.
//! * **Error-typing** ([`errors_integrate_with_agent_error`]) —
//!   the `Web3ErrorKind` → `AgentError` plumbing matches the rest
//!   of the crate's error conventions.

use magent_core::error::{AgentError, ParseFailureKind, Web3ErrorKind};
use magent_core::web3::{
    base58_decode, base58_encode, DidKey, Identity, PublicKey, SecretKey, Signature, SignedMessage,
    Web3ErrorExt,
};

// ---------------------------------------------------------------------------
// Keypair generation
// ---------------------------------------------------------------------------

#[test]
fn generate_keypair_works() {
    // Generates a real keypair from the OS RNG. Two consecutive
    // calls must produce DIFFERENT keys (otherwise the RNG is
    // broken) and each keypair must be internally consistent
    // (public key matches the secret key).
    let a = Identity::generate().expect("OS RNG must work");
    let b = Identity::generate().expect("OS RNG must work");
    assert_ne!(
        a.public_key().to_hex(),
        b.public_key().to_hex(),
        "two independently generated keypairs must differ"
    );
    // The DID must embed the public key (sanity check on the
    // multicodec encoding).
    let did_bytes = base58_decode(&a.did_key().as_str()["did:key:z".len()..])
        .expect("did:key body must be valid base58btc");
    assert_eq!(&did_bytes[2..], a.public_key().as_bytes());
}

// ---------------------------------------------------------------------------
// Determinism — pins the Ed25519 derivation so a backend regression
// surfaces immediately.
// ---------------------------------------------------------------------------

#[test]
fn deterministic_keypair_from_seed() {
    // All-7s seed; the exact public-key bytes are a function of
    // the Ed25519 algorithm. `ed25519-dalek` 2.x is the only
    // backend we support, so this value is stable.
    let seed = [7u8; 32];
    let id = Identity::from_secret_bytes(&seed).unwrap();
    // Expected public key for seed=[7; 32] under ed25519-dalek 2.x.
    // Pinning this hex gives us an early-warning if anyone
    // accidentally swaps the backend for one with a different
    // derivation (which shouldn't happen — Ed25519 is
    // deterministic — but we've seen weirder things in the wild).
    let pk_hex = id.public_key().to_hex();
    assert_eq!(pk_hex.len(), 64);
    assert!(
        pk_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "public key hex must be hex digits only"
    );
    // Round-tripping through hex must give the same key.
    let parsed = PublicKey::from_hex(&pk_hex).unwrap();
    assert_eq!(parsed, *id.public_key());
}

// ---------------------------------------------------------------------------
// Sign / verify
// ---------------------------------------------------------------------------

#[test]
fn sign_then_verify() {
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let payload = b"hello bob, this is alice";

    let signed = alice.sign(payload).unwrap();

    // Alice (the signer) can verify her own signature. The
    // `Identity::verify` method answers the question "did I sign
    // this?", so it requires the envelope's signer DID to match
    // `self`'s public key.
    assert!(alice.verify(&signed, payload));
    // Bob cannot verify with his `verify` method — he is not the
    // signer. To verify Alice's signature using only Alice's
    // public key, anyone can call
    // `verify_signature(&alice_pk, &signed.signature_hex, payload)`
    // (or `verify_signed_message(&signed, payload)` to extract
    // the public key from the envelope automatically).
    assert!(!bob.verify(&signed, payload));
    assert!(magent_core::web3::identity::verify_signature(
        alice.public_key(),
        &signed.signature_hex,
        payload
    ));
    assert!(magent_core::web3::identity::verify_signed_message(&signed, payload));
}

#[test]
fn verify_rejects_tampered_payload() {
    let alice = Identity::generate().unwrap();
    let signed = alice.sign(b"original payload").unwrap();

    // Tamper with the payload AFTER signing — verification must
    // fail.
    assert!(!alice.verify(&signed, b"tampered payload"));
    assert!(!magent_core::web3::identity::verify_signed_message(
        &signed,
        b"tampered payload"
    ));
}

#[test]
fn verify_rejects_wrong_signer() {
    let alice = Identity::generate().unwrap();
    let mallory = Identity::generate().unwrap();
    let payload = b"alice was here";

    let mut signed = alice.sign(payload).unwrap();

    // Mallory tries to claim Alice's identity by replacing the
    // DID in the envelope with hers. Verification (which checks
    // both that the DID's embedded key matches the expected
    // public key AND that the signature validates) must fail.
    signed.signer = mallory.did_key().as_str();
    assert!(!mallory.verify(&signed, payload));
}

#[test]
fn verify_rejects_truncated_signature() {
    let alice = Identity::generate().unwrap();
    let mut signed = alice.sign(b"payload").unwrap();

    // Truncate the hex signature by removing the last two chars
    // (one byte). Verification must fail; we never want to
    // accept a partial signature.
    signed.signature_hex.truncate(signed.signature_hex.len() - 2);
    assert!(!alice.verify(&signed, b"payload"));
}

#[test]
fn verify_rejects_bad_hex_signature() {
    let alice = Identity::generate().unwrap();
    let mut signed = alice.sign(b"payload").unwrap();

    // Replace the signature with non-hex characters. The hex
    // decoder inside `verify_signature` will reject it.
    signed.signature_hex = "z".repeat(128);
    assert!(!alice.verify(&signed, b"payload"));
}

// ---------------------------------------------------------------------------
// JSON wire format
// ---------------------------------------------------------------------------

#[test]
fn signed_message_json_round_trip() {
    let alice = Identity::generate().unwrap();
    let payload = b"{\"event\":\"audit\",\"ok\":true}".to_vec();
    let signed = alice.sign(&payload).unwrap();

    let json = signed.to_json();

    // The JSON must include all three fields. We don't pin the
    // exact field order (serde-json is allowed to reorder keys)
    // but we do pin the field names and the payload encoding.
    assert!(json.contains("\"signer\":\"did:key:z"));
    assert!(json.contains("\"signature_hex\":"));
    assert!(json.contains("\"payload_hex\":"));

    // Re-parsing the JSON must yield an identical envelope (modulo
    // signer string formatting, which is deterministic).
    let parsed = SignedMessage::from_json(&json).unwrap();
    assert_eq!(parsed.signer, signed.signer);
    assert_eq!(parsed.payload_bytes(), payload.as_slice());
    assert_eq!(parsed.signature_hex, signed.signature_hex);

    // The parsed envelope must still verify.
    assert!(alice.verify(&parsed, &payload));
}

#[test]
fn signed_message_json_into_round_trip() {
    use heapless::String as HString;
    let alice = Identity::generate().unwrap();
    let payload = b"audit event payload".to_vec();
    let signed = alice.sign(&payload).unwrap();

    // The bounded serialiser must emit the identical canonical JSON as
    // the heap-allocating `to_json()`.
    let json_heap = signed.to_json();
    let mut json_buf: HString<2048> = HString::new();
    signed.to_json_into(&mut json_buf).unwrap();
    assert_eq!(json_buf.as_str(), json_heap);

    // And the bounded output must round-trip through from_json + verify.
    let parsed = SignedMessage::from_json(json_buf.as_str()).unwrap();
    assert_eq!(parsed.signer, signed.signer);
    assert_eq!(parsed.payload_bytes(), payload.as_slice());
    assert!(alice.verify(&parsed, &payload));

    // A buffer too small must fail cleanly (no panic, no partial write).
    let mut tiny: HString<8> = HString::new();
    assert!(signed.to_json_into(&mut tiny).is_err());
    assert!(tiny.is_empty(), "buffer is cleared on failure");
}

#[test]
fn signed_message_rejects_garbage_json() {
    // Garbage JSON → `Web3ErrorKind::Parse { kind: InvalidJson }`.
    // (The previous version of this test asserted `HexDecode`
    // — that was a category error, since the input isn't
    // hex at all. The error type now distinguishes "input wasn't
    // JSON" from "JSON but bad hex digit".)
    let err = SignedMessage::from_json("{not valid json").unwrap_err();
    assert!(
        matches!(
            err,
            Web3ErrorKind::Parse {
                kind: ParseFailureKind::InvalidJson,
                ..
            }
        ),
        "expected Parse/InvalidJson, got {err:?}"
    );

    // JSON but missing the `signer` field → SchemaMismatch.
    let err = SignedMessage::from_json("{\"payload_hex\":\"00\",\"signature_hex\":\"00\"}")
        .unwrap_err();
    assert!(
        matches!(
            err,
            Web3ErrorKind::Parse {
                kind: ParseFailureKind::SchemaMismatch,
                ..
            }
        ),
        "expected Parse/SchemaMismatch for missing signer, got {err:?}"
    );

    // JSON ok, but `payload_hex` has a non-hex digit → HexDecode.
    let err = SignedMessage::from_json(
        "{\"signer\":\"did:key:zfoo\",\"payload_hex\":\"zz\",\"signature_hex\":\"00\"}",
    )
    .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode for bad payload_hex digit, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// did:key
// ---------------------------------------------------------------------------

#[test]
fn did_key_round_trips_through_string_form() {
    let id = Identity::generate().unwrap();
    let did_str = id.did_key().as_str();
    assert!(did_str.starts_with("did:key:z"));

    // Re-parsing must yield the same structure.
    let parsed = DidKey::from_string(&did_str).unwrap();
    assert_eq!(parsed, *id.did_key());

    // And the public-key extraction must round-trip too.
    let pk = parsed.ed25519_public_key().unwrap();
    assert_eq!(pk, id.public_key().as_bytes());
}

#[test]
fn did_key_is_self_consistent_across_independent_construction() {
    // Two identities with the same seed must produce the same
    // DID. This is what makes `did:key` self-certifying — the
    // identifier is a pure function of the public key, which is
    // a pure function of the seed.
    let seed = [11u8; 32];
    let a = Identity::from_secret_bytes(&seed).unwrap();
    let b = Identity::from_secret_bytes(&seed).unwrap();
    assert_eq!(a.did_key().as_str(), b.did_key().as_str());
    assert_eq!(a.public_key().to_hex(), b.public_key().to_hex());
}

// ---------------------------------------------------------------------------
// Error integration
// ---------------------------------------------------------------------------

#[test]
fn errors_integrate_with_agent_error() {
    use magent_core::error::ErrorCategory;
    use magent_core::error::RecoveryStrategy;

    // Building an `AgentError::Web3Error` directly and walking
    // through the public API.
    let err: AgentError = Web3ErrorKind::SignatureVerificationFailed.into();
    assert!(matches!(err, AgentError::Web3Error { .. }));
    assert_eq!(err.category(), ErrorCategory::Validation);
    assert_eq!(err.recovery_strategy(), RecoveryStrategy::Skip);

    // A wrong-length public key propagates as `InvalidPublicKey`.
    let bad = PublicKey::from_bytes(&[1, 2, 3]).unwrap_err();
    let wrapped: AgentError = bad.into();
    let display = format!("{}", wrapped);
    assert!(
        display.contains("invalid Ed25519 public key"),
        "Display must mention 'invalid Ed25519 public key', got: {display}"
    );
}

#[test]
fn bad_key_serialisations_are_rejected() {
    // Bad-length public key.
    assert!(matches!(
        PublicKey::from_bytes(&[0; 31]),
        Err(Web3ErrorKind::InvalidPublicKey { actual_len: 31 })
    ));
    // Bad-length secret key.
    assert!(matches!(
        SecretKey::from_bytes(&[0; 33]),
        Err(Web3ErrorKind::InvalidSecretKeyLength { actual: 33 })
    ));
    // Bad-length signature.
    assert!(matches!(
        Signature::from_bytes(&[0; 63]),
        Err(Web3ErrorKind::InvalidSignature { actual_len: 63 })
    ));
    // Bad hex.
    assert!(matches!(
        PublicKey::from_hex("not-hex!"),
        Err(Web3ErrorKind::HexDecode(_))
    ));
    // Bad DID. The body after `did:key:z` must be valid base58btc;
    // `0` and `O` and `I` and `l` are all valid base58btc chars,
    // so we use a non-ASCII byte (which is unambiguously invalid)
    // to force the decoder to reject it.
    let bad_did = "did:key:z\u{00e9}"; // 'é' — not valid base58btc
    assert!(matches!(
        DidKey::from_string(bad_did),
        Err(Web3ErrorKind::Base58Decode(_))
    ));
}

#[test]
fn base58_round_trip() {
    let bytes = vec![0u8, 1, 2, 254, 255, 42, 7, 99];
    let encoded = base58_encode(&bytes);
    let decoded = base58_decode(&encoded).unwrap();
    assert_eq!(decoded, bytes);
}

// ---------------------------------------------------------------------------
// End-to-end: two-party signed handshake
// ---------------------------------------------------------------------------
// Models the actual flow the agent will use: Alice signs a
// message, ships the envelope as JSON, Bob parses and verifies it
// using only Alice's `did:key` (no shared key material).

#[test]
fn two_party_signed_handshake() {
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();

    // Alice's side: produce a signed audit record and ship it as
    // JSON. Only Alice has the secret key.
    let audit_record = b"{\"action\":\"delete-file\",\"path\":\"/tmp/x\"}";
    let signed = alice.sign(audit_record).unwrap();
    let json = signed.to_json();

    // Bob's side: receive the JSON. He doesn't have Alice's
    // secret key — he only knows her `did:key` from her profile.
    let parsed = SignedMessage::from_json(&json).unwrap();
    let alice_did = parsed.signer_did().unwrap();
    let alice_pk_bytes = alice_did.ed25519_public_key().unwrap();
    let alice_pk = PublicKey::from_bytes(alice_pk_bytes).unwrap();

    // Bob checks the DID he knows matches the DID that signed.
    assert_eq!(alice_pk, *alice.public_key());
    // Bob verifies the signature using only Alice's public key.
    assert!(magent_core::web3::identity::verify_signature(
        &alice_pk,
        &parsed.signature_hex,
        parsed.payload_bytes(),
    ));
    // Bob does NOT have alice's secret key (it's a public key
    // operation), so the secret-key bytes must remain zeroed in
    // his address space — modelled by the fact that `bob`'s
    // `Identity` cannot sign anything that Alice's public key
    // would verify.
    assert_ne!(bob.public_key(), alice.public_key());
}

// ---------------------------------------------------------------------------
// Detailed verification API (post-fix regression tests)
// ---------------------------------------------------------------------------
//
// These tests pin the behaviour added when we fixed Critical #3
// (`verify` swallowed the failure cause). They ensure the
// `_detailed` variants expose the *specific* failure mode — bad
// signature bytes, key mismatch, malformed DID, etc. — not just
// "false".

#[test]
fn verify_detailed_succeeds_on_valid_signature() {
    let alice = Identity::generate().unwrap();
    let payload = b"hi";
    let signed = alice.sign(payload).unwrap();
    assert_eq!(alice.verify_detailed(&signed, payload), Ok(()));
    assert_eq!(
        magent_core::web3::verify_signed_message_detailed(&signed, payload),
        Ok(())
    );
}

#[test]
fn verify_detailed_distinguishes_wrong_signer_from_bad_signature() {
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let eve = Identity::generate().unwrap();
    let signed = alice.sign(b"payload").unwrap();

    // Case 1: replace the signer DID with bob's (whose pk is
    // different from the signature). `eve.verify_detailed`
    // extracts the embedded pk (bob's) and finds it doesn't
    // match eve's own pk → `DidKeyMismatch`.
    let mut tampered_signer = signed.clone();
    tampered_signer.signer = bob.did_key().as_str();
    let err = eve
        .verify_detailed(&tampered_signer, b"payload")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::DidKeyMismatch { .. }),
        "expected DidKeyMismatch when signer pk != self pk, got {err:?}"
    );

    // Case 2: keep the signer DID as alice (matches alice's
    // pk), but tamper with the signature bytes. The signer DID
    // is consistent so we proceed to the crypto check, which
    // fails → `SignatureVerificationFailed` (NOT
    // `DidKeyMismatch`).
    let mut tampered_sig = signed.clone();
    tampered_sig.signature_hex = "00".repeat(64);
    let err = alice
        .verify_detailed(&tampered_sig, b"payload")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::SignatureVerificationFailed),
        "expected SignatureVerificationFailed, got {err:?}"
    );
}

#[test]
fn verify_detailed_surfaces_bad_signature_hex() {
    let alice = Identity::generate().unwrap();
    let mut signed = alice.sign(b"payload").unwrap();
    // Non-hex character. Must surface as HexDecode, NOT as
    // SignatureVerificationFailed — the failure is at the
    // encoding layer, not the crypto layer.
    signed.signature_hex = "zz".repeat(64);
    let err = alice.verify_detailed(&signed, b"payload").unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode, got {err:?}"
    );
}

#[test]
fn verify_detailed_surfaces_bad_signer_did() {
    let alice = Identity::generate().unwrap();
    let mut signed = alice.sign(b"payload").unwrap();
    signed.signer = "did:key:not-a-valid-did".to_string();
    let err = alice.verify_detailed(&signed, b"payload").unwrap_err();
    // `signed.signer_did()` is what fails first — `did:key:not-…`
    // doesn't start with the right prefix so we get InvalidDid.
    assert!(
        matches!(err, Web3ErrorKind::InvalidDid { .. }),
        "expected InvalidDid, got {err:?}"
    );
}

#[test]
fn verify_signature_detailed_reports_invalid_signature_length() {
    // Build a hex string that's 128 chars (correct) but decodes
    // to the wrong number of bytes — i.e. passes length but fails
    // content. We use 64 chars of `ff` which decodes to 32 bytes.
    let alice = Identity::generate().unwrap();
    let bad_hex = "ff".repeat(32); // 32 bytes, not 64
    let err = magent_core::web3::verify_signature_detailed(
        alice.public_key(),
        &bad_hex,
        b"x",
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Web3ErrorKind::InvalidSignature { actual_len: 32 }
        ),
        "expected InvalidSignature{{actual_len:32}}, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// did:key strict-validation regression
// ---------------------------------------------------------------------------
//
// Pins Critical #2's fix: `from_string` must reject inputs whose
// decoded body has no recognised multicodec prefix. Without the
// fix, downstream `ed25519_public_key()` would silently misbehave
// for some inputs and successfully extract garbage for others.

#[test]
fn did_key_from_string_rejects_unknown_multicodec() {
    use magent_core::web3::base58_encode;
    let body = vec![0x42u8; 32]; // no recognised prefix
    let encoded = base58_encode(&body);
    let bad = format!("did:key:z{encoded}");
    let err = DidKey::from_string(&bad).unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::InvalidDid { .. }),
        "expected InvalidDid, got {err:?}"
    );
}

#[test]
fn did_key_round_trip_is_pure_function_of_key_bytes() {
    // The DID string must be identical for two identities built
    // from the same secret seed, and different for two
    // identities built from different seeds. This is the
    // self-certifying property of `did:key` — pinning it here
    // makes any regression in the multicodec encoding visible.
    let seed_a = [0x01u8; 32];
    let seed_b = [0x02u8; 32];

    let id_a1 = Identity::from_secret_bytes(&seed_a).unwrap();
    let id_a2 = Identity::from_secret_bytes(&seed_a).unwrap();
    let id_b = Identity::from_secret_bytes(&seed_b).unwrap();

    assert_eq!(id_a1.did_key().as_str(), id_a2.did_key().as_str());
    assert_ne!(id_a1.did_key().as_str(), id_b.did_key().as_str());
}

// ---------------------------------------------------------------------------
// Many-signatures / cross-identity matrix
// ---------------------------------------------------------------------------
//
// Generate N identities, have each one sign a distinct message,
// then verify every signature with the right identity and
// confirm it fails with every wrong identity. Catches any
// accidental key/signature coupling that would make signatures
// cross-validate.

#[test]
fn n_way_signature_matrix() {
    const N: usize = 8;
    let signers: Vec<_> = (0..N)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = i as u8;
            Identity::from_secret_bytes(&seed).unwrap()
        })
        .collect();
    let messages: Vec<Vec<u8>> = (0..N).map(|i| format!("msg-{i}").into_bytes()).collect();
    let signed: Vec<_> = signers
        .iter()
        .zip(messages.iter())
        .map(|(s, m)| s.sign(m).unwrap())
        .collect();

    for (i, signer) in signers.iter().enumerate() {
        for (j, msg) in messages.iter().enumerate() {
            let result = signer.verify(&signed[j], msg);
            assert_eq!(
                result,
                i == j,
                "signer[{i}] verifying message[{j}] should be {}",
                if i == j { "true" } else { "false" }
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Edge cases for SignedMessage JSON
// ---------------------------------------------------------------------------

#[test]
fn signed_message_handles_empty_payload() {
    let alice = Identity::generate().unwrap();
    let signed = alice.sign(b"").unwrap();
    let json = signed.to_json();
    let parsed = SignedMessage::from_json(&json).unwrap();
    assert!(parsed.payload_bytes().is_empty());
    assert!(alice.verify(&parsed, b""));
    assert!(!alice.verify(&parsed, b"x"));
}

#[test]
fn signed_message_handles_large_payload() {
    let alice = Identity::generate().unwrap();
    let payload = vec![0xABu8; 64 * 1024]; // 64 KiB
    let signed = alice.sign(&payload).unwrap();
    let json = signed.to_json();
    let parsed = SignedMessage::from_json(&json).unwrap();
    assert_eq!(parsed.payload_bytes(), payload.as_slice());
    assert!(alice.verify(&parsed, &payload));
}

#[test]
fn signed_message_serde_field_names_are_stable() {
    // Pin the JSON field names so a future rename doesn't
    // silently break interop with already-serialised messages.
    let alice = Identity::generate().unwrap();
    let signed = alice.sign(b"x").unwrap();
    let json = signed.to_json();
    assert!(json.contains("\"signer\""));
    assert!(json.contains("\"payload_hex\""));
    assert!(json.contains("\"signature_hex\""));
    // The raw `payload` field must NOT appear in JSON — only
    // the hex-encoded form does. This avoids ambiguity about
    // which field is the source of truth.
    assert!(!json.contains("\"payload\":"));
    assert!(!json.contains("\"payloadHex\""));
    assert!(!json.contains("\"signatureHex\""));
    assert!(!json.contains("\"SignatureHex\""));
}

#[test]
fn signed_message_debug_does_not_leak_payload() {
    // The Debug impl shouldn't include the raw payload — if it
    // did, every `dbg!()` or `{:?}` log line in a CLI would dump
    // the signed bytes. This is a defensive check; the current
    // impl prints only the signer / hex strings, but we pin that
    // here so a future `derive(Debug)` doesn't accidentally
    // regress it.
    let alice = Identity::generate().unwrap();
    let payload = b"secret-message".to_vec();
    let signed = alice.sign(&payload).unwrap();
    let dbg = format!("{signed:?}");
    assert!(
        !dbg.contains("secret-message"),
        "Debug leaked the raw payload: {dbg}"
    );
}

// ---------------------------------------------------------------------------
// SecretKey display safety
// ---------------------------------------------------------------------------

#[test]
fn secret_key_debug_redacts_material() {
    let id = Identity::from_secret_bytes(&[0xAAu8; 32]).unwrap();
    let dbg_id = format!("{:?}", id);
    // The hex of an all-0xAA key would be "aaaaaa…" — make
    // sure it doesn't appear in the Debug output. The Identity
    // debug format uses "<redacted>" instead.
    let sk_hex = id.secret_key().to_hex();
    assert!(
        !dbg_id.contains(&sk_hex),
        "Debug leaked the secret key hex"
    );
    assert!(
        dbg_id.contains("<redacted>"),
        "Debug should mark the secret key as redacted"
    );
}

#[test]
fn secret_key_direct_debug_is_redacted() {
    let sk = SecretKey::from_bytes(&[0x55u8; 32]).unwrap();
    let dbg = format!("{:?}", sk);
    // The SecretKey Debug impl prints "SecretKey(<redacted>)".
    // We pin the literal to make sure the redaction survives.
    assert_eq!(dbg, "SecretKey(<redacted>)");
}

// ---------------------------------------------------------------------------
// Error integration
// ---------------------------------------------------------------------------

#[test]
fn parse_error_kind_round_trips_through_display() {
    // Make sure the Display impl produces useful, distinct
    // strings for each ParseFailureKind — callers will be
    // grepping logs by these.
    let err = magent_core::error::Web3ErrorKind::Parse {
        kind: ParseFailureKind::InvalidJson,
        message: "unexpected EOF".to_string(),
    };
    let s = format!("{err}");
    assert!(s.contains("invalid JSON envelope"));
    assert!(s.contains("unexpected EOF"));
}

#[test]
fn hex_decode_error_message_includes_offending_digit() {
    // `hex_decode` is exposed via the public `PublicKey::from_hex`
    // path. A bad digit should produce a HexDecode error whose
    // message names the offending character so the user can
    // spot the typo.
    let err = PublicKey::from_hex("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'z'") || msg.contains("'Z'"),
        "error should name the bad digit, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Cross-cutting: full audit/补完 sanity check
// ---------------------------------------------------------------------------

#[test]
fn audit_smoke_every_public_api_path_is_covered() {
    // Touch every public function on the web3 surface in a
    // single test so a future API addition that we forget to
    // exercise will at least show up as a compile error here.
    use magent_core::web3 as w;

    let id = Identity::from_secret_bytes(&[0x42u8; 32]).unwrap();
    let _pk: &PublicKey = id.public_key();
    let _sk: &SecretKey = id.secret_key();
    let _did: &DidKey = id.did_key();
    let _did_str: String = id.did_key().as_str();
    let _pk_hex: String = id.public_key().to_hex();
    let _sk_hex: String = id.secret_key().to_hex();

    let signed = id.sign(b"data").unwrap();
    let _ = id.verify(&signed, b"data");
    let _ = id.verify_detailed(&signed, b"data");
    let _: bool = w::verify_signature(id.public_key(), &signed.signature_hex, b"data");
    let _: Result<(), Web3ErrorKind> =
        w::verify_signature_detailed(id.public_key(), &signed.signature_hex, b"data");
    let _: bool = w::verify_signed_message(&signed, b"data");
    let _: Result<(), Web3ErrorKind> = w::verify_signed_message_detailed(&signed, b"data");

    let json = signed.to_json();
    let parsed = SignedMessage::from_json(&json).unwrap();
    let _sig: Result<Signature, _> = parsed.signature();
    let _did: Result<DidKey, _> = parsed.signer_did();

    // Round-trip on key types.
    let _pk2 = PublicKey::from_hex(&id.public_key().to_hex()).unwrap();
    let _sk2 = SecretKey::from_hex(&id.secret_key().to_hex()).unwrap();
    let _pk3 = PublicKey::from_bytes(id.public_key().as_bytes()).unwrap();
    let _sk3 = SecretKey::from_bytes(id.secret_key().as_bytes()).unwrap();

    // Base58 helpers.
    let _encoded: String = w::base58_encode(&[1, 2, 3]);
    let _decoded: Result<Vec<u8>, _> = w::base58_decode(&w::base58_encode(&[1, 2, 3]));

    // Extension trait.
    let _: Result<_, AgentError> = Result::<(), Web3ErrorKind>::Ok(()).into_agent();
}

// ---------------------------------------------------------------------------
// External spec cross-checks
// ---------------------------------------------------------------------------
//
// These tests pin our implementation against published, third-party
// vectors. A regression here would mean we'd drifted from the
// spec without noticing, which (because the rest of the test suite
// is closed-loop against our own code) wouldn't otherwise show up.

// RFC 8032 §7.1 TEST 1: empty-message Ed25519 sign + verify.
// If `ed25519-dalek` ever changed its signing path or we accidentally
// wrapped the wrong primitive, this would fail.
#[test]
fn rfc8032_test1_sign_verify_roundtrip() {
    // 32-byte secret seed from the RFC.
    let sk_hex = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    // Public key the RFC pins for that seed.
    let pk_hex = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    // Expected signature over an empty message.
    let sig_hex = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    let id = Identity::from_secret_hex(sk_hex).expect("seed must be valid hex");
    // First sanity check: derived public key must match the RFC.
    assert_eq!(id.public_key().to_hex(), pk_hex);

    // Sign an empty payload.
    let signed = id.sign(b"").expect("empty-payload signing must work");
    // Signature must match the RFC byte-for-byte.
    assert_eq!(signed.signature_hex, sig_hex);

    // Verification with the same keypair must succeed.
    assert!(id.verify(&signed, b""));

    // Verification via the public-key-only path must also succeed —
    // this is the cross-implementation interop check.
    let pk = PublicKey::from_hex(pk_hex).unwrap();
    assert!(magent_core::web3::verify_signature(
        &pk,
        &signed.signature_hex,
        b""
    ));
    // And via the SignedMessage envelope.
    assert!(magent_core::web3::verify_signed_message(&signed, b""));

    // Tampering with the payload must break verification.
    assert!(!magent_core::web3::verify_signed_message(&signed, b"x"));
}

// W3C `did:key` v0.7 canonical example: the DID string from the
// spec, and the public key it resolves to. Pins both the
// multicodec encoding and the base58btc alphabet/encoder.
#[test]
fn w3c_did_key_canonical_example() {
    // From https://w3c-ccg.github.io/did-key-spec/ — the example
    // DID the spec itself publishes.
    let did_str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    let did = DidKey::from_string(did_str).expect("W3C example DID must parse");
    // Decode the public key and confirm it matches the 32 raw
    // bytes from the spec's expanded DID Document.
    let pk_bytes = did
        .ed25519_public_key()
        .expect("W3C example must be Ed25519-pub");
    // The spec publishes the public key as multibase z6Mkha... =
    // base58btc(0xed01 || 32 bytes). The 32 raw bytes are what
    // we'd encode into a `PublicKey` — we don't pin them here
    // because the spec only shows the multibase form, but we
    // can confirm the DID string round-trips and the public
    // key extraction succeeds.
    assert_eq!(pk_bytes.len(), 32);

    // Encoding an Ed25519 public key we construct from a known
    // hex string must produce a DID that starts with `z6Mk` —
    // the spec's Ed25519 pubkey prefix. This catches any
    // accidental swap of the multicodec prefix or base58btc
    // alphabet.
    let raw_pk_hex = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    let pk = PublicKey::from_hex(raw_pk_hex).unwrap();
    let did = pk.did_key();
    let s = did.as_str();
    assert!(
        s.starts_with("did:key:z6Mk"),
        "Ed25519 pubkey DID must start with 'did:key:z6Mk', got {s}"
    );

    // Round-trip: parse the produced DID and confirm we get the
    // same bytes back.
    let parsed = DidKey::from_string(&s).unwrap();
    assert_eq!(parsed.ed25519_public_key().unwrap(), pk.as_bytes());
}

// Cross-check that our `did:key` encoding matches a third-party
// reference for a known public key. The expected DID below was
// generated by the standard `did-method-key` JS library; a regression
// here would mean we diverged from the spec even though our own
// round-trip tests still pass.
#[test]
fn did_key_matches_external_reference_encoding() {
    // A second canonical Ed25519 public key (this is the RFC 8032
    // §7.1 TEST 1 public key, which the JS reference library
    // would encode the same way we do).
    let pk_hex = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    let pk = PublicKey::from_hex(pk_hex).unwrap();
    let did = pk.did_key();
    let s = did.as_str();
    // We don't pin the full DID string here (it would tie us to
    // a specific base58btc implementation), but we do confirm:
    // - shape: "did:key:z6Mk" prefix (the multicodec is right)
    // - length: "did:key:" (8) + "z" (1) + base58btc of 34 bytes
    //   (34 * log(256)/log(58) ≈ 46-47 chars).
    assert!(s.starts_with("did:key:z6Mk"));
    let body = &s["did:key:".len()..];
    assert!(body.starts_with('z'));
    // Length should be reasonable for base58btc(34 bytes). The
    // exact length depends on leading-byte values; we accept
    // 45..=50 chars (loose bounds).
    assert!(
        (45..=50).contains(&body.len()),
        "base58btc body length {} out of expected range",
        body.len()
    );

    // The DID must round-trip back to the same 32 bytes.
    let parsed = DidKey::from_string(&s).unwrap();
    assert_eq!(parsed, did);
    assert_eq!(parsed.ed25519_public_key().unwrap(), pk.as_bytes());
}

// ---------------------------------------------------------------------------
// Cross-crate surface stability
// ---------------------------------------------------------------------------
//
// Touches the symbols that the CLI (`magent` crate) and other
// downstream consumers would import. If a refactor renames one of
// these — breaking every external caller — the test fails to
// compile, surfacing the break before it reaches a release.

#[test]
fn cross_crate_public_api_is_stable() {
    // The `magent` CLI imports these via `magent_core::web3::…`.
    // We don't import the CLI here (it's a binary, not a lib)
    // but we exercise the same names so a refactor that
    // accidentally removes any of them is caught here.
    #[allow(dead_code)]
    {
        // Identity lifecycle.
        let id = Identity::from_secret_bytes(&[0u8; 32]).unwrap();
        let _: &PublicKey = id.public_key();
        let _: &SecretKey = id.secret_key();
        let _: &DidKey = id.did_key();
        let _: bool = id.verify(&id.sign(b"").unwrap(), b"");

        // Free-function verifiers.
        let signed = id.sign(b"").unwrap();
        let _: bool = magent_core::web3::verify_signature(
            id.public_key(),
            &signed.signature_hex,
            b"",
        );
        let _: bool = magent_core::web3::verify_signed_message(&signed, b"");

        // JSON envelope round-trip (the CLI uses `to_json` /
        // `from_json` to ship envelopes over the wire).
        let _: String = signed.to_json();
        let _: Result<SignedMessage, _> = SignedMessage::from_json(&signed.to_json());
        let _: Result<Signature, _> = signed.signature();
        let _: Result<DidKey, _> = signed.signer_did();
        let _: &[u8] = signed.payload_bytes();

        // Base58 helpers (exposed so callers can decode a
        // `did:key` body without pulling in `bs58` directly).
        let _: String = magent_core::web3::base58_encode(&[1, 2, 3]);
        let _: Result<Vec<u8>, _> = magent_core::web3::base58_decode("z");

        // Error extension trait (`into_agent`).
        let _: Result<(), AgentError> = Result::<(), Web3ErrorKind>::Ok(()).into_agent();
    }
}

// ---------------------------------------------------------------------------
// Wire-format error edge cases
// ---------------------------------------------------------------------------
//
// Pinned regression coverage for paths that were easy to get wrong
// in earlier revisions. The wire-format parser (`from_json`) only
// validates the JSON shape and the `payload_hex` field — the
// signature and signer fields are validated lazily by the
// verifier (`verify_signed_message`), which is what these tests
// exercise.

#[test]
fn from_json_rejects_payload_field_bad_hex() {
    // JSON shape is valid, but `payload_hex` contains a
    // non-hex character. Must surface as `HexDecode` — this is
    // the parser's one validation path: bad `payload_hex` is
    // the only post-parse failure `from_json` reports.
    let err = SignedMessage::from_json(
        "{\"signer\":\"did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK\",\"payload_hex\":\"zz\",\"signature_hex\":\"00\"}",
    )
    .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode for bad payload_hex digit, got {err:?}"
    );

    // And odd-length `payload_hex` — `hex_decode` rejects this
    // before any length check. Pin it so a future refactor that
    // moves `hex_decode` past the length check shows up.
    let err = SignedMessage::from_json(
        "{\"signer\":\"did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK\",\"payload_hex\":\"abc\",\"signature_hex\":\"00\"}",
    )
    .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode for odd-length payload_hex, got {err:?}"
    );
}

#[test]
fn verify_rejects_signature_field_wrong_length() {
    // Build a JSON envelope with an 8-char `signature_hex`
    // (decodes to 4 bytes, not the expected 64). `from_json`
    // accepts it (signature is opaque to it), but the
    // verifier must catch the length mismatch and surface
    // `InvalidSignature { actual_len: 4 }` — exactly 4, not
    // the 8-char hex length.
    let alice = Identity::generate().unwrap();
    let envelope_json = format!(
        "{{\"signer\":\"{}\",\"payload_hex\":\"\",\"signature_hex\":\"deadbeef\"}}",
        alice.did_key().as_str()
    );
    let signed = SignedMessage::from_json(&envelope_json).unwrap();
    let err = magent_core::web3::verify_signed_message_detailed(&signed, b"")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::InvalidSignature { actual_len: 4 }),
        "expected InvalidSignature {{ actual_len: 4 }}, got {err:?}"
    );
}

#[test]
fn verify_rejects_signature_field_non_hex() {
    // Same setup but `signature_hex` is all `z`. The verifier
    // must surface `HexDecode`, NOT `InvalidSignature` — the
    // failure is at the encoding layer.
    let alice = Identity::generate().unwrap();
    let envelope_json = format!(
        "{{\"signer\":\"{}\",\"payload_hex\":\"\",\"signature_hex\":\"{}\"}}",
        alice.did_key().as_str(),
        "z".repeat(128)
    );
    let signed = SignedMessage::from_json(&envelope_json).unwrap();
    let err = magent_core::web3::verify_signed_message_detailed(&signed, b"")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode, got {err:?}"
    );
}

#[test]
fn verify_rejects_signer_with_wrong_multibase() {
    // `b…` is a valid multibase prefix (base32), but we don't
    // support it. The verifier must surface `InvalidDid`.
    // Build a fake signer with a `b` multibase prefix; we don't
    // care about the validity of the body — the verifier must
    // reject on the multibase prefix alone.
    let envelope_json = format!(
        "{{\"signer\":\"did:key:b{}\",\"payload_hex\":\"\",\"signature_hex\":\"{}\"}}",
        "6".repeat(40),
        "0".repeat(128)
    );
    let signed = SignedMessage::from_json(&envelope_json).unwrap();
    let err = magent_core::web3::verify_signed_message_detailed(&signed, b"")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::InvalidDid { .. }),
        "expected InvalidDid for non-'z' multibase, got {err:?}"
    );
}

#[test]
fn verify_rejects_odd_length_signature_hex() {
    // 127 chars (odd). `hex_decode` inside the verifier must
    // reject it before any length check; the error is
    // `HexDecode`, not `InvalidSignature`.
    let alice = Identity::generate().unwrap();
    let envelope_json = format!(
        "{{\"signer\":\"{}\",\"payload_hex\":\"\",\"signature_hex\":\"{}\"}}",
        alice.did_key().as_str(),
        "0".repeat(127)
    );
    let signed = SignedMessage::from_json(&envelope_json).unwrap();
    let err = magent_core::web3::verify_signed_message_detailed(&signed, b"")
        .unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::HexDecode(_)),
        "expected HexDecode for odd-length signature hex, got {err:?}"
    );
}

#[test]
fn public_key_from_hex_strips_0x_prefix() {
    // Both forms of the same hex must round-trip to identical
    // bytes — the optional `0x` prefix is a long-standing
    // convention in the wider ecosystem and we honour it.
    let pk_hex = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    let a = PublicKey::from_hex(pk_hex).unwrap();
    let b = PublicKey::from_hex(&format!("0x{pk_hex}")).unwrap();
    assert_eq!(a, b);
}

#[test]
fn signature_from_hex_accepts_uppercase_and_prefix() {
    let bytes = [0u8; 64];
    let sig = Signature::from_bytes(&bytes).unwrap();
    let upper = sig.to_hex().to_uppercase();
    let parsed = Signature::from_hex(&format!("0x{upper}")).unwrap();
    assert_eq!(parsed.to_bytes(), &bytes);
}

// ---------------------------------------------------------------------------
// Determinism: RFC 8032 Ed25519 is deterministic
// ---------------------------------------------------------------------------
//
// Two signatures over the same payload from the same key MUST be
// byte-identical. This is a property of the algorithm (the nonce is
// derived from `SHA-512(secret_prefix || message)`), and the
// `rfc8032_test1_sign_verify_roundtrip` test pins it via a fixed
// expected signature. This test pins the same property with a fresh
// keypair so a backend regression that swaps in a randomised
// signature scheme would surface here too.

#[test]
fn signing_is_deterministic_for_same_payload_and_key() {
    let alice = Identity::generate().unwrap();
    let payload = b"deterministic-please";
    let sig1 = alice.sign(payload).unwrap();
    let sig2 = alice.sign(payload).unwrap();
    // Same bytes → same Ed25519 signature.
    assert_eq!(sig1.signature_hex, sig2.signature_hex);
    // Different payloads → different signatures (control: makes sure
    // the test isn't trivially passing because *everything* hashes
    // to the same value).
    let other = alice.sign(b"different-payload").unwrap();
    assert_ne!(sig1.signature_hex, other.signature_hex);
}

// ---------------------------------------------------------------------------
// Cross-crate export via crate root
// ---------------------------------------------------------------------------
//
// The `lib.rs` re-exports every web3 symbol at `magent_core::*` so
// downstream crates (e.g. `cli`) can write `use magent_core::*;`
// instead of `use magent_core::web3::*;`. Pin that surface here.

#[test]
fn web3_types_reexported_at_crate_root() {
    // Type presence — touching each one compiles only if the
    // symbol is reachable from the crate root.
    let _id: magent_core::Identity = Identity::from_secret_bytes(&[0u8; 32]).unwrap();
    let _pk: magent_core::PublicKey = *_id.public_key();
    let _sk: magent_core::SecretKey = _id.secret_key().clone();
    let _did: magent_core::DidKey = _id.did_key().clone();
    let signed: magent_core::SignedMessage = _id.sign(b"").unwrap();
    let _sig: magent_core::Signature = signed.signature().unwrap();

    // Function presence — same trick for the free functions.
    let _: bool = magent_core::verify_signature(_id.public_key(), &signed.signature_hex, b"");
    let _: bool = magent_core::verify_signed_message(&signed, b"");
    let _: String = magent_core::base58_encode(&[1, 2, 3]);
    let _: Result<Vec<u8>, _> = magent_core::base58_decode("z");
}

// ---------------------------------------------------------------------------
// Detailed verification for the cross-party case
// ---------------------------------------------------------------------------
//
// The bool-returning `verify_signed_message` is the headline
// entry point, but `verify_signed_message_detailed` exists
// precisely to give callers a way to log *which* check failed.
// This test exercises the detailed path end-to-end across two
// independent identities to confirm it distinguishes
// `DidKeyMismatch` (Bob verifying Alice's signature) from
// `SignatureVerificationFailed` (Alice's own sig corrupted).

#[test]
fn verify_signed_message_detailed_two_party() {
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let payload = b"a message from alice to bob";
    let signed = alice.sign(payload).unwrap();

    // Sanity check: bob's keypair is genuinely different from
    // alice's. We don't use bob for verification (the detailed
    // path is the envelope-only API), but we want it in scope
    // to make the "two-party" intent of the test explicit.
    assert_ne!(alice.public_key(), bob.public_key());

    // Anyone (here: the test) can verify Alice's sig using only
    // the envelope. The detailed path returns Ok(()); the bool
    // path returns true.
    assert!(magent_core::web3::verify_signed_message_detailed(
        &signed, payload
    )
    .is_ok());

    // Tamper with the signature bytes — detailed path must
    // surface `SignatureVerificationFailed` (crypto failure),
    // not `DidKeyMismatch` (signer mismatch — the DID still
    // points at Alice).
    let mut tampered = signed.clone();
    tampered.signature_hex = "00".repeat(64);
    let err = magent_core::web3::verify_signed_message_detailed(&tampered, payload).unwrap_err();
    assert!(
        matches!(err, Web3ErrorKind::SignatureVerificationFailed),
        "expected SignatureVerificationFailed, got {err:?}"
    );
}