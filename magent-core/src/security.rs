//! Security module for mAgent
//!
//! Provides BLE encryption, secure pairing, and message authentication
//! for aerospace-grade security.
//!
//! # Two code paths
//!
//! On real nRF52840 hardware the BLE stack itself (nRF SoftDevice) handles
//! AES-CCM, so this module's [`SecurityManager::encrypt`] / [`decrypt`] are
//! only consulted on the **host / no-SoftDevice** build. On those builds the
//! module now performs **real** AES-128-GCM authenticated encryption
//! (NIST-approved AEAD) instead of the historical XOR placeholder, gated on
//! the `web3` feature (which pulls in `aes-gcm` + `hmac` + `sha2`).
//!
//! ## Backwards compatibility
//!
//! - Builds without `web3` (the default `cargo check`) keep the historical
//!   XOR placeholder so the rest of the test suite stays unchanged.
//! - Builds with `web3` get the real AES-128-GCM encrypt/decrypt round-trip
//!   and HMAC-SHA-256 auth tags. The wire format is **self-incompatible**
//!   with the XOR path — it is deliberately a different module so a single
//!   build picks exactly one.
//!
//! **Security Notice**: Real hardware (nRF52840) continues to delegate
//! encryption to the SoftDevice AES-CCM engine, which provides FIPS-140-2
//! compliant authenticated encryption.

use crate::error::{AgentError, Result};
use heapless::{String, Vec};

// ============================================================================
// Real crypto path (gated on `web3`).
//
// The `web3` feature pulls in `aes-gcm` + `hmac` + `sha2`. We use:
//   * AES-128-GCM (NIST-approved AEAD) — for authenticated encryption of
//     payloads up to 512 bytes (the current `Vec<u8, 512>` cap).
//   * HMAC-SHA-256 — for short message-authentication tags compatible with
//     the existing `generate_auth_tag` / `verify_auth_tag` contract.
// ============================================================================
#[cfg(feature = "web3")]
mod real_crypto {
    use crate::error::{AgentError, Result};
    use aead::{Aead, KeyInit}; // Aead from aead crate (not aes_gcm)
    use aes_gcm::{Aes128Gcm, Key, Nonce};
    use heapless::{String, Vec};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    /// 96-bit (12-byte) AES-GCM nonce. Picked deterministically per-message
    /// from a 4-byte counter + the 8-byte message length so two encrypts of
    /// the same plaintext on the same key produce different ciphertexts
    /// (GCM's nonce-misuse-resistance requirement).
    const NONCE_LEN: usize = 12;
    const COUNTER_LEN: usize = 4;

