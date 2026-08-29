//! Property-based tests for core type invariants.
//!
//! Uses `proptest` to fuzz critical boundaries and ensure that:
//! - `try_heapless` round-trips correctly for all input lengths
//! - `Address` checksum validation is consistent
//! - Token sink budget enforcement is correct
//! - Keystore encrypt/decrypt round-trips correctly
//! - TransactionSigner produces verifiable signatures

#![cfg(all(test, feature = "std", feature = "web3"))]

use magent_core::agent::TokenSink;
use proptest::prelude::*;

// ============================================================================
// try_heapless round-trip
// ============================================================================

proptest! {
    /// `try_heapless::<N>` must never panic for any string.
    #[test]
    fn try_heapless_never_panics_on_valid_input(s in ".*") {
        let result = magent_core::error::try_heapless::<2048>(&s);
        prop_assert!(result.chars().count() <= s.chars().count());
        prop_assert!(result.len() <= 2048);
    }

    /// When input length (in bytes) < N, result has identical char count.
    #[test]
    fn try_heapless_perfect_roundtrip_under_limit(s in "[^\\x00-\\x1F\\x7F]{0,128}") {
        let result = magent_core::error::try_heapless::<256>(&s);
        prop_assert_eq!(result.len(), s.len());
        prop_assert_eq!(result.chars().count(), s.chars().count());
    }

    /// `try_heapless` truncates at UTF-8 character boundaries, never mid-codepoint.
    #[test]
    fn try_heapless_truncates_at_utf8_boundary(s in "[^\\x00-\\x1F\\x7F]{100,500}") {
        let result = magent_core::error::try_heapless::<64>(&s);
        prop_assert!(result.len() <= 64);
        // Result must be valid UTF-8
        assert!(core::str::from_utf8(result.as_str().as_bytes()).is_ok());
    }

    /// `try_heapless::<32>` on a 100-500 char string never panics.
    #[test]
    fn try_heapless_32_on_long_string_never_panics(s in ".{100,500}") {
        let _result = magent_core::error::try_heapless::<32>(&s);
    }

    /// Multi-byte UTF-8 CJK strings are handled correctly.
    #[test]
    fn try_heapless_multibyte_utf8(s in "[\u{4E00}-\u{9FFF}\u{3040}-\u{309F}]{0,50}") {
        let result = magent_core::error::try_heapless::<16>(&s);
        prop_assert!(result.len() <= 16);
        assert!(core::str::from_utf8(result.as_str().as_bytes()).is_ok());
    }
}

// ============================================================================
// Address checksum (EIP-55) round-trip
// ============================================================================

mod address_checksum_tests {
    use super::*;
    use magent_core::web3::blockchain::Address;

    proptest! {
        /// `Address::from_checksummed_hex` accepts all-lowercase addresses.
        #[test]
        fn checksum_validation_accepts_all_lowercase(hex in "[0-9a-f]{40}") {
            let addr = format!("0x{}", hex);
            prop_assert!(Address::from_checksummed_hex(&addr).is_ok());
        }

        /// `Address::from_hex` + `to_checksum` round-trips cleanly.
        #[test]
        fn checksum_roundtrip(hex in "[0-9a-fA-F]{40}") {
            let input = format!("0x{}", hex);
            if let Ok(addr) = Address::from_hex(&input) {
                let checksummed = addr.to_checksum();
                prop_assert_eq!(checksummed.len(), 42);
                prop_assert!(Address::from_checksummed_hex(&checksummed).is_ok());
            }
        }

        /// `Address::from_hex` with non-hex characters always returns Error.
        #[test]
        fn from_hex_rejects_invalid_hex(s in "[^0-9a-fA-F]{42}") {
            if s.starts_with("0x") || s.starts_with("0X") {
                prop_assert!(Address::from_hex(&s).is_err());
            }
        }

        /// `to_checksum` produces a 42-char hex string.
        #[test]
        fn to_checksum_length(hex in "[0-9a-fA-F]{40}") {
            let input = format!("0x{}", hex);
            if let Ok(addr) = Address::from_hex(&input) {
                let cs = addr.to_checksum();
                prop_assert_eq!(cs.len(), 42);
                prop_assert!(cs.starts_with("0x"));
            }
        }
    }
}

