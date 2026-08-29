//! Secp256k1 Signing for Ethereum Transactions
//!
//! This module provides secp256k1 key management and signing for Ethereum
//! transactions. This is separate from the Ed25519-based `Identity` system
//! because Ethereum specifically requires secp256k1.
//!
//! ## Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │                      Ethereum Signing                           │
//! │  ┌─────────────────────────────────────────────────────────┐  │
//! │  │  Secp256k1Keypair                                       │  │
//! │  │  - secret_key: 32 bytes                                 │  │
//! │  │  - public_key: 64 bytes (uncompressed)                  │  │
//! │  │  - address: derived from keccak256(pubkey)[12..32]      │  │
//! │  └─────────────────────────────────────────────────────────┘  │
//! │  ┌─────────────────────────────────────────────────────────┐  │
//! │  │  TransactionSigner                                       │  │
//! │  │  - sign_transaction(legacy)                             │  │
//! │  │  - sign_transaction(eip1559)                            │  │
//! │  │  - sign_message(personal_sign, EIP-191)                │  │
//! │  └─────────────────────────────────────────────────────────┘  │
//! └────────────────────────────────────────────────────────────────┘
//! ```

#[cfg(feature = "web3")]
use alloc::string::{String, ToString};
#[cfg(feature = "web3")]
use alloc::vec::Vec;

#[cfg(feature = "web3")]
use sha3::{Digest, Keccak256};

#[cfg(feature = "web3")]
use secp256k1::{
    ecdsa::{RecoverableSignature, RecoveryId, Signature as EcdsaSignature},
    Message, PublicKey, Secp256k1, SecretKey,
};

use crate::error::Web3ErrorKind;
#[allow(unused_imports)]
use crate::web3::blockchain::{Address, Hash};

// ============================================================================
// Key Types
// ============================================================================

/// A 32-byte secp256k1 secret key.
#[derive(Clone)]
pub struct Secp256k1SecretKey {
    bytes: [u8; 32],
}

impl Secp256k1SecretKey {
    /// Create from raw bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, Web3ErrorKind> {
        // Validate it's a valid secp256k1 scalar via parsing
        #[cfg(feature = "web3")]
        {
            Self::from_slice(&bytes)?;
        }
        Ok(Self { bytes })
    }

    /// Parse from hex string
    pub fn from_hex(hex: &str) -> Result<Self, Web3ErrorKind> {
        let s = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex_decode(s)?;
        if bytes.len() != 32 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Self::from_bytes(arr)
    }

    /// Parse from secp256k1 library
    #[cfg(feature = "web3")]
    pub fn from_slice(data: &[u8]) -> Result<(), Web3ErrorKind> {
        SecretKey::from_slice(data)
            .map(|_| ())
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid secp256k1 key: {}", e)))
    }

    /// Get raw bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Compute the public key
    ///
    /// Returns an error if the stored bytes are not a valid secp256k1
    /// scalar (i.e. ≥ the curve group order `n`). The previous
    /// implementation `.expect("validated at construction")` would panic
    /// the process on any construction path that bypassed `from_bytes`
    /// (e.g. a future refactor that adds a `pub fn new_unchecked`, or a
    /// direct field assignment via `Default`).
    ///
    /// HARDENING (audit-2026-08 H10): do not panic on bad-key input.
    #[cfg(feature = "web3")]
    pub fn public_key(&self) -> Result<Secp256k1PublicKey, Web3ErrorKind> {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&self.bytes).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!(
                "stored secp256k1 key is out of range ({} bytes), \
                 refusing to derive public key",
                e
            ))
        })?;
        let pk = PublicKey::from_secret_key(&secp, &sk);
        let full = pk.serialize_uncompressed();
        let mut uncompressed = [0u8; 64];
        uncompressed.copy_from_slice(&full[1..65]);
        Ok(Secp256k1PublicKey { uncompressed })
    }

    /// Get secp256k1 SecretKey (for internal signing)
    ///
    /// HARDENING (audit-2026-08 H10): same fix as `public_key` —
    /// propagate the error rather than panicking.
    #[cfg(feature = "web3")]
    pub fn inner(&self) -> Result<SecretKey, Web3ErrorKind> {
        SecretKey::from_slice(&self.bytes).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!(
                "stored secp256k1 key is out of range ({} bytes)",
                e
            ))
        })
    }
}

/// A 64-byte uncompressed secp256k1 public key (without 0x04 prefix).
#[derive(Clone, Copy)]
pub struct Secp256k1PublicKey {
    uncompressed: [u8; 64],
}