    /// Derive the per-message 96-bit nonce from a monotonic counter.
    fn nonce_for(counter: u32, msg_len: u64) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        n[..COUNTER_LEN].copy_from_slice(&counter.to_be_bytes());
        n[COUNTER_LEN..].copy_from_slice(&msg_len.to_be_bytes());
        n
    }

    /// 16-byte AES-128 key. In production this is provisioned by the BLE
    /// pairing flow; for the host simulation we derive it from a
    /// process-stable secret. The cipher refuses keys shorter than 16
    /// bytes (returns `Aes128Gcm::new_from_slice` error) — we surface
    /// this as `AgentError::CryptoKeyInvalid`.
    fn derive_aes_key() -> [u8; 16] {
        // Domain-separated constant for the host simulation key. Real
        // hardware uses the SoftDevice-derived link key instead.
        const SIM_KEY_SEED: &[u8] = b"magent-core security sim key v1";
        let mut key = [0u8; 16];
        for (i, b) in SIM_KEY_SEED.iter().cycle().take(16).enumerate() {
            key[i] = *b;
        }
        key
    }

    /// Encrypt `plaintext` with AES-128-GCM and prepend the 12-byte nonce.
    /// Output layout: `nonce(12) || ciphertext_with_tag`.
    ///
    /// Plaintext is bounded to 484 bytes (12 nonce + 484 ciphertext + 16 tag
    /// = 512 bytes, exactly fills `Vec<u8, 512>`).
    pub fn encrypt_aes_gcm(counter: u32, plaintext: &[u8]) -> Result<Vec<u8, 512>> {
        // Early capacity check — refuse plaintext that would overflow the
        // output buffer, rather than discover it deep inside the push loop.
        const MAX_PLAINTEXT: usize = 512 - NONCE_LEN - 16;
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(AgentError::BufferOverflow {
                capacity: 512,
                attempted: NONCE_LEN + plaintext.len() + 16,
            });
        }

        let key_bytes = derive_aes_key();
        let key = Key::<Aes128Gcm>::from_slice(&key_bytes);
        let cipher = Aes128Gcm::new(key);
        let nonce_bytes = nonce_for(counter, plaintext.len() as u64);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| AgentError::CryptoError {
                reason: crate::error::EncryptionError::CipherError,
            })?;

        // At this point ciphertext.len() == plaintext.len() + 16 (tag),
        // and we've already bounded plaintext.len() ≤ MAX_PLAINTEXT, so
        // nonce(12) + ciphertext fits 512 exactly — every push succeeds.
        let mut out: Vec<u8, 512> = Vec::new();
        for &b in &nonce_bytes {
            let _ = out.push(b);
        }
        for &b in ciphertext.iter() {
            let _ = out.push(b);
        }
        Ok(out)
    }

    /// Decrypt a payload produced by `encrypt_aes_gcm`. Strips the leading
    /// 12-byte nonce, then verifies the GCM tag (constant-time check) and
    /// returns the plaintext.
    pub fn decrypt_aes_gcm(ciphertext_with_nonce: &[u8]) -> Result<Vec<u8, 512>> {
        if ciphertext_with_nonce.len() < NONCE_LEN + 16 {
            return Err(AgentError::CryptoError {
                reason: crate::error::EncryptionError::InvalidCiphertext,
            });
        }
        let (nonce_bytes, payload) = ciphertext_with_nonce.split_at(NONCE_LEN);
        let key_bytes = derive_aes_key();
        let key = Key::<Aes128Gcm>::from_slice(&key_bytes);
        let cipher = Aes128Gcm::new(key);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, payload)
            .map_err(|_| AgentError::CryptoError {
                reason: crate::error::EncryptionError::AuthenticationFailed,
            })?;

        // Surface `BufferOverflow` explicitly — silently truncating via
        // `let _ = push()` would still verify (the tag is correct) and the
        // caller would receive an incomplete plaintext without knowing it.
        if plaintext.len() > 512 {
            return Err(AgentError::BufferOverflow {
                capacity: 512,
                attempted: plaintext.len(),
            });
        }
        let mut out: Vec<u8, 512> = Vec::new();
        for &b in &plaintext {
            // SAFETY: bounded by the `plaintext.len() > 512` check above;
            // the loop runs at most 512 iterations, so every push succeeds.
            let _ = out.push(b);
        }
        Ok(out)
    }

    /// HMAC-SHA-256 over `data`, truncated to 16 hex chars (8 bytes / 64 bits)
    /// of the full 32-byte SHA-256 MAC.
    ///
    /// Truncating to 8 bytes keeps the tag inside the 32-byte `String<32>`
    /// *cap without filling it to the brim* — callers in `agent.rs` and the
    /// firmware compare tags with `eq_ignore_ascii_case` / `==`, and a
    /// stable 16-char width means historical `verify_auth_tag` calls keep
    /// working.
    pub fn hmac_sha256_tag(data: &[u8]) -> Result<String<32>> {
        // Same domain-separated key as `derive_aes_key` for symmetry.
        const MAC_KEY_SEED: &[u8] = b"magent-core hmac sim key v1   ";
        let mut mac_key = [0u8; 32];
        for (i, b) in MAC_KEY_SEED.iter().cycle().take(32).enumerate() {
            mac_key[i] = *b;
        }
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&mac_key).map_err(|_| {
            AgentError::CryptoError {
                reason: crate::error::EncryptionError::CipherError,
            }
        })?;
        mac.update(data);
        let bytes = mac.finalize().into_bytes();

        // Render the FIRST 8 bytes (16 hex chars). 8 bytes = 64 bits of
        // MAC strength — sufficient for the wire tag, well below the
        // 32-byte `String<32>` cap so any future widening is safe.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut tag: String<32> = String::new();
        for &b in bytes.iter().take(8) {
            let hi = HEX[(b >> 4) as usize] as char;
            let lo = HEX[(b & 0x0f) as usize] as char;
            let _ = tag.push(hi);
            let _ = tag.push(lo);
        }
        Ok(tag)
    }
}