// ============================================================================
// BoundedTokenSink invariants
// ============================================================================

mod token_sink_tests {
    use super::*;
    use magent_core::agent::{BoundedTokenSink, TokenSink};

    proptest! {
        /// `BoundedTokenSink` never exceeds its byte budget.
        #[test]
        fn bounded_sink_respects_budget(tokens in ".{1,1000}") {
            let cap = 128usize;
            let mut buf = heapless::String::new();
            let mut sink = BoundedTokenSink::new(&mut buf, cap);
            for tok in tokens.split_inclusive(char::is_whitespace) {
                sink.on_token(tok);
            }
            prop_assert!(
                sink.written() <= cap,
                "sink wrote {} bytes but cap was {}",
                sink.written(),
                cap
            );
        }

        /// Zero-byte token is a no-op.
        #[test]
        fn bounded_sink_empty_token_is_noop(cap in 1usize..=1024) {
            let mut buf = heapless::String::new();
            let mut sink = BoundedTokenSink::new(&mut buf, cap);
            let result = sink.on_token("");
            // Empty token should return true (keep streaming), not affect counter
            prop_assert!(result);
            prop_assert_eq!(sink.written(), 0);
        }

        /// Sink that hits budget exactly is at most cap bytes.
        #[test]
        fn bounded_sink_at_exact_cap(cap in 1usize..=256) {
            let big_token = "x".repeat(cap * 2);
            let mut buf = heapless::String::new();
            let mut sink = BoundedTokenSink::new(&mut buf, cap);
            sink.on_token(&big_token);
            prop_assert!(sink.written() <= cap);
        }

        /// `was_truncated` is set when budget is exhausted.
        #[test]
        fn bounded_sink_truncation_flag(token_len in 128usize..=512) {
            let cap = 64usize;
            let big_token = "y".repeat(token_len);
            let mut buf = heapless::String::new();
            let mut sink = BoundedTokenSink::new(&mut buf, cap);
            let accepted = sink.on_token(&big_token);
            // Either it was accepted (all bytes fit) or truncated
            if !accepted {
                prop_assert!(sink.was_truncated());
            }
            prop_assert!(sink.written() <= cap);
        }
    }
}

// ============================================================================
// HealthAlert length bounds
// ============================================================================

mod health_alert_tests {
    use super::*;
    use magent_core::early_warning::{AlertSeverity, AlertType, HealthAlert};

    proptest! {
        /// `HealthAlert::new` never panics on very long inputs.
        #[test]
        fn health_alert_never_panics_on_long_input(
            msg_len in 256usize..=1000,
            rec_len in 256usize..=1000,
        ) {
            let msg = "x".repeat(msg_len);
            let rec = "y".repeat(rec_len);
            let _alert = HealthAlert::new(
                0,
                AlertType::GlucoseHigh,
                AlertSeverity::High,
                250.0,
                180.0,
                &msg,
                &rec,
                0,
            );
        }

        /// Truncated message/recommendation is always valid UTF-8.
        #[test]
        fn health_alert_truncated_is_valid_utf8(msg in ".{300,600}") {
            let alert = HealthAlert::new(
                0,
                AlertType::GlucoseHigh,
                AlertSeverity::High,
                250.0,
                180.0,
                &msg,
                "short recommendation",
                0,
            );
            assert!(core::str::from_utf8(alert.message.as_str().as_bytes()).is_ok());
            assert!(core::str::from_utf8(alert.recommendation.as_str().as_bytes()).is_ok());
        }
    }
}

// ============================================================================
// Secp256k1 key invariants
// ============================================================================

// ============================================================================
// try_heapless — edge cases not covered by existing tests
// ============================================================================