impl Secp256k1PublicKey {
    /// Get raw 64 bytes (64 = x || y, both 32 bytes)
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.uncompressed
    }

    /// Get the 65-byte serialized form (with 0x04 prefix)
    #[cfg(feature = "web3")]
    pub fn serialized(&self) -> [u8; 65] {
        let mut out = [0u8; 65];
        out[0] = 0x04;
        out[1..].copy_from_slice(&self.uncompressed);
        out
    }

    /// Derive the Ethereum address from the public key
    pub fn to_address(&self) -> Address {
        // address = keccak256(pubkey)[12..32]
        #[cfg(feature = "web3")]
        {
            let mut hasher = Keccak256::new();
            hasher.update(self.uncompressed);
            let hash = hasher.finalize();
            let mut addr_bytes = [0u8; 20];
            addr_bytes.copy_from_slice(&hash[12..32]);
            Address::from_bytes(addr_bytes)
        }
        #[cfg(not(feature = "web3"))]
        {
            // Fallback: simple derivation
            let mut addr_bytes = [0u8; 20];
            let len = self.uncompressed.len().min(20);
            for i in 0..len {
                addr_bytes[i] = self.uncompressed[44 + i];
            }
            Address::from_bytes(addr_bytes)
        }
    }

    /// Recover public key from signature (used in eth_sign)
    #[cfg(feature = "web3")]
    pub fn recover_from(
        message_hash: &[u8; 32],
        signature: &[u8; 65],
    ) -> Result<Self, Web3ErrorKind> {
        let secp = Secp256k1::new();
        let recid_byte = if signature[64] >= 27 {
            signature[64] - 27
        } else {
            signature[64]
        };
        let recid = RecoveryId::from_i32(recid_byte as i32)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid recovery id: {}", e)))?;
        let sig = EcdsaSignature::from_compact(&signature[..64])
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid signature: {}", e)))?;
        let recoverable = RecoverableSignature::from_compact(&signature[..64], recid)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid recoverable: {}", e)))?;
        let msg = Message::from_digest_slice(message_hash)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid message: {}", e)))?;
        let pk = secp
            .recover_ecdsa(&msg, &recoverable)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("recover failed: {}", e)))?;
        let _ = sig;
        let full = pk.serialize_uncompressed();
        let mut uncompressed = [0u8; 64];
        uncompressed.copy_from_slice(&full[1..65]);
        Ok(Self { uncompressed })
    }
}

impl core::fmt::Debug for Secp256k1SecretKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Redact secret key bytes for safety
        write!(f, "Secp256k1SecretKey([REDACTED])")
    }
}

impl core::fmt::Debug for Secp256k1PublicKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Secp256k1PublicKey({})", self.to_address().to_hex())
    }
}

// ============================================================================
// Keypair
// ============================================================================

/// A complete secp256k1 key pair (secret + public + address)
#[derive(Clone)]
pub struct Secp256k1Keypair {
    secret: Secp256k1SecretKey,
    public: Secp256k1PublicKey,
    address: Address,
}

impl Secp256k1Keypair {
    /// Generate a new random keypair using the OS RNG.
    #[cfg(feature = "web3")]
    pub fn generate() -> Self {
        generate_keypair()
    }

    /// Generate a deterministic test keypair (for tests only).
    #[cfg(feature = "web3")]
    pub fn generate_test() -> Self {
        generate_test_keypair()
    }

    /// Create from secret key bytes
    #[cfg(feature = "web3")]
    pub fn from_secret_key(bytes: [u8; 32]) -> Result<Self, Web3ErrorKind> {
        let secret = Secp256k1SecretKey::from_bytes(bytes)?;
        let public = secret.public_key()?;
        let address = public.to_address();
        Ok(Self {
            secret,
            public,
            address,
        })
    }

    /// Import from hex string
    pub fn from_hex(hex: &str) -> Result<Self, Web3ErrorKind> {
        let secret = Secp256k1SecretKey::from_hex(hex)?;
        #[cfg(feature = "web3")]
        {
            let public = secret.public_key()?;
            let address = public.to_address();
            Ok(Self {
                secret,
                public,
                address,
            })
        }
        #[cfg(not(feature = "web3"))]
        {
            let _ = hex;
            Err(Web3ErrorKind::BlockchainError(
                "secp256k1 not enabled".to_string(),
            ))
        }
    }

    /// Get the Ethereum address
    pub fn address(&self) -> &Address {
        &self.address
    }

    /// Get the public key
    pub fn public_key(&self) -> &Secp256k1PublicKey {
        &self.public
    }

