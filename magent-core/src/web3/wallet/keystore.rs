//! Ethereum Keystore
//!
//! Provides encrypted storage for private keys using **Argon2id** for
//! passphrase → key derivation and **AES-256-GCM** for authenticated
//! encryption. The private key is never stored in the clear.
//!
//! The encrypted blob is self-describing (it carries the KDF salt + cost
//! params + cipher nonce + version), so a `Keystore` can be persisted to
//! NVS / flash / disk and later decrypted with just the passphrase. This
//! is the primitive the `esp32_nvs` wallet store uses so that on-disk
//! keystore JSON is encrypted (and can additionally be sealed with the
//! device-bound key).

#![cfg(feature = "wallet")]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::{OsRng, RngCore};

/// Current keystore format version.
pub const KEYSTORE_VERSION: u32 = 1;

/// Argon2id memory cost (KiB). 1024 = 1 MiB. Raise for stronger
/// protection on devices with RAM to spare; the value is serialised so
/// older blobs keep decrypting.
pub const DEFAULT_MEMORY_KIB: u32 = 1024;
/// Argon2id time cost (iterations).
pub const DEFAULT_TIME_COST: u32 = 2;
/// Argon2id parallelism.
pub const DEFAULT_PARALLELISM: u32 = 1;
/// Argon2id salt length.
pub const SALT_LEN: usize = 16;
/// AES-256-GCM nonce length.
pub const NONCE_LEN: usize = 12;
/// Derived key length (AES-256).
pub const KEY_LEN: usize = 32;
/// Size of a secp256k1 private key we encrypt.
pub const PRIVATE_KEY_LEN: usize = 32;

/// Error type for keystore operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeystoreError {
    /// Wrong passphrase (GCM tag verification failed).
    InvalidPassphrase,
    /// The blob is malformed (bad version / field length).
    Malformed(String),
    /// A cryptographic primitive reported an error.
    Crypto(String),
}

impl core::fmt::Display for KeystoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            KeystoreError::InvalidPassphrase => {
                write!(f, "invalid passphrase (authentication failed)")
            }
            KeystoreError::Malformed(s) => write!(f, "malformed keystore: {s}"),
            KeystoreError::Crypto(s) => write!(f, "cryptographic error: {s}"),
        }
    }
}