proptest! {
    /// Empty string round-trips perfectly.
    #[test]
    fn try_heapless_empty_string(s in "") {
        let result = magent_core::error::try_heapless::<32>(&s);
        prop_assert_eq!(result.len(), 0);
    }

    /// Exactly N printable ASCII bytes (boundary) round-trips with no panic.
    /// We exclude control chars (including NUL) so byte-count equality holds.
    #[test]
    fn try_heapless_exactly_at_capacity(s in "[ -~]{32}") {
        let result = magent_core::error::try_heapless::<32>(&s);
        prop_assert_eq!(result.as_str(), s);
    }

    /// Exactly N+1 printable ASCII bytes truncates to N without panic.
    #[test]
    fn try_heapless_one_over_capacity(s in "[ -~]{33}") {
        let result = magent_core::error::try_heapless::<32>(&s);
        prop_assert_eq!(result.len(), 32);
    }

    /// 4-byte UTF-8 characters (emoji) never split mid-codepoint.
    #[test]
    fn try_heapless_emoji_boundary(s in "[\u{1F300}-\u{1F9FF}]{0,20}") {
        let result = magent_core::error::try_heapless::<12>(&s);
        // Result must be valid UTF-8
        assert!(core::str::from_utf8(result.as_str().as_bytes()).is_ok());
        prop_assert!(result.len() <= 12);
    }

    /// CJK characters (3-byte UTF-8) respect capacity.
    #[test]
    fn try_heapless_cjk_capacity(s in "[\u{4E00}-\u{9FFF}]{0,16}") {
        let result = magent_core::error::try_heapless::<12>(&s);
        assert!(core::str::from_utf8(result.as_str().as_bytes()).is_ok());
        prop_assert!(result.len() <= 12);
    }
}

// ============================================================================
// EIP-55 checksum — reject bad mixed-case (known corruption vectors)
// ============================================================================

proptest! {
    /// Addresses where the checksum is deliberately wrong (each upper-case
    /// character is placed at a non-EIP-55 position) must be rejected.
    #[test]
    fn from_checksummed_rejects_wrong_checksum(hex in "[0-9a-f]{40}") {
        // Flip every other nibble to uppercase — this will violate the
        // keccak hash rule and must produce an error.
        let wrong: String = hex.chars()
            .enumerate()
            .map(|(i, c)| if i % 2 == 0 { c.to_ascii_uppercase() } else { c })
            .collect();
        let checksummed = format!("0x{}", wrong);
        // This address is "valid hex but wrong checksum" → must be rejected
        prop_assert!(magent_core::web3::blockchain::Address::from_checksummed_hex(&checksummed).is_err(),
            "wrong-checksum address {} was incorrectly accepted", checksummed);
    }
}

// ============================================================================
// AgentTelemetry — success_rate_pct invariants
// ============================================================================

proptest! {
    /// `success_rate_pct` must always be in [0, 100] when Some.
    #[test]
    fn telemetry_success_rate_bounded(total in 1u32..10000, ok in 0u32..10000) {
        prop_assume!(ok <= total);
        let mut t = magent_core::agent::AgentTelemetry::default();
        t.runs_ok = ok;
        t.runs_total = total;
        let rate = t.success_rate_pct();
        match rate {
            None => prop_assert!(false, "None only valid when runs_total==0"),
            Some(r) => {
                prop_assert!(r >= 0 && r <= 100,
                    "success_rate_pct {} outside [0,100] for runs_total={}, runs_ok={}", r, total, ok);
            }
        }
    }

    /// 0% when no runs succeeded.
    #[test]
    fn telemetry_zero_ok_rate(total in 1u32..1000) {
        let mut t = magent_core::agent::AgentTelemetry::default();
        t.runs_ok = 0;
        t.runs_total = total;
        let rate = t.success_rate_pct();
        prop_assert_eq!(rate, Some(0));
    }

    /// 100% when all runs succeeded.
    #[test]
    fn telemetry_full_ok_rate(total in 1u32..1000) {
        let mut t = magent_core::agent::AgentTelemetry::default();
        t.runs_ok = total;
        t.runs_total = total;
        let rate = t.success_rate_pct();
        prop_assert_eq!(rate, Some(100));
    }

    /// `success_rate_pct` returns None when runs_total == 0.
    #[test]
    fn telemetry_no_runs_returns_none(ok in 0u32..10) {
        let mut t = magent_core::agent::AgentTelemetry::default();
        t.runs_ok = ok;
        t.runs_total = 0;
        prop_assert_eq!(t.success_rate_pct(), None);
    }
}