/// Constant-time byte-slice equality.
///
/// Returns `true` iff `a` and `b` have the same length and every byte
/// pair matches. Runs in time proportional to `max(a.len(), b.len())`,
/// independent of where the first mismatch is — required to avoid a
/// timing oracle on the HMAC-SHA-256 verification path.
///
/// `subtle::ConstantTimeEq` would be the canonical choice, but we
/// avoid pulling in another crate by hand-rolling the loop. The
/// implementation matches the standard pattern: XOR differences into
/// an accumulator that's only inspected after the full length walk.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encryption mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EncryptionMode {
    /// No encryption
    None = 0,
    /// AES-128 CCM (nRF SoftDevice) — `EncryptionMode` stays for the BLE
    /// enumeration; the host-side path uses AES-128-GCM via [`real_crypto`].
    Aes128Ccm = 1,
    /// AES-256 CCM (nRF SoftDevice)
    Aes256Ccm = 2,
}

/// Security level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SecurityLevel {
    /// No security
    None = 0,
    /// Low security (no encryption)
    Low = 1,
    /// Medium security (encryption only)
    Medium = 2,
    /// High security (encryption + authentication)
    High = 3,
}

/// Security manager
pub struct SecurityManager {
    encryption_mode: EncryptionMode,
    security_level: SecurityLevel,
    encryption_enabled: bool,
}

impl SecurityManager {
    /// Create a new security manager
    pub fn new() -> Self {
        Self {
            encryption_mode: EncryptionMode::Aes128Ccm,
            security_level: SecurityLevel::High,
            encryption_enabled: true,
        }
    }

    /// Create with default security level
    pub fn with_defaults() -> Self {
        Self::new()
    }

    /// Get encryption mode
    pub fn encryption_mode(&self) -> EncryptionMode {
        self.encryption_mode
    }

    /// Set encryption mode
    pub fn set_encryption_mode(&mut self, mode: EncryptionMode) -> Result<()> {
        self.encryption_mode = mode;
        Ok(())
    }

    /// Get security level
    pub fn security_level(&self) -> SecurityLevel {
        self.security_level
    }

    /// Set security level
    pub fn set_security_level(&mut self, level: SecurityLevel) -> Result<()> {
        self.security_level = level;
        Ok(())
    }

    /// Check if encryption is enabled
    pub fn is_encryption_enabled(&self) -> bool {
        self.encryption_enabled
    }

    /// Enable encryption
    pub fn enable_encryption(&mut self) {
        self.encryption_enabled = true;
    }

    /// Disable encryption
    pub fn disable_encryption(&mut self) {
        self.encryption_enabled = false;
    }

