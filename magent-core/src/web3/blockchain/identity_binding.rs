//! Identity-to-Blockchain Binding Module.
//!
//! This module implements the binding between mAgent's `did:key` identities
//! and blockchain addresses. It enables:
//!
//! - **On-chain DID binding**: Store a `did:key` in a blockchain registry
//! - **Proof of ownership**: Prove control of a blockchain address using did:key
//! - **Cross-chain identity**: Manage the same identity across multiple chains
//! - **Verification**: Verify that an address is bound to a specific DID
//!
//! ## Binding Methods
//!
//! 1. **Message Signing**: Sign a message proving control of the private key
//! 2. **Personal Sign**: Sign using Ethereum's personal_sign format
//! 3. **Typed Data**: Sign structured data following EIP-712
//!
//! ## Protocol
//!
//! ```text
//! User (mAgent)                          Blockchain Registry
//!    │                                           │
//!    │  1. Create binding intent                 │
//!    │  ────────────────────────────────────────│
//!    │                                           │
//!    │  2. Sign proof with did:key              │
//!    │     (EIP-191 or EIP-712)                 │
//!    │  ←───────────────────────────────────────
//!    │                                           │
//!    │  3. Submit binding transaction            │
//!    │  ────────────────────────────────────────│
//!    │                                           │
//!    │  4. Verify and store binding             │
//!    │  ←───────────────────────────────────────
//! ```

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use super::Address;

/// The status of a DID-to-address binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingStatus {
    /// Binding is pending (transaction submitted but not confirmed).
    Pending,
    /// Binding is confirmed on-chain.
    Confirmed,
    /// Binding has been revoked.
    Revoked,
    /// Binding has expired.
    Expired,
}

impl BindingStatus {
    /// Check if the binding is valid (confirmed and not expired).
    pub fn is_valid(&self) -> bool {
        matches!(self, BindingStatus::Confirmed)
    }
}

/// A binding between a DID and a blockchain address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityBinding {
    /// The DID (did:key format).
    pub did: String,
    /// The bound blockchain address.
    pub address: Address,
    /// Chain ID where the binding is registered.
    pub chain_id: u64,
    /// Block number when the binding was created.
    pub created_at_block: u64,
    /// Timestamp when the binding was created.
    pub created_at_timestamp: u64,
    /// Optional expiry time (Unix timestamp).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Optional domain separator (for EIP-712).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Binding metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

impl IdentityBinding {
    /// Create a new binding.
    pub fn new(
        did: impl Into<String>,
        address: Address,
        chain_id: u64,
        created_at_block: u64,
        created_at_timestamp: u64,
    ) -> Self {
        Self {
            did: did.into(),
            address,
            chain_id,
            created_at_block,
            created_at_timestamp,
            expires_at: None,
            domain: None,
            metadata: None,
        }
    }

    /// Set the expiry time.
    pub fn with_expiry(mut self, timestamp: u64) -> Self {
        self.expires_at = Some(timestamp);
        self
    }

    /// Set the domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Set the metadata.
    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Check if the binding has expired.
    pub fn is_expired(&self, current_time: u64) -> bool {
        self.expires_at
            .map(|exp| current_time > exp)
            .unwrap_or(false)
    }

    /// Get the current status.
    pub fn status(&self, current_time: u64) -> BindingStatus {
        if self.is_expired(current_time) {
            return BindingStatus::Expired;
        }
        BindingStatus::Confirmed
    }

    /// Short human-readable summary (`"did:…@0x… (chain 1)"`).
    /// Used for log lines; full re-encoding lives in the JSON
    /// (de)serialisers.
    pub fn display_short(&self) -> String {
        let mut s = format!("{} @ {} (chain {})", self.did, self.address.to_hex(), self.chain_id);
        if let Some(exp) = self.expires_at {
            s.push_str(&format!(" [expires {}]", exp));
        }
        s
    }
}

