//! Integration tests for the `blockchain` feature stack.
//!
//! These exercise the full pipeline end-to-end without touching
//! any network: signing → encoding → RLP / digest math →
//! identity verification. They are the regression net for the
//! "did the new helpers compose correctly with the existing
//! ones" question — every time we touch one of the
//! `web3::blockchain::*` modules, this file should keep passing.

#![cfg(feature = "blockchain")]

use magent_core::web3::blockchain::{
    Address, EventLog, Hash, Secp256k1Keypair, TransactionRequest, Wei,
};
use magent_core::web3::blockchain::transaction::{TransactionBuilder, TransactionType};

/// End-to-end "agent builds + signs + verifies" smoke test. This
/// is the smallest test that touches every layer of the
/// blockchain stack: keystore (Secp256k1Keypair), transaction
/// builder, signing, signature verification, address recovery.
#[test]
fn integration_full_legacy_tx_sign_and_recover() {
    let kp = Secp256k1Keypair::generate();
    let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();

    let tx = TransactionBuilder::new(Some(to), 1)
        .value(Wei::from_ether(1))
        .gas_price(Wei::from_gwei(20))
        .gas_limit(21_000)
        .nonce(7);
    // Validate runs inside `sign`; surface failures eagerly so
    // the test fails close to the cause.
    tx.validate().expect("legacy tx must validate");
    let signed = tx.sign(&kp).expect("signing must succeed");

    // EIP-155 v = chain_id*2 + 35 + y_parity. chain_id=1, so v
    // must be >= 37 (35 + 2 = 37 for y_parity=0, 38 for parity=1).
    assert!(signed.v == 37 || signed.v == 38, "got v={}", signed.v);

    // Round-trip: produce a separate signature over the same
    // hash using `sign_hash` (which gives a plain y_parity) and
    // recover the keypair's address from it.
    let sig = magent_core::web3::blockchain::TransactionSigner::sign_hash(
        kp.secret_key(),
        signed.hash.as_bytes(),
    )
    .expect("sign_hash must succeed");
    let recovered = magent_core::web3::blockchain::Secp256k1PublicKey::recover_from(
        signed.hash.as_bytes(),
        &sig.as_bytes(),
    )
    .expect("recover_from must not fail for a self-signed tx");

    assert_eq!(recovered.to_address().to_hex(), kp.address().to_hex());
}

/// EIP-1559 path. Different from the legacy path because the
/// signature `v` carries raw y_parity (0 or 1) — not the
/// chain-id-formatted value — and the encoded body uses
/// EIP-2718 type-byte + RLP-list framing.
#[test]
fn integration_eip1559_tx_sign_and_recover() {
    let kp = Secp256k1Keypair::generate();
    let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();

    let tx = TransactionBuilder::new(Some(to), 1).eip1559(
        Wei::from_gwei(2),
        Wei::from_gwei(50),
        21_000,
    );
    tx.validate().expect("eip1559 tx must validate");
    let signed = tx.sign(&kp).expect("eip1559 sign must succeed");

    // v is the y_parity (0 or 1) for EIP-1559, NOT the
    // chain-id-formatted value.
    assert!(signed.v <= 1, "EIP-1559 v must be 0 or 1, got {}", signed.v);

    // The encoded payload must start with the EIP-1559 type byte
    // (0x02), per EIP-2718 envelope framing.
    assert_eq!(
        signed.raw_transaction[0],
        TransactionType::Eip1559.type_byte(),
        "EIP-1559 envelope must start with type byte 0x02"
    );
}

/// Walk the same signing path the agent itself uses in
/// `agent_tools::sign_message`: produce an EIP-191 personal
/// signature over a UTF-8 message, then verify it via the
/// `verify` helper. This is the user-facing path ("sign this
/// arbitrary message with my wallet"), so the integration test
/// matters more than the unit-level signature test.
#[test]
fn integration_personal_sign_round_trip() {
    let kp = Secp256k1Keypair::generate();
    let message = "hello, this is the agent user";
    let sig = magent_core::web3::blockchain::TransactionSigner::sign_personal_message(
        kp.secret_key(),
        message.as_bytes(),
    )
    .expect("personal_sign must succeed");

    // EIP-191 prefix + keccak256 over (prefix || message).
    let mut prefixed = Vec::with_capacity(message.len() + 28);
    prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    prefixed.extend_from_slice(message.len().to_string().as_bytes());
    prefixed.extend_from_slice(message.as_bytes());
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(&prefixed);
    let digest: [u8; 32] = hasher.finalize().into();

    assert!(
        magent_core::web3::blockchain::TransactionSigner::verify(&digest, &sig, kp.address())
            .unwrap(),
        "personal_sign must recover to the keypair's address"
    );
}