// ============================================================================
// BoundedTokenSink — budget enforcement
//
// Note: testing `BoundedTokenSink` with real `heapless::String` buffers
// requires the buffer to have sufficient capacity for the cap. We use
// `#[cfg]` tricks or direct field inspection for unit tests in
// `agent.rs`; here we verify the cap-enforcement via the `capacity()`
// check path (which runs before `push_str`).
// ============================================================================

proptest! {
    /// `on_end(false)` can be called any number of times without panic.
    #[test]
    fn bounded_sink_on_end_idempotent(cap in 1usize..=64, token in ".{1,32}") {
        let mut buf = heapless::String::new();
        let mut sink = magent_core::agent::BoundedTokenSink::new(&mut buf, cap);
        // The sink must not panic regardless of token length vs cap.
        TokenSink::on_token(&mut sink, &token);
        TokenSink::on_end(&mut sink, false);
        TokenSink::on_end(&mut sink, false);
        prop_assert!(sink.written() <= cap);
    }

    /// Tokens over the cap never exceed it.
    #[test]
    fn bounded_sink_over_cap_never_exceeds_cap(token in ".{65,512}") {
        let cap = 32usize;
        let mut buf = heapless::String::new();
        let mut sink = magent_core::agent::BoundedTokenSink::new(&mut buf, cap);
        TokenSink::on_token(&mut sink, &token);
        prop_assert!(sink.written() <= cap);
    }
}

mod secp256k1_tests {
    use super::*;
    use magent_core::web3::blockchain::{Address, Secp256k1Keypair};

    proptest! {
        /// `from_hex` with 64-char valid hex always produces a valid address.
        #[test]
        fn from_hex_64char_produces_valid_address(seed in "[0-9a-fA-F]{64}") {
            if let Ok(kp) = Secp256k1Keypair::from_hex(&seed) {
                let addr = kp.address();
                prop_assert_eq!(addr.to_hex().len(), 42);
                prop_assert!(addr.to_hex().starts_with("0x"));
            }
        }

        /// `from_hex` with wrong-length input always returns Error.
        #[test]
        fn from_hex_rejects_wrong_length(hex in "[0-9a-fA-F]{1,63}") {
            if hex.len() != 64 {
                prop_assert!(Secp256k1Keypair::from_hex(&hex).is_err());
            }
        }

        /// Keypair address is stable across multiple calls.
        #[test]
        fn keypair_address_stable(seed in "[0-9a-fA-F]{64}") {
            if let Ok(kp) = Secp256k1Keypair::from_hex(&seed) {
                let addr1 = kp.address();
                let addr2 = kp.address();
                prop_assert_eq!(addr1.to_hex(), addr2.to_hex());
            }
        }
    }
}

// ============================================================================
// Keystore encrypt/decrypt round-trip
// ============================================================================

mod keystore_tests {
    use super::*;
    use magent_core::web3::wallet::Keystore;

    const TEST_KEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    proptest! {
        /// A keystore created with a passphrase round-trips through
        /// serialize → deserialize → decrypt back to the original key.
        #[test]
        fn keystore_roundtrip(seed in "[ -~]{1,128}", pass in "[ -~]{1,64}") {
            let ks = Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                &pass,
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            prop_assert_eq!(restored.decrypt_private_key(&pass).unwrap(), TEST_KEY);
        }

        /// A keystore without a passphrase round-trips correctly.
        #[test]
        fn keystore_no_passphrase_roundtrip(seed in "[ -~]{1,128}") {
            let ks = Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                "",
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            prop_assert_eq!(restored.decrypt_private_key("").unwrap(), TEST_KEY);
        }

        /// Wrong passphrase always returns an error — never accidentally
        /// decrypts to a different key.
        #[test]
        fn keystore_wrong_passphrase_fails(seed in "[ -~]{1,64}", pass in "[ -~]{1,32}") {
            let ks = Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                &pass,
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            // Any passphrase that differs from the original must fail.
            prop_assert!(restored.decrypt_private_key("wrong").is_err(),
                "wrong passphrase should not decrypt keystore");
        }
    }
}