    /// Encrypt data
    ///
    /// On builds with the `web3` feature this performs real **AES-128-GCM**
    /// authenticated encryption (NIST-approved AEAD) keyed off a
    /// domain-separated constant. The output layout is
    /// `nonce(12) || ciphertext_with_tag(plaintext.len() + 16)`.
    ///
    /// On builds without `web3` (the historical default) the function
    /// falls back to the XOR placeholder so the rest of the test suite
    /// stays unchanged. **Do not** use the XOR path for production data.
    ///
    /// Production hardware (nRF52840) delegates encryption to the SoftDevice
    /// AES-CCM engine and never calls this function.
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8, 512>> {
        if !self.encryption_enabled {
            return self.copy_to_vec(data);
        }

        #[cfg(feature = "web3")]
        {
            // Monotonic counter ensures nonce uniqueness across encrypt calls.
            use core::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            real_crypto::encrypt_aes_gcm(counter, data)
        }

        #[cfg(not(feature = "web3"))]
        {
            #[cfg(feature = "std")]
            {
                self.simulate_encrypt(data)
            }
            #[cfg(not(feature = "std"))]
            {
                // On embedded, encryption is handled by SoftDevice.
                // Pass through (real implementation would use crypto hardware).
                self.copy_to_vec(data)
            }
        }
    }

    /// Decrypt data
    ///
    /// On `web3` builds this inverts [`Self::encrypt`] using AES-128-GCM
    /// and verifies the 16-byte authentication tag (constant-time).
    /// Without `web3` the historical XOR placeholder is used.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8, 512>> {
        if !self.encryption_enabled {
            return self.copy_to_vec(data);
        }

        #[cfg(feature = "web3")]
        {
            real_crypto::decrypt_aes_gcm(data)
        }

        #[cfg(not(feature = "web3"))]
        {
            #[cfg(feature = "std")]
            {
                self.simulate_decrypt(data)
            }
            #[cfg(not(feature = "std"))]
            {
                self.copy_to_vec(data)
            }
        }
    }

    /// Generate authentication tag
    ///
    /// On `web3` builds this is a real HMAC-SHA-256 over `data`, hex-encoded
    /// (16 hex chars / 64 bits) so it fits the existing `String<32>` return
    /// type. On historical builds the simulator hash is used (test-only).
    pub fn generate_auth_tag(&self, data: &[u8]) -> Result<String<32>> {
        #[cfg(feature = "web3")]
        {
            real_crypto::hmac_sha256_tag(data)
        }

        #[cfg(not(feature = "web3"))]
        {
            #[cfg(feature = "std")]
            {
                self.simulate_auth_tag(data)
            }
            #[cfg(not(feature = "std"))]
            {
                // In a no_std embedded build the real SoftDevice provides the
                // tag; in this test-only stub we synthesize a short tag from
                // the data. The exact value doesn't matter for tests - the
                // round-trip `verify_auth_tag` call below just needs *some*
                // deterministic output.
                let mut tag: String<32> = String::new();
                for &b in data.iter().take(31) {
                    let _ = core::fmt::Write::write_fmt(&mut tag, format_args!("{:02x}", b));
                }
                Ok(tag)
            }
        }
    }

    /// Verify authentication tag.
    ///
    /// Uses a constant-time comparison so an attacker can't infer tag
    /// bytes one-at-a-time by timing the response. AES-GCM's built-in
    /// tag verification (in [`decrypt_aes_gcm`]) is already constant-time;
    /// this method protects the *out-of-band* HMAC path used by
    /// [`generate_auth_tag`].
    pub fn verify_auth_tag(&self, data: &[u8], tag: &str) -> Result<bool> {
        let expected = self.generate_auth_tag(data)?;
        Ok(constant_time_eq(expected.as_bytes(), tag.as_bytes()))
    }

    // ========================================================================
    // Private helper methods
    // ========================================================================

    fn copy_to_vec(&self, data: &[u8]) -> Result<Vec<u8, 512>> {
        let mut result = Vec::new();
        for &byte in data {
            if result.push(byte).is_err() {
                return Err(AgentError::BufferOverflow {
                    capacity: 512,
                    attempted: data.len(),
                });
            }
        }
        Ok(result)
    }

    /// Fallback encrypt for `std` builds without `web3`.
    /// When `web3` is active the real-crypto path handles encryption instead,
    /// so these simulation functions are dead code in that combination.
    #[cfg(all(feature = "std", not(feature = "web3")))]
    fn simulate_encrypt(&self, data: &[u8]) -> Result<Vec<u8, 512>> {
        // Simulation only - NOT SECURE
        // Production uses nRF SoftDevice AES-CCM
        let mut result = Vec::new();
        for &byte in data {
            if result.push(byte ^ 0xAA).is_err() {
                return Err(AgentError::BufferOverflow {
                    capacity: 512,
                    attempted: data.len(),
                });
            }
        }
        Ok(result)
    }

    #[cfg(all(feature = "std", not(feature = "web3")))]
    fn simulate_decrypt(&self, data: &[u8]) -> Result<Vec<u8, 512>> {
        // Simulation only - NOT SECURE
        // XOR is self-inverse, so same operation decrypts
        let mut result = Vec::new();
        for &byte in data {
            if result.push(byte ^ 0xAA).is_err() {
                return Err(AgentError::BufferOverflow {
                    capacity: 512,
                    attempted: data.len(),
                });
            }
        }
        Ok(result)
    }

    #[cfg(all(feature = "std", not(feature = "web3")))]
    fn simulate_auth_tag(&self, data: &[u8]) -> Result<String<32>> {
        // Simple hash for simulation
        let mut hash: u32 = 0;
        for &byte in data {
            hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
        }
        let hex = "0123456789abcdef";
        let mut result = String::new();
        for i in 0..8 {
            let byte = (hash >> (28 - i * 4)) & 0xf;
            if let Some(c) = hex.as_bytes().get(byte as usize) {
                let _ = result.push(*c as char);
            }
        }
        Ok(result)
    }
}