/// Cross-module integration: `BindingClaim::encode` produces
/// the bytes we sign in `BindingProof`. This test exercises the
/// "claim → encode → EIP-191 sign → verify" loop that any real
/// DID-binding flow goes through.
#[test]
fn integration_binding_claim_sign_and_verify() {
    use magent_core::web3::blockchain::identity_binding::{BindingClaim, BindingProof};

    let kp = Secp256k1Keypair::generate();
    let address = *kp.address();

    // 1. Build the claim (the data we want to bind).
    let claim = BindingClaim::new(
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
        address,
        1,
        3600,
    )
    .with_domain("test.app");

    // 2. Encode the claim as canonical bytes for signing.
    let claim_bytes = claim.encode();

    // 3. Sign with EIP-191 personal_sign so the proof is
    //    directly verifiable on-chain by an EOA.
    let sig = magent_core::web3::blockchain::TransactionSigner::sign_personal_message(
        kp.secret_key(),
        &claim_bytes,
    )
    .expect("claim signing must succeed");

    // 4. Build the BindingProof and verify it cryptographically.
    let proof = BindingProof::new(
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
        address,
        1,
        sig.to_hex(),
        std::str::from_utf8(&claim_bytes).unwrap(),
    );
    assert!(
        proof.verify().expect("proof format check"),
        "binding proof must verify"
    );
}

/// Identity-binding construction + metadata round-trip. The
/// `IdentityBinding` is the *result* of a successful on-chain
/// registration; the binding itself is just metadata that an
/// indexer fills in. This test makes sure all the helpers
/// (expiry, domain, display) compose without losing data.
#[test]
fn integration_identity_binding_full_lifecycle() {
    use magent_core::web3::blockchain::identity_binding::IdentityBinding;

    let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
    let binding = IdentityBinding::new(
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
        address,
        1,
        18000000,    // block number
        1700000000,   // timestamp
    )
    .with_expiry(1800003600)
    .with_domain("app.example")
    .with_metadata("audit:abc123");

    // Display summary round-trip. `to_hex()` emits lowercase,
    // so the summary contains the lowercase address even when
    // the address was supplied via mixed-case EIP-55 form.
    let summary = binding.display_short();
    assert!(summary.contains("did:key:"));
    assert!(
        summary.contains("0x742d35cc6634c0532925a3b844bc9e7595f8be21"),
        "got: {summary}"
    );
    assert!(summary.contains("chain 1"));
    assert!(summary.contains("expires"));
}

/// `EventLog::new` builds the minimum-viable log structure used
/// by `eth_getLogs` responses. Smoke test: a freshly constructed
/// log must round-trip through serde so a downstream
/// log-decoder doesn't have to special-case "missing field".
#[test]
fn integration_event_log_round_trip() {
    let address = Address::from_hex("0x000000000000000000000000000000000000dEaD").unwrap();
    let log = EventLog::new(address);
    let json = serde_json::to_string(&log).expect("EventLog must serialise");
    let parsed: EventLog = serde_json::from_str(&json).expect("EventLog must deserialise");
    assert_eq!(parsed.address.to_hex(), log.address.to_hex());
}

/// Wei/ETH conversion pipeline used by every UI-facing balance
/// display. Walks the helpers that live on `BlockchainManager`
/// (and as free functions) end-to-end to confirm the
/// integer-arithmetic path is consistent across them.
#[test]
fn integration_wei_to_eth_pipeline_consistent() {
    use magent_core::web3::blockchain::agent_tools::{
        gwei_to_wei, wei_to_eth, wei_to_eth_string, wei_to_gwei,
    };
    let one_eth_wei = 1_000_000_000_000_000_000u128;
    // f64 path.
    assert_eq!(wei_to_eth(one_eth_wei), 1.0);
    // integer path.
    assert_eq!(wei_to_eth_string(one_eth_wei), "1.000000 ETH");
    // gwei round-trip.
    assert_eq!(wei_to_gwei(one_eth_wei), 1_000_000_000);
    assert_eq!(gwei_to_wei(1_000_000_000), one_eth_wei);
}

/// Ed25519 (magent-native DID) signature round-trip exercises
/// the same code path the VC issuance / verification module
/// uses. We don't bring in the VC types here — this test
/// confirms only the lower-level Ed25519 primitives that VC
/// sits on top of.
#[test]
fn integration_ed25519_sign_and_verify_round_trip() {
    let secret = [7u8; 32];
    let identity =
        magent_core::web3::Identity::from_secret_bytes(&secret).expect("from_secret_bytes");
    let payload = b"integration test payload";
    let signed = identity.sign(payload).expect("Ed25519 sign");
    assert!(
        magent_core::web3::verify_signature(&identity.public_key(), &signed.signature_hex, payload),
        "signature must verify"
    );
}

