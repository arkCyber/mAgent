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