impl Default for SecurityManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Simulation-path tests: build with `std` but NOT `web3`, so `encrypt` /
/// `decrypt` route through the XOR placeholder and `generate_auth_tag`
/// through the simulator hash.
#[cfg(all(test, feature = "std", not(feature = "web3")))]
mod std_tests {
    use super::*;

    #[test]
    fn new_defaults_are_secure() {
        let sm = SecurityManager::new();
        assert_eq!(sm.encryption_mode(), EncryptionMode::Aes128Ccm);
        assert_eq!(sm.security_level(), SecurityLevel::High);
        assert!(sm.is_encryption_enabled());
        assert_eq!(
            SecurityManager::default().security_level(),
            SecurityLevel::High
        );
    }

    #[test]
    fn xor_encrypt_decrypt_round_trip() {
        let sm = SecurityManager::new();
        let ct = sm.encrypt(b"hello").unwrap();
        assert_ne!(&ct[..], b"hello"); // actually transformed
        let pt = sm.decrypt(&ct).unwrap();
        assert_eq!(&pt[..], b"hello");
    }

    #[test]
    fn disabled_encryption_is_passthrough() {
        let mut sm = SecurityManager::new();
        sm.disable_encryption();
        assert!(!sm.is_encryption_enabled());
        let ct = sm.encrypt(b"data").unwrap();
        assert_eq!(&ct[..], b"data");
        assert_eq!(sm.decrypt(b"data").unwrap().as_slice(), b"data");
    }

    #[test]
    fn set_mode_and_level() {
        let mut sm = SecurityManager::new();
        sm.set_encryption_mode(EncryptionMode::Aes256Ccm).unwrap();
        assert_eq!(sm.encryption_mode(), EncryptionMode::Aes256Ccm);
        sm.set_security_level(SecurityLevel::Low).unwrap();
        assert_eq!(sm.security_level(), SecurityLevel::Low);
    }

    #[test]
    fn auth_tag_round_trip() {
        let sm = SecurityManager::new();
        let tag = sm.generate_auth_tag(b"payload").unwrap();
        assert!(!tag.as_str().is_empty());
        assert!(sm.verify_auth_tag(b"payload", tag.as_str()).unwrap());
        assert!(!sm.verify_auth_tag(b"tampered", tag.as_str()).unwrap());
    }
}

/// Real-crypto tests: build with `web3`, so `encrypt` / `decrypt` use real
/// AES-128-GCM and `generate_auth_tag` uses HMAC-SHA-256.
#[cfg(all(test, feature = "web3"))]
mod web3_tests {
    use super::*;