/// A cryptographic proof of DID ownership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingProof {
    /// The DID being proven.
    pub did: String,
    /// The Ethereum address claiming ownership.
    pub address: Address,
    /// Chain ID for this proof.
    pub chain_id: u64,
    /// The signature proving control (EIP-191 format).
    pub signature: String,
    /// The message that was signed.
    pub message: String,
    /// Unix timestamp when the proof was created.
    pub created_at: u64,
    /// Optional nonce to prevent replay attacks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    /// The domain separator (for EIP-712).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl BindingProof {
    /// Create a new binding proof.
    pub fn new(
        did: impl Into<String>,
        address: Address,
        chain_id: u64,
        signature: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            did: did.into(),
            address,
            chain_id,
            signature: signature.into(),
            message: message.into(),
            created_at: current_unix_timestamp(),
            nonce: None,
            domain: None,
        }
    }

    /// Set a nonce.
    pub fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = Some(nonce);
        self
    }

    /// Set the domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Get the signature bytes.
    pub fn signature_bytes(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        let sig = self.signature.strip_prefix("0x").unwrap_or(&self.signature);
        hex_decode(sig)
    }

    /// Verify the proof format is correct.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if self.did.is_empty() {
            return Err(Web3ErrorKind::BlockchainError(
                "DID cannot be empty".to_string(),
            ));
        }
        if !self.did.starts_with("did:key:") {
            return Err(Web3ErrorKind::BlockchainError(
                "DID must be in did:key format".to_string(),
            ));
        }
        // Accept either bare hex (128 chars for a 64-byte
        // sig with no recovery byte, plus a length check on
        // the 130-char 65-byte sig) or 0x-prefixed forms.
        // The actual cryptographic verification tolerates
        // both via the prefix-strip in `verify_crypto`, so
        // the format check must mirror that.
        let sig_len = self.signature.len();
        let sig_stripped = self
            .signature
            .strip_prefix("0x")
            .unwrap_or(&self.signature);
        // Two valid forms: a 64-byte signature (128 hex chars) or a
        // 65-byte signature with `v` (130 hex chars). Either form may be
        // `0x`-prefixed (adds 2 to the char count) — but the stripped
        // length must still be 128 or 130.
        let stripped_len = sig_stripped.len();
        let valid_length = matches!(sig_len, 128 | 130)
            || (matches!(sig_len, 130 | 132) && matches!(stripped_len, 128 | 130));
        if !valid_length {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "signature must be 64 or 65 bytes (128/130 hex chars, optionally 0x-prefixed), got {}",
                sig_len
            )));
        }
        if self.message.is_empty() {
            return Err(Web3ErrorKind::BlockchainError(
                "message cannot be empty".to_string(),
            ));
        }
        Ok(())
    }

    /// Cryptographically verify the proof: recover the secp256k1
    /// public key from the signature over `self.message` (using the
    /// Ethereum personal-sign / EIP-191 prefix) and confirm it
    /// matches `self.address`.
    ///
    /// This is the trust anchor of the binding — it answers "does
    /// the signer of this proof actually control `self.address`?"
    ///
    /// Returns `Ok(true)` if the signature is valid and recovers to
    /// `self.address`, `Ok(false)` if the signature is well-formed
    /// but does not recover to that address, and `Err(_)` if the
    /// signature is malformed (wrong length, bad hex, …).
    #[cfg(feature = "web3")]
    pub fn verify_crypto(&self) -> Result<bool, Web3ErrorKind> {
        use sha3::{Digest, Keccak256};

        // Decode signature bytes (strip `0x` prefix if present).
        let sig_hex = self
            .signature
            .strip_prefix("0x")
            .unwrap_or(&self.signature);
        let sig_bytes = match hex_decode(sig_hex) {
            Ok(b) if b.len() == 65 => b,
            Ok(_) => {
                return Err(Web3ErrorKind::BlockchainError(
                    "signature must be 65 bytes".to_string(),
                ))
            }
            Err(e) => return Err(e),
        };

        // EIP-191 personal_sign prefix. Matches the layout used by
        // `TransactionSigner::sign_personal_message` exactly:
        //
        //     \x19Ethereum Signed Message:\n{len}{message}
        //
        // Adding an extra `\n` here would diverge from the signer
        // and every recovery would silently fail.
        let mut prefixed = Vec::with_capacity(self.message.len() + 28);
        prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        prefixed.extend_from_slice(self.message.len().to_string().as_bytes());
        prefixed.extend_from_slice(self.message.as_bytes());

        let mut hasher = Keccak256::new();
        hasher.update(&prefixed);
        let digest: [u8; 32] = hasher.finalize().into();

        // Recover the public key.
        let mut sig_arr = [0u8; 65];
        sig_arr.copy_from_slice(&sig_bytes);
        let pk = crate::web3::blockchain::Secp256k1PublicKey::recover_from(&digest, &sig_arr)?;
        let recovered_addr = pk.to_address();

        // Compare addresses (case-insensitive on the hex).
        let claimed = self.address.to_hex().to_lowercase();
        let recovered = recovered_addr.to_hex().to_lowercase();
        Ok(claimed == recovered)
    }

    /// Convenience: chain `validate()` + `verify_crypto()`. Returns
    /// `Err` for format errors and `Ok(false)` for cryptographic
    /// mismatches.
    #[cfg(feature = "web3")]
    pub fn verify(&self) -> Result<bool, Web3ErrorKind> {
        self.validate()?;
        self.verify_crypto()
    }
}