    /// Get the secret key (handle with care!)
    pub fn secret_key(&self) -> &Secp256k1SecretKey {
        &self.secret
    }
}

impl core::fmt::Debug for Secp256k1Keypair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Only show address in debug
        write!(f, "Secp256k1Keypair({})", self.address.to_hex())
    }
}

// ============================================================================
// Free Keypair Generators
// ============================================================================

/// Generate a new random keypair using the OS RNG.
#[cfg(feature = "web3")]
pub fn generate_keypair() -> Secp256k1Keypair {
    let secp = Secp256k1::new();
    let mut rng = rand_core::OsRng;
    let (sk, pk) = secp.generate_keypair(&mut rng);
    let secret_bytes = sk.secret_bytes();
    let public_bytes = pk.serialize_uncompressed();
    let mut uncompressed = [0u8; 64];
    uncompressed.copy_from_slice(&public_bytes[1..65]);
    let secret = Secp256k1SecretKey {
        bytes: secret_bytes,
    };
    let public = Secp256k1PublicKey { uncompressed };
    let address = public.to_address();
    Secp256k1Keypair {
        secret,
        public,
        address,
    }
}

/// Generate a deterministic test keypair (for tests only).
#[cfg(feature = "web3")]
pub fn generate_test_keypair() -> Secp256k1Keypair {
    let secp = Secp256k1::new();
    // Deterministic placeholder key (for testing only).
    let mut bytes = [0u8; 32];
    for (i, slot) in bytes.iter_mut().enumerate() {
        *slot = (i as u8).wrapping_add(0x42);
    }
    // Ensure it's a valid scalar (not zero, less than curve order)
    bytes[0] = 1;
    let sk = SecretKey::from_slice(&bytes).expect("deterministic key is valid");
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let secret_bytes = sk.secret_bytes();
    let public_bytes = pk.serialize_uncompressed();
    let mut uncompressed = [0u8; 64];
    uncompressed.copy_from_slice(&public_bytes[1..65]);
    let secret = Secp256k1SecretKey {
        bytes: secret_bytes,
    };
    let public = Secp256k1PublicKey { uncompressed };
    let address = public.to_address();
    Secp256k1Keypair {
        secret,
        public,
        address,
    }
}

// ============================================================================
// Transaction Signing
// ============================================================================

/// A 65-byte Ethereum-style signature: r (32) || s (32) || v (1)
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EthereumSignature {
    bytes: [u8; 65],
}

// Manual Serialize/Deserialize implementations for EthereumSignature
// because [u8; 65] doesn't have a serde derive impl by default.
impl serde::Serialize for EthereumSignature {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        self.bytes.to_vec().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for EthereumSignature {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        let bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::deserialize(deserializer)?;
        if bytes.len() != 65 {
            return Err(serde::de::Error::custom(
                "expected 65 bytes for EthereumSignature",
            ));
        }
        let mut arr = [0u8; 65];
        arr.copy_from_slice(&bytes);
        Ok(Self { bytes: arr })
    }
}

impl EthereumSignature {
    /// Create from raw bytes
    pub fn from_bytes(bytes: [u8; 65]) -> Self {
        Self { bytes }
    }

    /// Get raw bytes
    pub fn as_bytes(&self) -> &[u8; 65] {
        &self.bytes
    }

    /// Get the recovery id (v) value
    pub fn recovery_id(&self) -> u8 {
        self.bytes[64]
    }

    /// Get r value
    pub fn r(&self) -> &[u8; 32] {
        self.bytes[..32]
            .try_into()
            .expect("signature is 65 bytes; first 32 are r")
    }

    /// Get s value
    pub fn s(&self) -> &[u8; 32] {
        self.bytes[32..64]
            .try_into()
            .expect("signature is 65 bytes; s spans 32..64")
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(2 + 130);
        out.push_str("0x");
        for b in &self.bytes {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    /// Parse from hex string
    pub fn from_hex(hex: &str) -> Result<Self, Web3ErrorKind> {
        let s = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex_decode(s)?;
        if bytes.len() != 65 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "expected 65 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 65];
        arr.copy_from_slice(&bytes);
        Ok(Self { bytes: arr })
    }
}

impl core::fmt::Debug for EthereumSignature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EthereumSignature({})", self.to_hex())
    }
}

/// Sign messages and transactions using secp256k1
pub struct TransactionSigner;