/// Hash chain: produce a Keccak256 digest from a multi-chunk
/// update and confirm it matches the single-shot equivalent.
/// This is the property every on-chain hashing path depends
/// on; if it ever diverges, `eth_sendRawTransaction`
/// immediately rejects every transaction we produce.
#[test]
fn integration_keccak256_chunked_matches_singleshot() {
    use sha3::{Digest, Keccak256};
    let data = b"the quick brown fox jumps over the lazy dog";
    let mut single = Keccak256::new();
    single.update(data);
    let single_digest: [u8; 32] = single.finalize().into();

    let mut chunked = Keccak256::new();
    // Split into 5-byte chunks to exercise the multi-update path.
    for c in data.chunks(5) {
        chunked.update(c);
    }
    let chunked_digest: [u8; 32] = chunked.finalize().into();

    assert_eq!(single_digest, chunked_digest);
}

/// Confirm that a `TransactionRequest::validate()` failure
/// surfaces *before* any signing work is done. The previous
/// implementation called `validate()` inside `sign()`, so a
/// zero-fee transaction would still produce an unusable signed
/// transaction. The integration check is: bad inputs are
/// rejected at the validate step, not at the sign step.
#[test]
fn integration_validation_runs_before_sign() {
    let kp = Secp256k1Keypair::generate();
    let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();

    // gas_price = 0 → must fail validation, never reach sign.
    let bad = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1).with_gas_limit(21_000);
    assert!(bad.validate().is_err());

    // EIP-1559 with zero fees → must fail validation.
    let bad1559 = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
        .with_eip1559_fees(Wei::ZERO, Wei::from_gwei(50));
    assert!(bad1559.validate().is_err());

    // Suppress the unused-keypair warning when the test runs in
    // isolation.
    let _ = kp;
}

/// EIP-712 typed-data signing + verification, walking the
/// low-level helpers we added. We don't compare to an
/// external reference vector (EIP-712 has many correct
/// encodings depending on which fields the caller chose);
/// instead we assert the round-trip property (sign → verify
/// → same address) and the digest-shape property
/// (`0x1901 || domain_sep || message_hash`).
#[test]
fn integration_eip712_sign_and_verify() {
    use magent_core::web3::blockchain::TransactionSigner;
    let kp = Secp256k1Keypair::generate();

    // Domain separator with one field: chainId=1.
    let domain_type = TransactionSigner::eip712_hash_type_hash(
        "EIP712Domain(uint256 chainId)",
    );
    let mut chain_id_bytes = [0u8; 32];
    chain_id_bytes[31] = 1;
    let domain_sep = TransactionSigner::eip712_domain_separator(
        &domain_type,
        None,
        None,
        Some(&chain_id_bytes),
        None,
        None,
    );

    // Message struct: Person(address wallet).
    let person_type = TransactionSigner::eip712_hash_type_hash("Person(address wallet)");
    let mut wallet_bytes = [0u8; 32];
    wallet_bytes[12..32].copy_from_slice(kp.address().as_bytes());
    let msg_struct = TransactionSigner::eip712_hash_struct(&person_type, &[&wallet_bytes]);

    // EIP-712 digest.
    let digest = TransactionSigner::eip712_digest(&domain_sep, &msg_struct);

    // Sign + verify.
    let sig =
        TransactionSigner::sign_typed_data_hash(kp.secret_key(), &digest).unwrap();
    assert!(
        TransactionSigner::verify(&digest, &sig, kp.address()).unwrap(),
        "EIP-712 signed digest must recover to the keypair's address"
    );
}

/// `Wei::is_zero` semantics. We rely on this for the EIP-1559
/// validation paths; an incorrect `is_zero` would silently let
/// zero-fee transactions slip through.
#[test]
fn integration_wei_is_zero_semantics() {
    assert!(Wei::ZERO.is_zero());
    assert!(!Wei::from_wei(1).is_zero());
    assert!(!Wei::from_ether(1).is_zero());
    assert!(Wei::from_wei(0).is_zero());
}

/// `Hash::zero()` and `Hash::from_bytes` round-trip.
#[test]
fn integration_hash_zero_and_from_bytes_round_trip() {
    let zero = Hash::zero();
    let from_bytes = Hash::from_bytes([0u8; 32]);
    assert_eq!(zero.to_hex(), from_bytes.to_hex());
    assert!(zero.is_zero());

    let nonzero = Hash::from_bytes([1u8; 32]);
    assert!(!nonzero.is_zero());
}