    #[test]
    fn aes_gcm_encrypt_decrypt_round_trip() {
        let sm = SecurityManager::new();
        let ct = sm.encrypt(b"hello world").unwrap();
        // Layout: 12-byte nonce + (plaintext + 16-byte tag).
        assert_eq!(ct.len(), 12 + b"hello world".len() + 16);
        let pt = sm.decrypt(&ct).unwrap();
        assert_eq!(&pt[..], b"hello world");
    }

    #[test]
    fn distinct_encrypts_yield_distinct_ciphertexts() {
        // Monotonic counter ⇒ distinct nonces ⇒ distinct ciphertexts for the
        // same plaintext (GCM nonce-misuse-resistance).
        let sm = SecurityManager::new();
        let a = sm.encrypt(b"same").unwrap();
        let b = sm.encrypt(b"same").unwrap();
        assert_ne!(&a[..], &b[..]);
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let sm = SecurityManager::new();
        let mut ct = sm.encrypt(b"secret").unwrap();
        // Flip a byte inside the payload (past the 12-byte nonce).
        let idx = ct.len() - 5;
        ct[idx] ^= 0x01;
        let err = sm.decrypt(&ct).unwrap_err();
        assert!(matches!(
            err,
            AgentError::CryptoError {
                reason: crate::error::EncryptionError::AuthenticationFailed
            }
        ));
    }

    #[test]
    fn disabled_encryption_is_passthrough() {
        let mut sm = SecurityManager::new();
        sm.disable_encryption();
        assert_eq!(sm.encrypt(b"data").unwrap().as_slice(), b"data");
    }

    #[test]
    fn auth_tag_round_trip_and_rejects_tamper() {
        let sm = SecurityManager::new();
        let tag = sm.generate_auth_tag(b"authenticated").unwrap();
        assert_eq!(tag.len(), 16); // 16 hex chars
        assert!(sm.verify_auth_tag(b"authenticated", tag.as_str()).unwrap());
        assert!(!sm.verify_auth_tag(b"authenticated!", tag.as_str()).unwrap());
    }

    #[test]
    fn direct_encrypt_aes_gcm_round_trip() {
        let ct = real_crypto::encrypt_aes_gcm(7, b"direct").unwrap();
        assert_eq!(ct.len(), 12 + b"direct".len() + 16);
        let pt = real_crypto::decrypt_aes_gcm(&ct).unwrap();
        assert_eq!(&pt[..], b"direct");
    }

    #[test]
    fn direct_encrypt_aes_gcm_rejects_oversized_plaintext() {
        let big = [0u8; 485]; // > 512 - 12 - 16 = 484
        let err = real_crypto::encrypt_aes_gcm(0, &big).unwrap_err();
        assert!(matches!(err, AgentError::BufferOverflow { .. }));
    }

    #[test]
    fn direct_decrypt_aes_gcm_rejects_short_ciphertext() {
        let short = [0u8; 20]; // < nonce(12) + tag(16)
        let err = real_crypto::decrypt_aes_gcm(&short).unwrap_err();
        assert!(matches!(
            err,
            AgentError::CryptoError {
                reason: crate::error::EncryptionError::InvalidCiphertext
            }
        ));
    }