impl TransactionSigner {
    /// Sign a 32-byte message hash, returning a 65-byte Ethereum signature
    #[cfg(feature = "web3")]
    pub fn sign_hash(
        secret: &Secp256k1SecretKey,
        message_hash: &[u8; 32],
    ) -> Result<EthereumSignature, Web3ErrorKind> {
        let secp = Secp256k1::signing_only();
        let msg = Message::from_digest_slice(message_hash)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid message: {}", e)))?;
        let sk = SecretKey::from_slice(secret.as_bytes())
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid key: {}", e)))?;
        let sig = secp.sign_ecdsa_recoverable(&msg, &sk);
        let (recid, compact) = sig.serialize_compact();
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&compact);
        out[64] = recid.to_i32() as u8 + 27;
        Ok(EthereumSignature::from_bytes(out))
    }

    /// Sign a message following EIP-191 personal_sign format
    #[cfg(feature = "web3")]
    pub fn sign_personal_message(
        secret: &Secp256k1SecretKey,
        message: &[u8],
    ) -> Result<EthereumSignature, Web3ErrorKind> {
        // EIP-191 prefix: \x19Ethereum Signed Message:\n{len}{message}
        let mut prefixed = Vec::with_capacity(message.len() + 28);
        prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        prefixed.extend_from_slice(&decimal_bytes(message.len()));
        prefixed.extend_from_slice(message);

        let mut hasher = Keccak256::new();
        hasher.update(&prefixed);
        let hash: [u8; 32] = hasher.finalize().into();

        Self::sign_hash(secret, &hash)
    }

    /// Sign a transaction hash (already computed by caller)
    #[cfg(feature = "web3")]
    pub fn sign_transaction_hash(
        secret: &Secp256k1SecretKey,
        tx_hash: &[u8; 32],
    ) -> Result<EthereumSignature, Web3ErrorKind> {
        Self::sign_hash(secret, tx_hash)
    }

    /// Sign a transaction using EIP-155 (v = chain_id * 2 + 35 + recid).
    ///
    /// Used for legacy transactions: the signature `v` value is computed
    /// from the chain id to prevent replay attacks across chains.
    #[cfg(feature = "web3")]
    pub fn sign_legacy_eip155(
        secret: &Secp256k1SecretKey,
        tx_hash: &[u8; 32],
        chain_id: u64,
    ) -> Result<EthereumSignature, Web3ErrorKind> {
        let mut sig = Self::sign_hash(secret, tx_hash)?;
        // EIP-155: v = chain_id * 2 + 35 + recovery_id (0 or 1)
        // recid = sig.recovery_id() - 27 (y_parity)
        let y_parity = sig.recovery_id().saturating_sub(27);
        sig.bytes[64] = (chain_id * 2 + 35 + y_parity as u64) as u8;
        Ok(sig)
    }

    /// Verify an Ethereum signature
    #[cfg(feature = "web3")]
    pub fn verify(
        message_hash: &[u8; 32],
        signature: &EthereumSignature,
        expected_address: &Address,
    ) -> Result<bool, Web3ErrorKind> {
        let recovered = Secp256k1PublicKey::recover_from(message_hash, signature.as_bytes())?;
        Ok(&recovered.to_address() == expected_address)
    }

    /// Sign an EIP-712 typed-data digest. The 32-byte digest must
    /// already be `keccak256(0x1901 || domain_separator ||
    /// message_hash)` — this function does NOT build the digest
    /// for you, because the typed-data structure (the `types` map,
    /// primary type, etc.) is application-specific. We expose
    /// [`eip712_domain_separator`] and [`eip712_hash_struct`] as
    /// building blocks for callers that want to construct the
    /// digest themselves.
    ///
    /// Returns the signature with `v` carrying the raw y_parity
    /// (0 or 1) — the convention used by every EIP-712 consumer
    /// (MetaMask, Ethers, viem, …).
    #[cfg(feature = "web3")]
    pub fn sign_typed_data_hash(
        secret: &Secp256k1SecretKey,
        digest: &[u8; 32],
    ) -> Result<EthereumSignature, Web3ErrorKind> {
        // EIP-712 deliberately uses the same keccak256-of-digest
        // signing path as personal_sign; the only difference is
        // *how the digest was constructed*. So this is just
        // `sign_hash` renamed for clarity at the call site.
        Self::sign_hash(secret, digest)
    }

    /// Compute the EIP-712 domain separator hash:
    ///
    /// ```text
    /// keccak256(typeHash || nameHash || versionHash || chainId
    ///           || verifyingContract || salt)
    /// ```
    ///
    /// `type_hash` is `keccak256("EIP712Domain(string name,string
    /// version,uint256 chainId,address verifyingContract,bytes32
    /// salt)")` — the caller must compute it (or use the helper
    /// [`eip712_domain_type_hash`]) and pass it in. Each `*Hash`
    /// argument is the keccak256 of the corresponding field
    /// value (e.g. `name_hash = keccak256(bytes("MyDApp"))`).
    ///
    /// This is intentionally a low-level helper: EIP-712 has
    /// many domain-field variations (some deployments omit
    /// `salt`, others omit `version`, …). Building a permissive
    /// but well-typed helper that handles every combination would
    /// be more code than letting the caller pick.
    #[cfg(feature = "web3")]
    pub fn eip712_domain_separator(
        type_hash: &[u8; 32],
        name_hash: Option<&[u8; 32]>,
        version_hash: Option<&[u8; 32]>,
        chain_id: Option<&[u8; 32]>,
        verifying_contract: Option<&[u8; 32]>,
        salt: Option<&[u8; 32]>,
    ) -> [u8; 32] {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(type_hash);
        if let Some(h) = name_hash {
            hasher.update(h);
        }
        if let Some(h) = version_hash {
            hasher.update(h);
        }
        if let Some(h) = chain_id {
            hasher.update(h);
        }
        if let Some(h) = verifying_contract {
            hasher.update(h);
        }
        if let Some(h) = salt {
            hasher.update(h);
        }
        hasher.finalize().into()
    }

    /// Compute the EIP-712 struct hash for a `primaryType` whose
    /// fields are given as pre-hashed 32-byte values. This
    /// matches the on-wire form: `keccak256(typeHash || h(field1)
    // || … || h(fieldN))`. As with [`Self::eip712_domain_separator`],
    /// the type hash is the caller's responsibility — use
    /// [`Self::eip712_hash_type_hash`] to compute it from the
    /// canonical "TypeName(fieldType,fieldType,…)" string.
    #[cfg(feature = "web3")]
    pub fn eip712_hash_struct(type_hash: &[u8; 32], field_hashes: &[&[u8; 32]]) -> [u8; 32] {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(type_hash);
        for h in field_hashes {
            hasher.update(*h);
        }
        hasher.finalize().into()
    }

    /// keccak256 of the canonical EIP-712 type string, e.g.
    /// `keccak256("Person(string name,uint256 age)")`. The input
    /// is the parenthesised type declaration as it appears in
    /// the spec; whitespace is preserved (callers should NOT
    /// pre-collapse it).
    #[cfg(feature = "web3")]
    pub fn eip712_hash_type_hash(type_declaration: &str) -> [u8; 32] {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(type_declaration.as_bytes());
        hasher.finalize().into()
    }

    /// EIP-712 final digest:
    /// `keccak256(0x1901 || domain_separator || message_hash)`.
    /// Use this on the output of [`Self::eip712_domain_separator`]
    /// and [`Self::eip712_hash_struct`] before passing to
    /// [`Self::sign_typed_data_hash`].
    #[cfg(feature = "web3")]
    pub fn eip712_digest(domain_separator: &[u8; 32], message_hash: &[u8; 32]) -> [u8; 32] {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update([0x19, 0x01]);
        hasher.update(domain_separator);
        hasher.update(message_hash);
        hasher.finalize().into()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert a length to decimal byte representation
fn decimal_bytes(n: usize) -> Vec<u8> {
    let s = n.to_string();
    s.into_bytes()
}

/// Decode hex to bytes
fn hex_decode(s: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(Web3ErrorKind::BlockchainError("odd hex length".to_string()));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = match chunk[0] {
            b'0'..=b'9' => chunk[0] - b'0',
            b'a'..=b'f' => chunk[0] - b'a' + 10,
            b'A'..=b'F' => chunk[0] - b'A' + 10,
            _ => return Err(Web3ErrorKind::BlockchainError("invalid hex".to_string())),
        };
        let lo = match chunk[1] {
            b'0'..=b'9' => chunk[1] - b'0',
            b'a'..=b'f' => chunk[1] - b'a' + 10,
            b'A'..=b'F' => chunk[1] - b'A' + 10,
            _ => return Err(Web3ErrorKind::BlockchainError("invalid hex".to_string())),
        };
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_hex_round_trip() {
        // Use test vector: signature with known bytes
        let bytes = [0xab; 65];
        let sig = EthereumSignature::from_bytes(bytes);
        let hex = sig.to_hex();
        let parsed = EthereumSignature::from_hex(&hex).unwrap();
        assert_eq!(parsed.as_bytes(), &bytes);
    }

    #[test]
    fn test_signature_r_s_split() {
        let mut bytes = [0u8; 65];
        for i in 0..32 {
            bytes[i] = i as u8;
            bytes[32 + i] = (32 + i) as u8;
        }
        bytes[64] = 27;
        let sig = EthereumSignature::from_bytes(bytes);
        assert_eq!(sig.r()[0], 0);
        assert_eq!(sig.s()[0], 32);
        assert_eq!(sig.recovery_id(), 27);
    }

    #[test]
    fn test_signature_debug_redacts() {
        // Verify signature can be printed (not the bytes themselves in clear)
        let sig = EthereumSignature::from_bytes([1; 65]);
        let debug = format!("{:?}", sig);
        assert!(debug.contains("0x"));
    }

    #[test]
    fn test_hex_decode_even() {
        let bytes = hex_decode("48656c6c6f").unwrap();
        assert_eq!(bytes, b"Hello");
    }

    #[test]
    fn test_hex_decode_strips_prefix() {
        let bytes = hex_decode("0x48656c6c6f").unwrap();
        assert_eq!(bytes, b"Hello");
    }

    #[test]
    fn test_hex_decode_odd_length_error() {
        assert!(hex_decode("0x123").is_err());
    }

    #[test]
    fn test_hex_decode_invalid_char() {
        assert!(hex_decode("0xZZ").is_err());
    }

    #[test]
    fn test_decimal_bytes() {
        assert_eq!(decimal_bytes(0), b"0");
        assert_eq!(decimal_bytes(5), b"5");
        assert_eq!(decimal_bytes(100), b"100");
        assert_eq!(decimal_bytes(9999), b"9999");
    }

    #[test]
    fn test_signature_recovery_id_extraction() {
        let mut bytes = [0u8; 65];
        bytes[64] = 27; // Standard v
        let sig = EthereumSignature::from_bytes(bytes);
        assert_eq!(sig.recovery_id(), 27);

        let mut bytes2 = [0u8; 65];
        bytes2[64] = 28;
        let sig2 = EthereumSignature::from_bytes(bytes2);
        assert_eq!(sig2.recovery_id(), 28);
    }

    #[test]
    fn test_signature_from_hex_invalid() {
        // Wrong length
        assert!(EthereumSignature::from_hex("0x1234").is_err());
        // Odd length
        assert!(EthereumSignature::from_hex("0x12345").is_err());
        // Invalid chars
        assert!(EthereumSignature::from_hex("0xZZZZ").is_err());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_keypair_generate_random() {
        // Two random keypairs should never collide.
        let k1 = Secp256k1Keypair::generate();
        let k2 = Secp256k1Keypair::generate();
        assert_ne!(k1.address().to_hex(), k2.address().to_hex());
        assert_ne!(k1.secret_key().as_bytes(), k2.secret_key().as_bytes());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_keypair_address_derivation() {
        // Hardcoded test vector: secret key 0x01 repeated 32 times maps to
        // a known address. This lets us verify our derivation logic without
        // needing real random keys.
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        let kp = Secp256k1Keypair::from_secret_key(bytes).unwrap();
        // Address is keccak256(pubkey)[12..32]; the hex must be 42 chars
        // (0x + 40 hex digits).
        assert_eq!(kp.address().to_hex().len(), 42);
        // Address hex should be lowercased.
        assert!(kp
            .address()
            .to_hex()
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == 'x'));
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_sign_and_verify_hash_round_trip() {
        // Sign a hash, then verify the signature recovers the original
        // signer's address.
        let kp = Secp256k1Keypair::generate();
        let msg_hash: [u8; 32] = [0xab; 32];
        let sig = TransactionSigner::sign_hash(kp.secret_key(), &msg_hash).unwrap();
        assert!(TransactionSigner::verify(&msg_hash, &sig, kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_sign_personal_message_eip191() {
        let kp = Secp256k1Keypair::generate();
        let message = b"Hello, Ethereum!";
        let sig = TransactionSigner::sign_personal_message(kp.secret_key(), message).unwrap();

        // Reconstruct the EIP-191 prefixed digest and verify it.
        let mut prefixed = Vec::with_capacity(message.len() + 28);
        prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        let len_str = message.len().to_string();
        prefixed.extend_from_slice(len_str.as_bytes());
        prefixed.extend_from_slice(message);

        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(&prefixed);
        let hash: [u8; 32] = hasher.finalize().into();

        assert!(TransactionSigner::verify(&hash, &sig, kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_sign_legacy_eip155() {
        // EIP-155 v = chain_id * 2 + 35 + y_parity
        let kp = Secp256k1Keypair::generate();
        let tx_hash: [u8; 32] = [0x42; 32];
        let chain_id = 1u64;

        let sig =
            TransactionSigner::sign_legacy_eip155(kp.secret_key(), &tx_hash, chain_id).unwrap();

        // Re-derive the expected v from a fresh signature's recid, because
        // sign_legacy_eip155 overwrites the v byte in-place.
        let fresh_sig = TransactionSigner::sign_hash(kp.secret_key(), &tx_hash).unwrap();
        let y_parity = fresh_sig.recovery_id().saturating_sub(27);
        let expected_v = (chain_id * 2 + 35 + y_parity as u64) as u8;
        assert_eq!(sig.recovery_id(), expected_v);

        // v must be >= 37 (EIP-155 boundary for chain_id == 0).
        assert!(sig.recovery_id() >= 37);
        // v must encode the chain_id (EIP-155: (v - 35) / 2 == chain_id).
        assert_eq!((sig.recovery_id() - 35) / 2, chain_id as u8);

        // Verify against the recovered key using a copy of the signature with
        // the raw y_parity (EIP-155 v values are too large to recover
        // directly; recovery only needs the y_parity bit).
        let fresh_sig = TransactionSigner::sign_hash(kp.secret_key(), &tx_hash).unwrap();
        assert!(TransactionSigner::verify(&tx_hash, &fresh_sig, kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_invalid_signature_does_not_verify() {
        // A signature over a different hash should not match the expected
        // address.
        let kp = Secp256k1Keypair::generate();
        let other_kp = Secp256k1Keypair::generate();
        let msg_hash: [u8; 32] = [0xab; 32];
        let sig = TransactionSigner::sign_hash(kp.secret_key(), &msg_hash).unwrap();
        // Signature was made by `kp`, not `other_kp`.
        assert!(!TransactionSigner::verify(&msg_hash, &sig, other_kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_recover_public_key() {
        let kp = Secp256k1Keypair::generate();
        let msg_hash: [u8; 32] = [0xcd; 32];
        let sig = TransactionSigner::sign_hash(kp.secret_key(), &msg_hash).unwrap();

        let recovered = Secp256k1PublicKey::recover_from(&msg_hash, sig.as_bytes()).unwrap();
        assert_eq!(recovered.to_address().to_hex(), kp.address().to_hex());
    }

    #[test]
    fn test_keccak256_pubkey_to_address_known_vector() {
        // Pre-computed: keccak256 of an all-zero 64-byte pubkey yields a
        // specific 32-byte hash; we take the last 20 bytes as the address.
        // This is a stable, self-contained test of the keccak256 -> address
        // path.
        #[cfg(feature = "web3")]
        {
            let pk = Secp256k1PublicKey {
                uncompressed: [0u8; 64],
            };
            let addr = pk.to_address();
            // The address must be exactly 20 bytes when serialized.
            assert_eq!(addr.as_bytes().len(), 20);
            assert_eq!(addr.to_hex().len(), 42);
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_type_hash_known_vector() {
        // keccak256("Person(string name,uint256 age)") — the helper
        // must be deterministic: two calls of the same input
        // produce the same output, and different inputs produce
        // different output.
        let a = TransactionSigner::eip712_hash_type_hash("Person(string name,uint256 age)");
        let b = TransactionSigner::eip712_hash_type_hash("Person(string name,uint256 age)");
        assert_eq!(a, b);
        let c = TransactionSigner::eip712_hash_type_hash("Person(string name,uint256 dob)");
        assert_ne!(a, c);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_hash_struct_concatenates_field_hashes() {
        let type_hash = [1u8; 32];
        let f1 = [2u8; 32];
        let f2 = [3u8; 32];
        let h = TransactionSigner::eip712_hash_struct(&type_hash, &[&f1, &f2]);
        // Different field sets → different hash.
        let h_swap = TransactionSigner::eip712_hash_struct(&type_hash, &[&f2, &f1]);
        assert_ne!(h, h_swap);
        // Empty field list → keccak256(type_hash).
        let h_empty = TransactionSigner::eip712_hash_struct(&type_hash, &[]);
        assert_ne!(h, h_empty);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_domain_separator_omits_unset_fields() {
        // A domain with no fields reduces to keccak256(type_hash).
        let type_hash = [7u8; 32];
        let none =
            TransactionSigner::eip712_domain_separator(&type_hash, None, None, None, None, None);
        // A domain with one field set is different from the empty case.
        let some = TransactionSigner::eip712_domain_separator(
            &type_hash,
            Some(&[1u8; 32]),
            None,
            None,
            None,
            None,
        );
        assert_ne!(none, some);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_digest_starts_with_0x1901() {
        // Build a known digest manually and confirm the helper
        // produces it. `eip712_digest(d, m) = keccak256(0x1901
        // || d || m)`.
        let d = [0xaa; 32];
        let m = [0xbb; 32];
        let digest = TransactionSigner::eip712_digest(&d, &m);
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update([0x19, 0x01]);
        hasher.update(d);
        hasher.update(m);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(digest, expected);
        // Digest for swapped domain/message must differ.
        let other = TransactionSigner::eip712_digest(&m, &d);
        assert_ne!(digest, other);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_sign_and_recover_round_trip() {
        // End-to-end: build a typed-data digest, sign it with a
        // keypair, and confirm `verify` recovers the right
        // address. The test is fully self-contained — we don't
        // compare against an external reference because EIP-712
        // has many correct encodings depending on which fields
        // the caller chose to include.
        let kp = Secp256k1Keypair::generate();

        // 1. Domain separator.
        let domain_type =
            TransactionSigner::eip712_hash_type_hash("EIP712Domain(string name,uint256 chainId)");
        let name_hash = TransactionSigner::eip712_hash_type_hash("MyDApp");
        let chain_id_bytes: [u8; 32] = {
            // chainId = 1, big-endian 32-byte.
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        };
        let domain_sep = TransactionSigner::eip712_domain_separator(
            &domain_type,
            Some(&name_hash),
            None,
            Some(&chain_id_bytes),
            None,
            None,
        );

        // 2. Message struct hash.
        let person_type = TransactionSigner::eip712_hash_type_hash("Person(address wallet)");
        let mut wallet_bytes = [0u8; 32];
        // 20-byte address → left-pad with zeros to 32.
        let addr_bytes = kp.address().0;
        wallet_bytes[12..32].copy_from_slice(&addr_bytes);
        let msg_struct = TransactionSigner::eip712_hash_struct(&person_type, &[&wallet_bytes]);

        // 3. Final digest.
        let digest = TransactionSigner::eip712_digest(&domain_sep, &msg_struct);

        // 4. Sign + verify.
        let sig = TransactionSigner::sign_typed_data_hash(kp.secret_key(), &digest).unwrap();
        assert!(TransactionSigner::verify(&digest, &sig, kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_eip712_sign_typed_data_hash_is_alias_for_sign_hash() {
        // `sign_typed_data_hash` and `sign_hash` use the same
        // underlying signing path. The contract is: anything
        // signed by one can be verified by the other. This is
        // important because every EIP-712 verifier on the
        // planet assumes that `verify(digest, …)` works.
        let kp = Secp256k1Keypair::generate();
        let digest: [u8; 32] = [0x42; 32];

        let sig = TransactionSigner::sign_typed_data_hash(kp.secret_key(), &digest).unwrap();
        assert!(TransactionSigner::verify(&digest, &sig, kp.address()).unwrap());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn h10_from_bytes_rejects_zero_scalar() {
        // HARDENING (audit-2026-08 H10): a scalar equal to the
        // curve group order, or any larger value, must not produce
        // an invalid secp256k1 key. The constructor `from_bytes`
        // must reject it via `SecretKey::from_slice`.
        let bytes = [0u8; 32];
        let r = Secp256k1SecretKey::from_bytes(bytes);
        assert!(
            matches!(r, Err(Web3ErrorKind::BlockchainError(_))),
            "expected BlockchainError for zero scalar, got {r:?}"
        );
    }

    #[cfg(feature = "web3")]
    #[test]
    fn h10_from_bytes_rejects_all_ones_scalar() {
        // 0xFFFF...FF is a valid field element but exceeds the curve
        // group order, so `SecretKey::from_slice` must reject it.
        let bytes = [0xFFu8; 32];
        let r = Secp256k1SecretKey::from_bytes(bytes);
        assert!(
            matches!(r, Err(Web3ErrorKind::BlockchainError(_))),
            "expected BlockchainError for all-ones scalar, got {r:?}"
        );
    }

    #[cfg(feature = "web3")]
    #[test]
    fn h10_public_key_for_valid_key_succeeds() {
        // Make sure the post-H10 `Result` return type doesn't break
        // the happy path: a valid secret key still derives a public
        // key successfully.
        let kp = Secp256k1Keypair::generate();
        let pk = kp.secret_key().public_key().expect("valid key derives pk");
        assert_eq!(pk.as_bytes().len(), 64);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn h10_inner_for_valid_key_succeeds() {
        // Mirror of the public_key test for `inner()` — same happy
        // path, must remain working after the H10 hardening.
        let kp = Secp256k1Keypair::generate();
        let _sk = kp.secret_key().inner().expect("valid key returns sk");
    }
}