/// Derive a 32-byte key from `passphrase` + `salt` via Argon2id.
fn derive_key(
    passphrase: &str,
    salt: &[u8],
    memory_kib: u32,
    time_cost: u32,
    parallelism: u32,
) -> Result<[u8; KEY_LEN], KeystoreError> {
    let params = Params::new(memory_kib, time_cost, parallelism, Some(KEY_LEN))
        .map_err(|e| KeystoreError::Crypto(format!("argon2 params: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; KEY_LEN];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| KeystoreError::Crypto(format!("argon2: {e}")))?;
    Ok(key)
}

/// Keystore metadata (without sensitive data)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeystoreMetadata {
    /// Wallet name
    pub name: String,
    /// Creation timestamp
    pub creation_time: u64,
    /// Ethereum address
    pub address: Option<String>,
}

/// An encrypted private-key keystore.
///
/// The plaintext private key is stored only transiently during
/// [`Keystore::encrypt_private_key`]; thereafter only the AES-256-GCM
/// ciphertext (+tag) is retained.
#[derive(Debug, Clone)]
pub struct Keystore {
    name: String,
    address: Option<String>,
    creation_time: u64,
    version: u32,
    /// Argon2id memory cost (KiB).
    memory_kib: u32,
    /// Argon2id time cost.
    time_cost: u32,
    /// Argon2id parallelism.
    parallelism: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

impl Keystore {
    /// Create a new empty keystore with the given name (no ciphertext).
    pub fn new(name: &str) -> Self {
        Self::new_with_metadata(KeystoreMetadata {
            name: name.to_string(),
            creation_time: 0,
            address: None,
        })
    }

    /// Create a new keystore from metadata, carrying no ciphertext yet.
    pub fn new_with_metadata(metadata: KeystoreMetadata) -> Self {
        Self {
            name: metadata.name,
            address: metadata.address,
            creation_time: metadata.creation_time,
            version: KEYSTORE_VERSION,
            memory_kib: DEFAULT_MEMORY_KIB,
            time_cost: DEFAULT_TIME_COST,
            parallelism: DEFAULT_PARALLELISM,
            salt: Vec::new(),
            nonce: Vec::new(),
            ciphertext: Vec::new(),
        }
    }

    /// Encrypt a 32-byte private key under `passphrase`.
    ///
    /// Returns a keystore that holds only the authenticated ciphertext.
    pub fn encrypt_private_key(
        name: &str,
        private_key: &[u8; PRIVATE_KEY_LEN],
        passphrase: &str,
        address: Option<String>,
    ) -> Result<Self, KeystoreError> {
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce);

        let key = derive_key(
            passphrase,
            &salt,
            DEFAULT_MEMORY_KIB,
            DEFAULT_TIME_COST,
            DEFAULT_PARALLELISM,
        )?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), private_key.as_slice())
            .map_err(|_| KeystoreError::Crypto("AES-256-GCM encryption failed".into()))?;

        Ok(Self {
            name: name.to_string(),
            address,
            creation_time: 0,
            version: KEYSTORE_VERSION,
            memory_kib: DEFAULT_MEMORY_KIB,
            time_cost: DEFAULT_TIME_COST,
            parallelism: DEFAULT_PARALLELISM,
            salt: salt.to_vec(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// Decrypt the private key using `passphrase`.
    ///
    /// Returns [`KeystoreError::InvalidPassphrase`] if the passphrase is
    /// wrong (GCM tag mismatch) — never a partial key.
    pub fn decrypt_private_key(
        &self,
        passphrase: &str,
    ) -> Result<[u8; PRIVATE_KEY_LEN], KeystoreError> {
        if self.salt.len() != SALT_LEN || self.nonce.len() != NONCE_LEN {
            return Err(KeystoreError::Malformed("salt/nonce length".into()));
        }
        let key = derive_key(passphrase, &self.salt, self.memory_kib, self.time_cost, self.parallelism)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&self.nonce), self.ciphertext.as_slice())
            .map_err(|_| KeystoreError::InvalidPassphrase)?;
        let out: [u8; PRIVATE_KEY_LEN] = plaintext
            .try_into()
            .map_err(|_| KeystoreError::Malformed("decrypted length".into()))?;
        Ok(out)
    }

    /// Get metadata (public fields only).
    pub fn metadata(&self) -> KeystoreMetadata {
        KeystoreMetadata {
            name: self.name.clone(),
            creation_time: self.creation_time,
            address: self.address.clone(),
        }
    }

    /// Whether this keystore currently holds an encrypted key.
    pub fn is_sealed(&self) -> bool {
        !self.ciphertext.is_empty()
    }

    /// Serialise the sealed blob to a compact binary form for NVS/flash
    /// persistence. Version-tagged so the format can evolve.
    ///
    /// Layout: `[version:u32][mem:u32][t:u32][p:u32][salt:16][nonce:12][ciphertext:…]`
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 12 + SALT_LEN + NONCE_LEN + self.ciphertext.len());
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.memory_kib.to_be_bytes());
        out.extend_from_slice(&self.time_cost.to_be_bytes());
        out.extend_from_slice(&self.parallelism.to_be_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Reconstruct a keystore from a blob produced by [`Keystore::to_bytes`].
    pub fn from_bytes(
        name: &str,
        address: Option<String>,
        bytes: &[u8],
    ) -> Result<Self, KeystoreError> {
        const FIXED: usize = 4 * 4 + SALT_LEN + NONCE_LEN;
        if bytes.len() < FIXED + 16 {
            return Err(KeystoreError::Malformed("blob too short".into()));
        }
        // Read a big-endian u32 field; length was validated above, so the
        // slice is always in bounds (returns Malformed defensively).
        let read_u32 = |b: &[u8], at: usize| -> Result<u32, KeystoreError> {
            b.get(at..at + 4)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_be_bytes)
                .ok_or_else(|| KeystoreError::Malformed("u32 field out of bounds".into()))
        };
        let version = read_u32(bytes, 0)?;
        let memory_kib = read_u32(bytes, 4)?;
        let time_cost = read_u32(bytes, 8)?;
        let parallelism = read_u32(bytes, 12)?;
        let salt = bytes[16..16 + SALT_LEN].to_vec();
        let nonce = bytes[16 + SALT_LEN..16 + SALT_LEN + NONCE_LEN].to_vec();
        let ciphertext = bytes[16 + SALT_LEN + NONCE_LEN..].to_vec();

        if version != KEYSTORE_VERSION {
            return Err(KeystoreError::Malformed(format!("unsupported version {version}")));
        }
        Ok(Self {
            name: name.to_string(),
            address,
            creation_time: 0,
            version,
            memory_kib,
            time_cost,
            parallelism,
            salt,
            nonce,
            ciphertext,
        })
    }
}