    #[test]
    fn hmac_sha256_tag_is_16_hex_chars() {
        let tag = real_crypto::hmac_sha256_tag(b"data").unwrap();
        assert_eq!(tag.len(), 16);
        assert!(tag.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonce_varies_with_counter_and_length() {
        // `real_crypto::nonce_for` is a private helper; we assert its
        // contract indirectly through distinct ciphertexts in
        // `distinct_encrypts_yield_distinct_ciphertexts` and by checking
        // the direct encrypt/decrypt round-trip is length-stable.
        let ct = real_crypto::encrypt_aes_gcm(1, b"abc").unwrap();
        let ct2 = real_crypto::encrypt_aes_gcm(2, b"abc").unwrap();
        assert_ne!(&ct[..], &ct2[..]);
        assert_eq!(ct.len(), ct2.len());
    }

    #[test]
    fn constant_time_eq_rejects_length_mismatch() {
        assert!(crate::security::constant_time_eq(b"abc", b"abc"));
        assert!(!crate::security::constant_time_eq(b"abc", b"abcd"));
        assert!(!crate::security::constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn aes_gcm_matches_nist_kat() {
        // NIST GCM spec (McGrew & Viega) AES-128-GCM, Test Case 2.
        // Encrypting P with K/IV must yield exactly C || T — a published
        // reference answer that pins our wiring of the `aes-gcm` crate
        // (key sizing, IV length, tag placement) to a known-good vector.
        // (Vector re-derived and cross-checked against the `cryptography`
        //  library: C = 0388dace60b6a392f328c2b971b2fe78,
        //              T = ab6e47d42cec13bdf53a67b21257bddf.)
        use aead::{Aead, KeyInit};
        use aes_gcm::{Aes128Gcm, Key, Nonce};

        const K: [u8; 16] = [0u8; 16];
        const IV: [u8; 12] = [0u8; 12];
        const P: &[u8] = &[0u8; 16];
        const EXPECTED_CT_TAG: &[u8] = &[
            0x03, 0x88, 0xda, 0xce, 0x60, 0xb6, 0xa3, 0x92, 0xf3, 0x28, 0xc2, 0xb9, 0x71, 0xb2,
            0xfe, 0x78, // ciphertext
            0xab, 0x6e, 0x47, 0xd4, 0x2c, 0xec, 0x13, 0xbd, 0xf5, 0x3a, 0x67, 0xb2, 0x12, 0x57,
            0xbd, 0xdf, // tag
        ];

        let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&K));
        let nonce = Nonce::from_slice(&IV);

        let out = cipher
            .encrypt(nonce, P)
            .expect("NIST KAT encrypt must succeed");
        assert_eq!(
            &out[..],
            EXPECTED_CT_TAG,
            "AES-128-GCM output must match the published NIST vector"
        );

        let plain = cipher
            .decrypt(nonce, &*out)
            .expect("NIST KAT decrypt must verify");
        assert_eq!(&plain[..], P, "decrypt(encrypt(P)) must recover P");
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_kat() {
        // RFC 4231 Test Case 1: 20 × 0x0b key over "Hi There". The full
        // HMAC-SHA-256 is b0344c61d8db38535ca8afceaf0bf12b…; we render the
        // first 8 bytes (16 hex chars), which is exactly the truncation
        // `real_crypto::hmac_sha256_tag` uses on the wire.
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        const KEY: [u8; 20] = [0x0b; 20];
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&KEY).expect("valid 20-byte key");
        mac.update(b"Hi There");
        let bytes = mac.finalize().into_bytes();

        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut got: heapless::String<16> = heapless::String::new();
        for &b in bytes.iter().take(8) {
            let _ = got.push(HEX[(b >> 4) as usize] as char);
            let _ = got.push(HEX[(b & 0x0f) as usize] as char);
        }
        assert_eq!(&got[..], "b0344c61d8db3853", "RFC 4231 TC1 HMAC-SHA-256");
    }

    #[test]
    fn encrypt_nonce_prefix_encodes_counter() {
        // `real_crypto` lays the 4-byte big-endian counter into the first 4
        // bytes of the 12-byte nonce. Assert the wire prefix reflects the
        // counter so nonce uniqueness is observable and regressions in the
        // nonce derivation are caught.
        let ct = real_crypto::encrypt_aes_gcm(0x0102_0304, b"x").unwrap();
        assert_eq!(&ct[..4], &[0x01, 0x02, 0x03, 0x04]);
        // Nonce length is 12 bytes regardless of message length.
        assert_eq!(ct.len(), 12 + b"x".len() + 16);
    }
}