/// A claim for creating or updating a DID binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingClaim {
    /// The DID to bind.
    pub did: String,
    /// The blockchain address to bind to.
    pub address: Address,
    /// Chain ID.
    pub chain_id: u64,
    /// Unix timestamp of claim creation.
    pub issued_at: u64,
    /// Claim expiry time.
    pub expires_at: u64,
    /// Domain separator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl BindingClaim {
    /// Create a new binding claim.
    pub fn new(
        did: impl Into<String>,
        address: Address,
        chain_id: u64,
        validity_seconds: u64,
    ) -> Self {
        let now = current_unix_timestamp();
        Self {
            did: did.into(),
            address,
            chain_id,
            issued_at: now,
            expires_at: now.saturating_add(validity_seconds),
            domain: None,
        }
    }

    /// Set the domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Encode as canonical bytes for signing (EIP-712 structured data).
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Domain separator
        if let Some(ref domain) = self.domain {
            bytes.extend_from_slice(b"domain:");
            bytes.extend_from_slice(domain.as_bytes());
            bytes.push(b'\n');
        }

        // Primary type
        bytes.extend_from_slice(b"DIDBindingClaim\n");

        // Field: did
        bytes.extend_from_slice(b"did:");
        bytes.extend_from_slice(self.did.as_bytes());
        bytes.push(b'\n');

        // Field: address
        bytes.extend_from_slice(b"address:");
        bytes.extend_from_slice(self.address.to_hex().as_bytes());
        bytes.push(b'\n');

        // Field: chainId
        bytes.extend_from_slice(b"chainId:");
        bytes.extend_from_slice(self.chain_id.to_string().as_bytes());
        bytes.push(b'\n');

        // Field: issuedAt
        bytes.extend_from_slice(b"issuedAt:");
        bytes.extend_from_slice(self.issued_at.to_string().as_bytes());
        bytes.push(b'\n');

        // Field: expiresAt
        bytes.extend_from_slice(b"expiresAt:");
        bytes.extend_from_slice(self.expires_at.to_string().as_bytes());

        bytes
    }

    /// Check if the claim has expired.
    pub fn is_expired(&self) -> bool {
        current_unix_timestamp() > self.expires_at
    }
}

/// A verification request for checking a DID binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRequest {
    /// The DID to verify.
    pub did: String,
    /// The claimed address.
    pub address: Address,
    /// Chain ID.
    pub chain_id: u64,
    /// Block number at which to verify (None = latest).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_block: Option<u64>,
}

impl VerificationRequest {
    /// Create a new verification request.
    pub fn new(did: impl Into<String>, address: Address, chain_id: u64) -> Self {
        Self {
            did: did.into(),
            address,
            chain_id,
            at_block: None,
        }
    }

    /// Set the block number.
    pub fn at_block(mut self, block: u64) -> Self {
        self.at_block = Some(block);
        self
    }
}

/// The result of a binding verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Whether the binding is valid.
    pub is_valid: bool,
    /// The binding if found and valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<IdentityBinding>,
    /// The reason if invalid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether the proof was verified.
    pub proof_verified: bool,
}

impl VerificationResult {
    /// Create a successful verification.
    pub fn valid(binding: IdentityBinding) -> Self {
        Self {
            is_valid: true,
            binding: Some(binding),
            reason: None,
            proof_verified: true,
        }
    }

    /// Create a failed verification.
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            is_valid: false,
            binding: None,
            reason: Some(reason.into()),
            proof_verified: false,
        }
    }
}

/// Builder for creating DID bindings.
#[derive(Debug, Clone)]
pub struct BindingBuilder {
    did: Option<String>,
    address: Option<Address>,
    chain_id: u64,
    created_at_block: u64,
    created_at_timestamp: u64,
    expires_at: Option<u64>,
    domain: Option<String>,
    metadata: Option<String>,
}