/// Generate a random UUID v4
pub fn generate_uuid() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Hex encode bytes
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Hex decode a string into bytes. Accepts an optional `0x` prefix and
/// both upper/lower-case hex digits. Returns `Err(msg)` on odd length or
/// non-hex characters (never panics, never produces partial output).
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let body = s.strip_prefix("0x").unwrap_or(s);
    if !body.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    let mut out = Vec::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    for i in (0..body.len()).step_by(2) {
        let hi = hex_nibble(bytes[i]).ok_or("invalid hex character")?;
        let lo = hex_nibble(bytes[i + 1]).ok_or("invalid hex character")?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl Keystore {
    /// Serialise the keystore to a lowercase hex string. The binary blob
    /// is opaque and self-describing (version, KDF params, salt, nonce,
    /// ciphertext), so this string can be stored in NVS / JSON / disk and
    /// later restored with [`Keystore::from_hex`] + the passphrase.
    ///
    /// This is the adapter that bridges the firmware's `esp32_nvs` string
    /// store (which persists a `String` via `nvs.set_str`) to the binary
    /// [`Keystore::to_bytes`] format.
    pub fn to_hex(&self) -> String {
        hex_encode(&self.to_bytes())
    }

    /// Reconstruct a keystore from the output of [`Keystore::to_hex`].
    pub fn from_hex(name: &str, address: Option<String>, hex: &str) -> Result<Self, KeystoreError> {
        let bytes = hex_decode(hex).map_err(KeystoreError::Malformed)?;
        Self::from_bytes(name, address, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
        0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,
    ];

    #[test]
    fn encrypt_decrypt_round_trips() {
        let ks =
            Keystore::encrypt_private_key("w", &KEY, "hunter2", Some("0xabc".into())).unwrap();
        assert!(ks.is_sealed());
        let decrypted = ks.decrypt_private_key("hunter2").unwrap();
        assert_eq!(decrypted, KEY);
        assert_eq!(ks.metadata().name, "w");
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "correct", None).unwrap();
        let err = ks.decrypt_private_key("wrong").unwrap_err();
        assert_eq!(err, KeystoreError::InvalidPassphrase);
    }

    #[test]
    fn blob_round_trips_through_bytes() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "pass", Some("0xabc".into())).unwrap();
        let bytes = ks.to_bytes();
        let restored = Keystore::from_bytes("w", Some("0xabc".into()), &bytes).unwrap();
        assert_eq!(restored.decrypt_private_key("pass").unwrap(), KEY);
        // Two keystores of the same key+passphrase use fresh salt/nonce,
        // so the blobs must differ (defeats offline equality attacks).
        let ks2 = Keystore::encrypt_private_key("w", &KEY, "pass", Some("0xabc".into())).unwrap();
        assert_ne!(ks.to_bytes(), ks2.to_bytes());
    }

    #[test]
    fn from_bytes_rejects_short_blob() {
        assert!(matches!(
            Keystore::from_bytes("w", None, &[0u8; 10]),
            Err(KeystoreError::Malformed(_))
        ));
    }

    #[test]
    fn from_bytes_rejects_wrong_version() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "pass", None).unwrap();
        let mut bytes = ks.to_bytes();
        bytes[0] = 0x09; // corrupt version field
        assert!(matches!(
            Keystore::from_bytes("w", None, &bytes),
            Err(KeystoreError::Malformed(_))
        ));
    }

    #[test]
    fn tampered_ciphertext_is_detected() {
        // Flip one bit in the ciphertext: AES-GCM authentication must
        // reject it as a wrong passphrase (never a corrupt key).
        let ks = Keystore::encrypt_private_key("w", &KEY, "pass", None).unwrap();
        let mut bytes = ks.to_bytes();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let restored = Keystore::from_bytes("w", None, &bytes).unwrap();
        let err = restored.decrypt_private_key("pass").unwrap_err();
        assert_eq!(err, KeystoreError::InvalidPassphrase);
    }

    #[test]
    fn tampered_salt_is_detected() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "pass", None).unwrap();
        let mut bytes = ks.to_bytes();
        // Salt lives at bytes[16..32].
        bytes[16] ^= 0x80;
        let restored = Keystore::from_bytes("w", None, &bytes).unwrap();
        // A different salt yields a different key, so decryption fails.
        assert!(matches!(
            restored.decrypt_private_key("pass"),
            Err(KeystoreError::InvalidPassphrase)
        ));
    }

    #[test]
    fn empty_passphrase_is_acceptable() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "", None).unwrap();
        assert_eq!(ks.decrypt_private_key("").unwrap(), KEY);
        // Empty and non-empty passphrases differ.
        let ks2 = Keystore::encrypt_private_key("w", &KEY, "x", None).unwrap();
        assert_ne!(ks.to_bytes(), ks2.to_bytes());
    }

    #[test]
    fn hex_round_trips_keystore() {
        let ks = Keystore::encrypt_private_key("w", &KEY, "pass", Some("0xabc".into())).unwrap();
        let hex = ks.to_hex();
        let restored = Keystore::from_hex("w", Some("0xabc".into()), &hex).unwrap();
        assert_eq!(restored.decrypt_private_key("pass").unwrap(), KEY);
    }

    #[test]
    fn hex_decode_accepts_prefix_and_case() {
        assert_eq!(hex_decode("0x00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert_eq!(hex_decode("00FF10").unwrap(), vec![0x00, 0xff, 0x10]);
        // Empty string decodes to empty.
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
        assert_eq!(hex_decode("0x").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hex_decode_rejects_bad_input() {
        assert!(hex_decode("abc").is_err()); // odd length
        assert!(hex_decode("0xzz").is_err()); // non-hex
        assert!(hex_decode("0x0g").is_err()); // non-hex
    }

    #[test]
    fn hex_encode_matches_expected() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[]), "");
    }
}

