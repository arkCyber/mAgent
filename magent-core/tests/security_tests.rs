//! End-to-end tests for the host-side `SecurityManager`.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p magent-core --features std,web3 --test security_tests
//! ```
//!
//! These tests cover the new AES-128-GCM path (gated on `web3`):
//!   * encrypt/decrypt round-trip recovers plaintext
//!   * nonce uniqueness: two encrypts of the same plaintext produce
//!     different ciphertext
//!   * tampering: flipping a ciphertext byte fails authentication
//!   * HMAC tag is deterministic, format-stable, and verifies
//!   * empty-input and boundary-length payloads handled correctly
//!   * `verify_auth_tag` false-positive on wrong tag
//!
//! On `web3`-less builds (default `cargo check`) the test module degrades
//! to the historical XOR placeholder round-trip so the test target still
//! compiles under the lightweight feature set.

#![cfg(feature = "std")]

use magent_core::security::SecurityManager;

#[test]
fn encrypt_then_decrypt_roundtrips_plaintext() {
    let mgr = SecurityManager::new();
    let plaintext: &[u8] = b"aerospace-grade mAgent telemetry payload v1";

    let ciphertext = mgr.encrypt(plaintext).expect("encrypt");
    let decrypted = mgr.decrypt(&ciphertext).expect("decrypt");
    assert_eq!(
        decrypted.as_slice(),
        plaintext,
        "decrypt(encrypt(p)) must equal p"
    );
}

#[test]
fn encrypt_of_same_plaintext_is_nondeterministic() {
    // With real AES-GCM (web3 feature) the per-message nonce ensures the
    // ciphertext differs even for identical plaintexts. With the XOR
    // placeholder the encrypt output is also deterministic per byte, so
    // this test would fail — we skip it there.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let plaintext: &[u8] = b"same payload";
        let a = mgr.encrypt(plaintext).unwrap();
        let b = mgr.encrypt(plaintext).unwrap();
        assert_ne!(
            a, b,
            "AES-GCM nonces must make same-plaintext ciphertexts differ"
        );
    }
}

#[test]
fn decryption_rejects_tampered_ciphertext() {
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let mut ciphertext = mgr.encrypt(b"important message").unwrap();
        // Flip a byte in the body (past the 12-byte nonce header).
        let idx = ciphertext.len() / 2;
        ciphertext[idx] ^= 0x01;
        assert!(
            mgr.decrypt(&ciphertext).is_err(),
            "tampered ciphertext must fail AES-GCM authentication"
        );
    }
    #[cfg(not(feature = "web3"))]
    {
        // XOR placeholder has no authentication — tampering is silent by design.
        // The SecurityManager is explicitly a placeholder on non-web3 builds.
        // This branch is intentionally a no-op so the test compiles in both paths.
    }
}

#[test]
fn auth_tags_verify_when_data_is_unchanged() {
    let mgr = SecurityManager::new();
    let data: &[u8] = b"agent heartbeat payload";
    let tag = mgr.generate_auth_tag(data).expect("tag");
    // HMAC-SHA-256 (web3): truncated to 8 bytes = 16 hex chars.
    // XOR placeholder (no web3): produces 8 hex chars.
    #[cfg(feature = "web3")]
    assert_eq!(
        tag.len(),
        16,
        "HMAC-SHA-256 tag must be exactly 16 hex chars"
    );
    #[cfg(not(feature = "web3"))]
    assert_eq!(
        tag.len(),
        8,
        "XOR-placeholder tag must be exactly 8 hex chars"
    );
    assert!(
        tag.chars().all(|c| c.is_ascii_hexdigit()),
        "auth tag must be hex: {tag:?}"
    );
    assert!(mgr.verify_auth_tag(data, &tag).unwrap(), "tag must verify");
}

#[test]
fn auth_tag_changes_when_data_changes() {
    // Property: distinct inputs must produce distinct tags.
    // Holds for both real HMAC-SHA-256 and the historical simulator hash.
    let mgr = SecurityManager::new();
    let a = mgr.generate_auth_tag(b"alpha").unwrap();
    let b = mgr.generate_auth_tag(b"beta").unwrap();
    assert_ne!(a, b);
}