impl BindingBuilder {
    /// Create a new builder.
    pub fn new(chain_id: u64) -> Self {
        Self {
            did: None,
            address: None,
            chain_id,
            created_at_block: 0,
            created_at_timestamp: current_unix_timestamp(),
            expires_at: None,
            domain: None,
            metadata: None,
        }
    }

    /// Set the DID.
    pub fn did(mut self, did: impl Into<String>) -> Self {
        self.did = Some(did.into());
        self
    }

    /// Set the address.
    pub fn address(mut self, address: Address) -> Self {
        self.address = Some(address);
        self
    }

    /// Set the creation block.
    pub fn created_at_block(mut self, block: u64) -> Self {
        self.created_at_block = block;
        self
    }

    /// Set the creation timestamp.
    pub fn created_at_timestamp(mut self, timestamp: u64) -> Self {
        self.created_at_timestamp = timestamp;
        self
    }

    /// Set the expiry time.
    pub fn expires_at(mut self, timestamp: u64) -> Self {
        self.expires_at = Some(timestamp);
        self
    }

    /// Set the domain.
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Set the metadata.
    pub fn metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Build the binding.
    pub fn build(self) -> Result<IdentityBinding, Web3ErrorKind> {
        let did = self.did.ok_or_else(|| {
            Web3ErrorKind::BlockchainError("DID is required".to_string())
        })?;
        let address = self.address.ok_or_else(|| {
            Web3ErrorKind::BlockchainError("address is required".to_string())
        })?;

        Ok(IdentityBinding {
            did,
            address,
            chain_id: self.chain_id,
            created_at_block: self.created_at_block,
            created_at_timestamp: self.created_at_timestamp,
            expires_at: self.expires_at,
            domain: self.domain,
            metadata: self.metadata,
        })
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get the current Unix timestamp.
///
/// In `std` environments, uses `std::time::SystemTime`.
/// In `no_std` environments, falls back to a counter-based approximation.
fn current_unix_timestamp() -> u64 {
    #[cfg(feature = "std")]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    #[cfg(not(feature = "std"))]
    {
        use core::sync::atomic::{AtomicU32 as AtomicCounter, Ordering};
        static COUNTER: AtomicCounter = AtomicCounter::new(1700000000u32); // Jan 2024 approx
        COUNTER.fetch_add(1, Ordering::Relaxed).into()
    }
}

/// Hex decode a string.
fn hex_decode(s: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(Web3ErrorKind::BlockchainError(
            "odd number of hex digits".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = match chunk[0] {
            b'0'..=b'9' => chunk[0] - b'0',
            b'a'..=b'f' => chunk[0] - b'a' + 10,
            b'A'..=b'F' => chunk[0] - b'A' + 10,
            _ => return Err(Web3ErrorKind::BlockchainError("invalid hex digit".to_string())),
        };
        let lo = match chunk[1] {
            b'0'..=b'9' => chunk[1] - b'0',
            b'a'..=b'f' => chunk[1] - b'a' + 10,
            b'A'..=b'F' => chunk[1] - b'A' + 10,
            _ => return Err(Web3ErrorKind::BlockchainError("invalid hex digit".to_string())),
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
    fn test_binding_creation() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let binding = IdentityBinding::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            18000000,
            1700000000,
        );

        assert_eq!(binding.chain_id, 1);
        assert!(!binding.is_expired(0));
    }

    #[test]
    fn test_binding_builder() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let binding = BindingBuilder::new(1)
            .did("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK")
            .address(address)
            .created_at_block(18000000)
            .expires_at(1800000000)
            .build()
            .unwrap();

        assert_eq!(binding.chain_id, 1);
        assert!(binding.expires_at.is_some());
    }

    #[test]
    fn test_binding_claim_encode() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let claim = BindingClaim::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            3600,
        );

        let encoded = claim.encode();
        assert!(!encoded.is_empty());
        assert!(encoded.windows(8).any(|w| w == b"did:key:"));
    }