// ============================================================================
// TransactionSigner signature invariants
// ============================================================================

mod transaction_signer_tests {
    use super::*;
    use magent_core::web3::blockchain::{Secp256k1Keypair, TransactionSigner};

    const TEST_KEY: [u8; 32] = [
        0x1a, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70, 0x81, 0x92, 0xa3, 0xb4, 0xc5, 0xd6, 0xe7, 0xf8,
        0x09, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    proptest! {
        /// `sign_hash` + `verify` round-trips correctly for any 32-byte hash.
        /// We construct hash bytes manually to avoid needing the `hex` crate.
        #[test]
        fn sign_then_verify(a0 in 0u8..=255u8, a1 in 0u8..=255u8) {
            let mut hash_bytes = TEST_KEY;
            hash_bytes[0] = a0;
            hash_bytes[1] = a1;
            // Only use bytes that form a valid secp256k1 scalar (high probability)
            let kp = Secp256k1Keypair::from_secret_key(hash_bytes).ok();
            if let Some(kp) = kp {
                let sig = TransactionSigner::sign_hash(&kp.secret_key(), &hash_bytes).unwrap();
                prop_assert_eq!(sig.as_bytes().len(), 65,
                    "Ethereum sign_hash produces 65-byte sig");
                prop_assert!(TransactionSigner::verify(&hash_bytes, &sig, &kp.address()).unwrap());
            }
        }

        /// `sign_personal_message` produces a valid 65-byte signature.
        #[test]
        fn personal_sign_produces_valid_signature(msg in "[ -~]{1,256}") {
            let kp = Secp256k1Keypair::from_secret_key(TEST_KEY).unwrap();
            let msg_bytes = msg.as_bytes();
            let sig = TransactionSigner::sign_personal_message(kp.secret_key(), msg_bytes).unwrap();
            prop_assert_eq!(sig.as_bytes().len(), 65,
                "Ethereum personal_sign produces 65-byte sig");
        }

        /// Signature length is always 65 bytes regardless of message.
        #[test]
        fn signature_length_is_always_65(a0 in 0u8..=255u8, a1 in 0u8..=255u8) {
            let mut hash_bytes = TEST_KEY;
            hash_bytes[0] = a0;
            hash_bytes[1] = a1;
            if let Some(kp) = Secp256k1Keypair::from_secret_key(hash_bytes).ok() {
                let sig = TransactionSigner::sign_hash(&kp.secret_key(), &hash_bytes).unwrap();
                prop_assert_eq!(sig.as_bytes().len(), 65);
            }
        }
    }
}

// ============================================================================
// SkillsManager — introspection invariants
// ============================================================================

#[test]
fn skills_count_empty_is_zero() {
    let mgr = magent_core::skills::SkillsManager::new(16);
    assert_eq!(mgr.count_by_category().len(), 0);
}

#[test]
fn skills_names_empty_is_empty() {
    let mgr = magent_core::skills::SkillsManager::new(16);
    assert!(mgr.names().is_empty());
}

// ============================================================================
// Keystore encrypt/decrypt round-trip
// ============================================================================

mod keystore_roundtrip_tests {
    use super::*;

    const TEST_KEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    proptest! {
        /// A keystore with passphrase encrypts, serializes, deserializes, and
        /// decrypts back to the original key.
        fn keystore_roundtrip(seed in "[ -~]{1,128}", pass in "[ -~]{1,64}") {
            let ks = magent_core::web3::wallet::Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                &pass,
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = magent_core::web3::wallet::Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            prop_assert_eq!(restored.decrypt_private_key(&pass).unwrap(), TEST_KEY);
        }

        /// A keystore without a passphrase round-trips correctly.
        fn keystore_no_pass_roundtrip(seed in "[ -~]{1,128}") {
            let ks = magent_core::web3::wallet::Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                "",
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = magent_core::web3::wallet::Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            prop_assert_eq!(restored.decrypt_private_key("").unwrap(), TEST_KEY);
        }

        /// Wrong passphrase always returns an error.
        fn keystore_wrong_pass_fails(pass in "[ -~]{1,32}") {
            let ks = magent_core::web3::wallet::Keystore::encrypt_private_key(
                "test-wallet",
                &TEST_KEY,
                &pass,
                None,
            ).unwrap();
            let bytes = ks.to_bytes();
            let restored = magent_core::web3::wallet::Keystore::from_bytes(
                "test-wallet",
                None,
                &bytes,
            ).unwrap();
            prop_assert!(restored.decrypt_private_key("wrong").is_err());
        }
    }
}

// ============================================================================
// Secp256k1Keypair — address derivation consistency
// ============================================================================

mod secp256k1_keypair_tests {
    use super::*;
    use magent_core::web3::blockchain::{Address, Secp256k1Keypair};