#[test]
fn auth_tag_is_collision_resistant_on_single_byte_change() {
    // A 1-byte input change must produce a completely different tag
    // (avalanche property). Two inputs that differ in exactly one byte
    // at every position should produce uncorrelated tags.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let baseline = mgr.generate_auth_tag(b"baseline-tag-payload").unwrap();
        for i in 0..b"baseline-tag-payload".len() {
            let mut modified = *b"baseline-tag-payload";
            modified[i] ^= 0xff;
            let other = mgr.generate_auth_tag(&modified).unwrap();
            assert_ne!(baseline, other, "flipping byte {i} must change the tag");
        }
    }
}

#[test]
fn encryption_disabled_passes_through_unchanged() {
    let mut mgr = SecurityManager::new();
    mgr.disable_encryption();
    let data: &[u8] = b"plain pass-through";
    let enc = mgr.encrypt(data).unwrap();
    let dec = mgr.decrypt(&enc).unwrap();
    assert_eq!(enc.as_slice(), data);
    assert_eq!(dec.as_slice(), data);
}

#[test]
fn encryption_mode_round_trip() {
    let mut mgr = SecurityManager::new();
    assert_eq!(
        mgr.encryption_mode(),
        magent_core::security::EncryptionMode::Aes128Ccm
    );
    mgr.set_encryption_mode(magent_core::security::EncryptionMode::Aes256Ccm)
        .unwrap();
    assert_eq!(
        mgr.encryption_mode(),
        magent_core::security::EncryptionMode::Aes256Ccm
    );
    assert!(mgr.is_encryption_enabled());
    mgr.set_security_level(magent_core::security::SecurityLevel::High)
        .unwrap();
    assert_eq!(
        mgr.security_level(),
        magent_core::security::SecurityLevel::High
    );
}

#[test]
fn encrypt_empty_payload_roundtrips() {
    // Empty plaintext is valid for both the AES-GCM path (nonce+tag only = 28 bytes)
    // and the XOR path (empty output). The round-trip must recover an empty
    // plaintext — never panic and never silently lose data.
    let mgr = SecurityManager::new();
    let ct = mgr.encrypt(b"").expect("encrypt empty");
    let pt = mgr.decrypt(&ct).expect("decrypt empty");
    assert!(pt.is_empty(), "decrypt(encrypt(b\"\")) must be empty");
}

#[test]
fn encrypt_max_payload_fits() {
    // 484 bytes plaintext: 484 + 12 (nonce) + 16 (tag) = 512 — exactly fills Vec<u8, 512>.
    // In AES-GCM path (web3 feature) the output is nonce+ciphertext+tag = 512 bytes.
    // In XOR path the output length equals the plaintext length.
    let mgr = SecurityManager::new();
    let payload: Vec<u8> = (0..=u8::MAX).cycle().take(484).collect();
    let ct = mgr.encrypt(&payload);
    assert!(ct.is_ok(), "484-byte payload must fit in Vec<u8, 512>");
    let ct = ct.unwrap();
    #[cfg(feature = "web3")]
    {
        // AES-GCM: nonce(12) + ciphertext(484) + tag(16) = 512 bytes.
        assert_eq!(ct.len(), 512, "AES-GCM output must be exactly 512 bytes");
    }
    #[cfg(not(feature = "web3"))]
    {
        // XOR placeholder: output length equals plaintext length.
        assert_eq!(
            ct.len(),
            484,
            "XOR placeholder output length equals plaintext"
        );
    }
    // Round-trip must recover the original (works for both paths).
    let pt = mgr.decrypt(&ct);
    assert!(pt.is_ok());
    assert_eq!(pt.unwrap().as_slice(), &payload);
}

#[test]
fn decrypt_rejects_too_short_ciphertext() {
    // Any ciphertext shorter than NONCE_LEN(12) + GCM_TAG(16) = 28 bytes
    // is structurally invalid — must fail authentication.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        // Test each boundary: empty, 1 byte, exactly nonce, just below minimum.
        for len in [0usize, 1, 12, 27] {
            let ct: Vec<u8> = (0u8..).take(len).collect();
            let r = mgr.decrypt(&ct);
            assert!(r.is_err(), "decrypt({len}-byte ciphertext) must fail");
        }
    }
    #[cfg(not(feature = "web3"))]
    {
        // XOR path passes through without authentication — no assertion needed.
        let _mgr = SecurityManager::new();
    }
}

#[test]
fn verify_auth_tag_rejects_wrong_tag() {
    let mgr = SecurityManager::new();
    let data = b"critical heartbeat";
    let wrong_tag = "deadbeefcafebabe0000";
    let result = mgr.verify_auth_tag(data, wrong_tag).unwrap();
    assert!(!result, "verify_auth_tag must return false for wrong tag");
}