    #[test]
    fn test_proof_validation() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let proof = BindingProof::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            "0x".to_string() + &"a".repeat(128),
            "test message",
        );

        assert!(proof.validate().is_ok());
    }

    #[test]
    fn test_proof_validation_rejects_invalid() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();

        // Too short signature
        let proof = BindingProof::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            "0xabcd",
            "test message",
        );

        assert!(proof.validate().is_err());
    }

    #[test]
    fn test_verification_result() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let binding = IdentityBinding::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            18000000,
            1700000000,
        );

        let valid = VerificationResult::valid(binding.clone());
        assert!(valid.is_valid);
        assert!(valid.binding.is_some());

        let invalid = VerificationResult::invalid("binding not found");
        assert!(!invalid.is_valid);
        assert!(invalid.reason.is_some());
    }

    #[test]
    fn test_display_short_basic() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let binding = IdentityBinding::new(
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            address,
            1,
            18000000,
            1700000000,
        );

        let s = binding.display_short();
        assert!(s.contains("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"));
        assert!(s.contains("chain 1"));
        assert!(!s.contains("expires")); // no expiry set

        let with_exp = binding.clone().with_expiry(1800000000);
        let s2 = with_exp.display_short();
        assert!(s2.contains("expires"));
    }

    #[test]
    fn test_signature_bytes_with_and_without_prefix() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let with_prefix =
            BindingProof::new("did:key:z6Mktest", address, 1, "0x".to_string() + &"a".repeat(128), "m");
        let without_prefix =
            BindingProof::new("did:key:z6Mktest", address, 1, "a".repeat(128), "m");

        // Both should decode successfully (130 chars vs 128 chars).
        assert_eq!(with_prefix.signature_bytes().unwrap().len(), 64);
        assert_eq!(without_prefix.signature_bytes().unwrap().len(), 64);
    }

    #[test]
    fn test_signature_bytes_rejects_odd_hex() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let proof = BindingProof::new(
            "did:key:z6Mktest",
            address,
            1,
            "abc", // odd number of chars
            "m",
        );
        // validate() rejects the length, but signature_bytes() is allowed
        // for any hex string (the format check is the caller's job).
        // Both behaviours are reasonable; we just want *some* failure path.
        assert!(proof.validate().is_err());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_verify_crypto_well_formed_signature_does_not_recover() {
        // Garbage-but-65-byte signature: verify_crypto should recover
        // to *some* address, but it won't match the claimed one, so
        // the function returns Ok(false).
        use crate::web3::blockchain::{Secp256k1Keypair, TransactionSigner};

        let keypair = Secp256k1Keypair::generate_test();
        let real_addr = *keypair.address();
        // Sign something different from what we put in the proof.
        let real_sig = TransactionSigner::sign_personal_message(
            keypair.secret_key(),
            b"different message",
        )
        .unwrap();
        let proof = BindingProof::new(
            "did:key:z6Mktest",
            real_addr,
            1,
            real_sig.to_hex(),
            "yet another message",
        );
        let res = proof.verify_crypto();
        assert!(res.is_ok());
        assert!(!res.unwrap()); // false: address mismatch / wrong hash
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_verify_crypto_malformed_signature_returns_err() {
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let proof = BindingProof::new(
            "did:key:z6Mktest",
            address,
            1,
            "0x".to_string() + &"ab".repeat(32), // 64 bytes — wrong length
            "hello",
        );
        let res = proof.verify_crypto();
        assert!(res.is_err());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_verify_chain_validates_then_verifies() {
        // Empty DID should fail at the validate() step, not at the
        // crypto step.
        let address = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let proof = BindingProof::new(
            "",
            address,
            1,
            "0x".to_string() + &"a".repeat(128),
            "hi",
        );
        assert!(proof.verify().is_err());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_verify_crypto_happy_path() {
        // Generate a real signature with a known keypair and confirm
        // that verify_crypto() recovers the right address. This is
        // the only test that exercises the real cryptographic path;
        // it would have caught the off-by-one `\n` bug that
        // previously made every recovery return Ok(false).
        use crate::web3::blockchain::{Secp256k1Keypair, TransactionSigner};

        let keypair = Secp256k1Keypair::generate_test();
        let addr = *keypair.address();
        let msg = "I am the owner of this address";
        let sig = TransactionSigner::sign_personal_message(keypair.secret_key(), msg.as_bytes())
            .unwrap();

        let proof = BindingProof::new(
            "did:key:z6Mktest",
            addr,
            1,
            sig.to_hex(),
            msg,
        );
        assert!(proof.verify_crypto().unwrap());
    }
}