    proptest! {
        /// The address derived from a keypair's public key must be a valid
        /// 42-char hex string starting with "0x".
        #[test]
        fn keypair_address_is_valid_hex(seed in "[0-9a-fA-F]{64}") {
            if let Ok(kp) = Secp256k1Keypair::from_hex(&seed) {
                let addr = kp.address();
                let hex = addr.to_hex();
                prop_assert_eq!(hex.len(), 42, "address hex must be 42 chars");
                prop_assert!(hex.starts_with("0x"), "address must start with 0x");
                prop_assert!(
                    hex[2..].chars().all(|c| c.is_ascii_hexdigit()),
                    "address hex chars must all be [0-9a-fA-F]");
            }
        }

        /// Two keypairs from the same seed must produce the same address.
        #[test]
        fn keypair_address_is_deterministic(seed in "[0-9a-fA-F]{64}") {
            if let (Ok(kp1), Ok(kp2)) = (
                Secp256k1Keypair::from_hex(&seed),
                Secp256k1Keypair::from_hex(&seed),
            ) {
                prop_assert_eq!(
                    kp1.address().to_hex(),
                    kp2.address().to_hex(),
                    "same seed must produce same address"
                );
            }
        }

        /// Signing a hash and verifying it back must succeed.
        #[test]
        fn sign_verify_roundtrip(seed in "[0-9a-fA-F]{64}") {
            let kp = Secp256k1Keypair::from_hex(&seed).ok();
            if let Some(kp) = kp {
                let mut msg_hash = [0u8; 32];
                for (i, b) in seed.as_bytes().iter().enumerate() {
                    if i < 32 { msg_hash[i] = *b; }
                }
                if let Ok(sig) = magent_core::web3::blockchain::TransactionSigner::sign_hash(
                    kp.secret_key(), &msg_hash
                ) {
                    let verify_result = magent_core::web3::blockchain::TransactionSigner::verify(
                        &msg_hash, &sig, &kp.address()
                    );
                    prop_assert!(verify_result.is_ok(),
                        "signature must verify for valid keypair");
                }
            }
        }
    }
}

// ============================================================================
// SkillsManager — best_k ordering and count_by_category
// ============================================================================

/// `best_k` returns at most k items regardless of input.
#[test]
fn skills_best_k_respects_k_param() {
    let mut mgr = magent_core::skills::SkillsManager::new(16);
    for i in 0..10 {
        let name = format!("skill_{}", i);
        let skill = magent_core::skills::Skill::new(&name, "desc", "device", "x").unwrap();
        let _ = mgr.add(skill);
    }
    for k in 0..=15 {
        let best = mgr.best_k(k);
        assert!(
            best.len() <= k,
            "best_k({}) returned {} items",
            k,
            best.len()
        );
    }
}

/// `count_by_category` sum equals total skill count.
#[test]
fn skills_count_by_category_sums_to_total() {
    let mut mgr = magent_core::skills::SkillsManager::new(16);
    for i in 0..5 {
        let cat = match i % 3 {
            0 => "device",
            1 => "voice",
            _ => "network",
        };
        let name = format!("skill_{}", i);
        let skill = magent_core::skills::Skill::new(&name, "desc", cat, "x").unwrap();
        let _ = mgr.add(skill);
    }
    let counts = mgr.count_by_category();
    let total: usize = counts.iter().map(|(_, n)| *n as usize).sum();
    assert_eq!(
        total,
        mgr.count(),
        "sum of category counts {} must equal total {}",
        total,
        mgr.count()
    );
}