#[test]
fn encrypt_overflows_cleanly_above_buffer() {
    // 485 bytes plaintext: 12 + 485 + 16 = 513 bytes, overflows Vec<u8, 512>.
    // The contract is BufferOverflow, NOT silent truncation. Without this
    // assertion a regression to `let _ = push()` would still let the
    // round-trip succeed (with a confusing AuthenticationFailed error)
    // because the truncated tag would mismatch.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(485).collect();
        let result = mgr.encrypt(&payload);
        assert!(
            matches!(
                result,
                Err(magent_core::error::AgentError::BufferOverflow { .. })
            ),
            "485-byte plaintext must overflow Vec<u8, 512>, got {:?}",
            result.map(|_| "Ok")
        );
    }
}

// ---------------------------------------------------------------------------
// New tests covering the timing-attack-safe HMAC verification and the
// explicit plaintext capacity bound. These pins prevent regressions to
// the previously-quiet `==` comparison and the push-loop truncation.
// ---------------------------------------------------------------------------

#[test]
fn verify_auth_tag_uses_constant_time_comparison() {
    // Smoke-test: a single-bit difference at the start vs the end of the
    // tag must both return `false`. We can't reliably measure timing on
    // CI, but we can pin the contract that *both* forms of mismatch
    // produce `false` and don't error out.
    let mgr = SecurityManager::new();
    let data = b"agent heartbeat frame v2";
    let real_tag = mgr.generate_auth_tag(data).unwrap();
    let real = real_tag.as_str();

    // Flip the first byte of the tag — early mismatch.
    let mut early_bytes: [u8; 32] = [0u8; 32];
    early_bytes[..real.len()].copy_from_slice(real.as_bytes());
    early_bytes[0] = early_bytes[0].wrapping_add(1);
    let early_mismatch = core::str::from_utf8(&early_bytes[..real.len()]).unwrap();
    assert!(!mgr.verify_auth_tag(data, early_mismatch).unwrap());

    // Flip the last byte of the tag — late mismatch.
    let mut late_bytes: [u8; 32] = [0u8; 32];
    late_bytes[..real.len()].copy_from_slice(real.as_bytes());
    let last = real.len() - 1;
    late_bytes[last] = late_bytes[last].wrapping_add(1);
    let late_mismatch = core::str::from_utf8(&late_bytes[..real.len()]).unwrap();
    assert!(!mgr.verify_auth_tag(data, late_mismatch).unwrap());

    // And the right tag still verifies.
    assert!(mgr.verify_auth_tag(data, real).unwrap());
}

#[test]
fn verify_auth_tag_length_mismatch_returns_false() {
    // A different-length tag must also return false (without panicking
    // or running the inner loop). This is the constant-time-eq short
    // circuit.
    let mgr = SecurityManager::new();
    let data = b"data";
    let tag = mgr.generate_auth_tag(data).unwrap();
    let truncated = &tag.as_str()[..tag.len() - 1];
    let extended = format!("{tag}00");
    assert!(!mgr.verify_auth_tag(data, truncated).unwrap());
    assert!(!mgr.verify_auth_tag(data, &extended).unwrap());
}

#[test]
fn encrypt_rejects_plaintext_exactly_one_byte_over_limit() {
    // 485-byte plaintext is exactly 1 byte over the 484-byte limit
    // (12 nonce + 484 ct + 16 tag = 512). The early capacity check must
    // refuse this *before* running AES-GCM, returning BufferOverflow.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(485).collect();
        let err = mgr
            .encrypt(&payload)
            .expect_err("485-byte payload must be refused");
        match err {
            magent_core::error::AgentError::BufferOverflow {
                capacity,
                attempted,
            } => {
                assert_eq!(capacity, 512);
                assert_eq!(attempted, 12 + 485 + 16);
            }
            other => panic!("expected BufferOverflow, got {other:?}"),
        }
    }
}

#[test]
fn encrypt_accepts_plaintext_exactly_at_limit() {
    // 484-byte plaintext hits the buffer exactly: 12 + 484 + 16 = 512.
    #[cfg(feature = "web3")]
    {
        let mgr = SecurityManager::new();
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(484).collect();
        let ct = mgr.encrypt(&payload).expect("484-byte payload must fit");
        assert_eq!(ct.len(), 512);
        let pt = mgr.decrypt(&ct).expect("round-trip");
        assert_eq!(pt.as_slice(), &payload[..]);
    }
}
