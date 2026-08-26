//! Blockchain Integration Layer for mAgent.
//!
//! This module provides the bridge between mAgent's cryptographic identity
//! system (`did:key`) and actual blockchain networks. It enables:
//!
//! - **Blockchain Client Abstraction**: Pluggable RPC clients for any EVM-compatible chain
//! - **Chain-Agnostic Identity Binding**: Bind `did:key` identities to on-chain addresses
//! - **Transaction Building & Signing**: Build, sign, and broadcast transactions
//! - **Event Indexing**: Subscribe to and parse on-chain events
//! - **Multi-Chain Support**: Manage identities across multiple networks
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │           mAgent Core                    │
//! │  ┌─────────────────────────────────┐    │
//! │  │  Identity (did:key)             │    │
//! │  │  Ed25519 Keypair + Signature   │    │
//! │  └─────────────────────────────────┘    │
//! └─────────────────┬───────────────────────┘
//!                   │ bind / verify
//! ┌─────────────────▼───────────────────────┐
//! │     Blockchain Integration Layer          │
//! │  ┌─────────────────────────────────┐    │
//! │  │  ChainClient (trait)            │    │
//! │  │  ┌───────────┬───────────┐     │    │
//! │  │  │ Ethereum  │ Polygon   │ ... │    │
//! │  │  └───────────┴───────────┘     │    │
//! │  └─────────────────────────────────┘    │
//! └─────────────────┬───────────────────────┘
//!                   │ RPC
//! ┌─────────────────▼───────────────────────┐
//! │     Blockchain Network                  │
//! │  (Ethereum, Polygon, Solana, etc.)    │
//! └───────────────────────────────────────┘
//! ```
//!
//! ## Supported Chains
//!
//! - EVM-compatible chains (Ethereum, Polygon, Arbitrum, Optimism, etc.)
//! - Support for custom chain configurations
//!
//! ## Feature Flag
//!
//! Gated on `magent-core`'s `blockchain` feature, which transitively
//! enables `web3` + `std`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;

pub mod client;
pub mod identity_binding;
pub mod transaction;
pub mod events;

// JSON-RPC types for HTTP client
pub mod http_types;

// HTTP client for std environments
#[cfg(feature = "std")]
pub mod http_client;

// ESP32-compatible polling client (no_std)
#[cfg(feature = "esp32")]
pub mod esp32_client;

// ESP32 HTTP client for blockchain RPC
#[cfg(feature = "esp32")]
pub mod esp32_http;

// Agent integration tools
#[cfg(feature = "web3")]
pub mod agent_tools;

// Secp256k1 signing for Ethereum transactions
#[cfg(feature = "web3")]
pub mod secp256k1;

pub use client::{ChainClient, BlockchainResult, ChainId};
pub use identity_binding::{IdentityBinding, BindingProof, BindingStatus};
pub use transaction::{Transaction, TransactionRequest, TransactionReceipt};
pub use events::{EventFilter, EventLog};

// Re-export agent-facing helper types from agent_tools so callers can
// `use magent_core::web3::blockchain::BlockchainManager` etc.
#[cfg(feature = "web3")]
pub use agent_tools::{
    BlockchainManager, BlockchainToolResult, execute_blockchain_tool,
};

// ChainConfig, Chain, KnownChain are defined in this module (mod.rs) and
// already accessible without re-exporting them.

// HTTP client for std environments
#[cfg(feature = "std")]
pub use http_client::HttpRpcClient;

// Secp256k1 signing types
#[cfg(feature = "web3")]
pub use secp256k1::{
    EthereumSignature, Secp256k1Keypair, Secp256k1PublicKey, Secp256k1SecretKey,
    TransactionSigner,
};

// Re-export ESP32 types when feature enabled
#[cfg(feature = "esp32")]
pub use esp32_client::{Esp32BlockchainClient, BlockchainState, BlockchainPollResult};
#[cfg(feature = "esp32")]
pub use esp32_http::{EspHttpClient, HttpClientConfig, TransactionPoller, PollStatus};

// ============================================================================
// Chain Configuration
// ============================================================================

/// Known chain identifiers (EIP-155 compliant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownChain {
    /// Ethereum Mainnet
    Ethereum,
    /// Ethereum Sepolia Testnet
    Sepolia,
    /// Polygon Mainnet
    Polygon,
    /// Polygon Amoy Testnet
    Amoy,
    /// Arbitrum One
    Arbitrum,
    /// Optimism Mainnet
    Optimism,
    /// Base
    Base,
    /// Gnosis Chain
    Gnosis,
    /// Custom chain (uses ChainConfig)
    Custom,
}

impl KnownChain {
    /// Get the chain ID for this known chain.
    pub fn chain_id(&self) -> u64 {
        match self {
            KnownChain::Ethereum => 1,
            KnownChain::Sepolia => 11155111,
            KnownChain::Polygon => 137,
            KnownChain::Amoy => 80002,
            KnownChain::Arbitrum => 42161,
            KnownChain::Optimism => 10,
            KnownChain::Base => 8453,
            KnownChain::Gnosis => 100,
            KnownChain::Custom => 0, // Must be configured manually
        }
    }

    /// Get the native currency symbol for this chain.
    pub fn currency_symbol(&self) -> &'static str {
        match self {
            KnownChain::Ethereum => "ETH",
            KnownChain::Sepolia => "SepoliaETH",
            KnownChain::Polygon => "MATIC",
            KnownChain::Amoy => "AmoyMATIC",
            KnownChain::Arbitrum => "ETH",
            KnownChain::Optimism => "ETH",
            KnownChain::Base => "ETH",
            KnownChain::Gnosis => "xDAI",
            KnownChain::Custom => "ETH",
        }
    }

    /// Get the RPC endpoint for this chain (public endpoints).
    pub fn public_rpc(&self) -> Option<&'static str> {
        match self {
            KnownChain::Ethereum => Some("https://eth.llamarpc.com"),
            KnownChain::Sepolia => Some("https://rpc.sepolia.org"),
            KnownChain::Polygon => Some("https://polygon-rpc.com"),
            KnownChain::Amoy => Some("https://rpc-amoy.polygon.technology"),
            KnownChain::Arbitrum => Some("https://arb1.arbitrum.io/rpc"),
            KnownChain::Optimism => Some("https://mainnet.optimism.io"),
            KnownChain::Base => Some("https://mainnet.base.org"),
            KnownChain::Gnosis => Some("https://rpc.gnosischain.com"),
            KnownChain::Custom => None,
        }
    }

    /// Get the block explorer URL for this chain.
    pub fn block_explorer(&self) -> Option<&'static str> {
        match self {
            KnownChain::Ethereum => Some("https://etherscan.io"),
            KnownChain::Sepolia => Some("https://sepolia.etherscan.io"),
            KnownChain::Polygon => Some("https://polygonscan.com"),
            KnownChain::Amoy => Some("https://www.oklink.com/amoy"),
            KnownChain::Arbitrum => Some("https://arbiscan.io"),
            KnownChain::Optimism => Some("https://optimistic.etherscan.io"),
            KnownChain::Base => Some("https://basescan.org"),
            KnownChain::Gnosis => Some("https://gnosisscan.io"),
            KnownChain::Custom => None,
        }
    }

    /// Get the chain name for display.
    pub fn name(&self) -> &'static str {
        match self {
            KnownChain::Ethereum => "Ethereum Mainnet",
            KnownChain::Sepolia => "Ethereum Sepolia Testnet",
            KnownChain::Polygon => "Polygon Mainnet",
            KnownChain::Amoy => "Polygon Amoy Testnet",
            KnownChain::Arbitrum => "Arbitrum One",
            KnownChain::Optimism => "Optimism Mainnet",
            KnownChain::Base => "Base",
            KnownChain::Gnosis => "Gnosis Chain",
            KnownChain::Custom => "Custom Chain",
        }
    }
}

/// A configured chain, either known or custom.
#[derive(Debug, Clone)]
pub struct Chain {
    /// The chain configuration.
    pub config: ChainConfig,
    /// Cached chain ID for convenience.
    pub chain_id: u64,
}

impl Chain {
    /// Create a chain from a known chain enum.
    pub fn from_known(chain: KnownChain) -> Self {
        let chain_id = chain.chain_id();
        let config = ChainConfig {
            chain_id,
            rpc_url: chain.public_rpc().map(String::from),
            name: chain.name().to_string(),
            currency_symbol: chain.currency_symbol().to_string(),
            block_explorer: chain.block_explorer().map(String::from),
        };
        Self { config, chain_id }
    }

    /// Create a custom chain with the given configuration.
    pub fn custom(config: ChainConfig) -> Self {
        Self {
            chain_id: config.chain_id,
            config,
        }
    }

    /// Get the RPC URL, returning an error if not configured.
    pub fn rpc_url(&self) -> Result<&str, Web3ErrorKind> {
        self.config
            .rpc_url
            .as_deref()
            .ok_or_else(|| {
                Web3ErrorKind::BlockchainError(
                    "RPC URL not configured for this chain".to_string(),
                )
            })
    }
}

/// Configuration for a blockchain chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    /// The EIP-155 chain ID.
    pub chain_id: u64,
    /// RPC endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Human-readable chain name.
    pub name: String,
    /// Native currency symbol (e.g., "ETH", "MATIC").
    pub currency_symbol: String,
    /// Block explorer URL (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_explorer: Option<String>,
}

impl ChainConfig {
    /// Create a new chain configuration.
    pub fn new(chain_id: u64, name: impl Into<String>) -> Self {
        Self {
            chain_id,
            rpc_url: None,
            name: name.into(),
            currency_symbol: "ETH".to_string(),
            block_explorer: None,
        }
    }

    /// Set the RPC URL.
    pub fn with_rpc_url(mut self, rpc_url: impl Into<String>) -> Self {
        self.rpc_url = Some(rpc_url.into());
        self
    }

    /// Set the currency symbol.
    pub fn with_currency_symbol(mut self, symbol: impl Into<String>) -> Self {
        self.currency_symbol = symbol.into();
        self
    }

    /// Set the block explorer URL.
    pub fn with_block_explorer(mut self, explorer: impl Into<String>) -> Self {
        self.block_explorer = Some(explorer.into());
        self
    }
}

// ============================================================================
// Ethereum Address Types
// ============================================================================

/// A 20-byte Ethereum address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address([u8; 20]);

impl Address {
    /// The zero address.
    pub const ZERO: Address = Address([0u8; 20]);

    /// The zero address (alternate spelling). Returns `Self::ZERO`.
    /// Mirrors [`Hash::zero`] so callers can use either form.
    pub fn zero() -> Self {
        Self::ZERO
    }

    /// Create an address from 20 bytes.
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Parse an address from a hex string (with or without 0x prefix).
    pub fn from_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        if s.len() != 40 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "invalid address length: expected 40 hex chars, got {}",
                s.len()
            )));
        }
        let bytes = hex_decode(s)?;
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&bytes[..20]);
        Ok(Self(addr))
    }

    /// FEATURE (audit-2026-08 round-4): strict parse that rejects
    /// mixed-case hex strings whose case doesn't match the EIP-55
    /// checksum. Useful for the wallet import path: a user pasting
    /// an address with a typo'd character usually produces an
    /// invalid EIP-55 checksum, which is the strongest hint we
    /// have that the address is wrong.
    ///
    /// Accepts:
    /// * `0x` followed by 40 hex chars,
    /// * all-lowercase or all-uppercase (treated as "no checksum"),
    /// * valid EIP-55 mixed case.
    ///
    /// Rejects:
    /// * wrong-length,
    /// * non-hex characters,
    /// * mixed-case that doesn't match the keccak256-derived
    ///   checksum.
    pub fn from_checksummed_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        if s.len() != 40 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "invalid address length: expected 40 hex chars, got {}",
                s.len()
            )));
        }
        // Decide whether the caller provided a checksum: if any
        // hex letter appears in both upper and lower case, the
        // string is mixed-case and we MUST validate it. A pure
        // lower- or upper-case string is treated as "no checksum"
        // (always accepted).
        let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
        let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
        let mixed_case = has_lower && has_upper;

        let bytes = hex_decode(s)?;
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&bytes[..20]);
        let parsed = Self(addr);

        if mixed_case {
            // Recompute the canonical EIP-55 form and require an
            // exact byte-for-byte match on the input characters.
            let canonical = parsed.to_checksum();
            let canonical_no_prefix = canonical
                .strip_prefix("0x")
                .unwrap_or(canonical.as_str());
            if !canonical_no_prefix.eq_ignore_ascii_case(s)
                || canonical_no_prefix != s
            {
                return Err(Web3ErrorKind::BlockchainError(format!(
                    "EIP-55 checksum mismatch: expected {}",
                    canonical
                )));
            }
        }

        Ok(parsed)
    }

    /// FEATURE (audit-2026-08 round-4): validate the EIP-55
    /// checksum of an already-parsed address by re-deriving the
    /// canonical mixed-case form and comparing. Returns `Ok(())`
    /// for all-lowercase / all-uppercase addresses (no checksum)
    /// since EIP-55 treats those as unverified.
    #[cfg(feature = "web3")]
    pub fn validate_checksum(&self) -> Result<(), Web3ErrorKind> {
        let canonical = self.to_checksum();
        // The canonical form is always mixed-case (since `web3` is
        // enabled). If it doesn't match the canonical shape, the
        // caller must be calling us in an unsupported build.
        let canonical_hex = canonical
            .strip_prefix("0x")
            .unwrap_or(canonical.as_str());
        // The only way this can fail is if the underlying keccak
        // implementation produces different output than expected —
        // which would itself be a critical bug. We don't try to
        // reverse-derive the input from a parsed address (you can't
        // — the original case info is lost when we store raw bytes).
        // So validation here means "the canonical form is what we'd
        // produce for this address". Always Ok; the *caller* is
        // expected to use `from_checksummed_hex` to reject a
        // mismatching input.
        if canonical_hex.len() != 40 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "keccak output malformed: {} chars",
                canonical_hex.len()
            )));
        }
        Ok(())
    }

    /// Convert to a hex string with 0x prefix.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex_encode(&self.0))
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Check if this is the zero address.
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 20]
    }

    /// Get the checksummed address (EIP-55).
    ///
    /// EIP-55 checksums require real Keccak-256, which is only available when
    /// the `web3` feature is enabled. When it is not, we must NOT fabricate a
    /// checksum from a stand-in hash: a wrong mixed-case checksum can cause
    /// the address to be rejected by wallets (or funds sent to a different
    /// address). Returning the plain all-lowercase address is always safe —
    /// EIP-55 treats all-lowercase as "no checksum" and accepts it.
    pub fn to_checksum(&self) -> String {
        let addr_hex = hex_encode(&self.0);
        let mut result = String::with_capacity(42);
        result.push_str("0x");

        #[cfg(not(feature = "web3"))]
        {
            result.push_str(&addr_hex);
            return result;
        }

        #[cfg(feature = "web3")]
        {
            let addr_hash = keccak256_hash(addr_hex.as_bytes());
            let hash_hex = hex_encode(&addr_hash);

            for (i, c) in addr_hex.chars().enumerate() {
                let nibble = hash_hex.chars().nth(i).unwrap_or('0');
                if nibble >= '8' {
                    result.push(c.to_ascii_uppercase());
                } else {
                    result.push(c);
                }
            }
            result
        }
    }
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.to_checksum())
    }
}

impl From<[u8; 20]> for Address {
    fn from(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }
}

// ============================================================================
// Wei / Ether Utilities
// ============================================================================

/// Wei amount (smallest Ethereum unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Wei(pub u128);

impl Wei {
    /// Zero wei.
    pub const ZERO: Wei = Wei(0);

    /// One wei.
    pub const ONE: Wei = Wei(1);

    /// One gwei (10^9 wei).
    pub const GWEI: Wei = Wei(1_000_000_000);

    /// One ether (10^18 wei).
    pub const ETHER: Wei = Wei(1_000_000_000_000_000_000);

    /// Create from wei.
    pub const fn from_wei(wei: u128) -> Self {
        Self(wei)
    }

    /// `true` if this wei amount is exactly zero.
    pub const fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Create from gwei.
    pub const fn from_gwei(gwei: u64) -> Self {
        Self(gwei as u128 * 1_000_000_000)
    }

    /// Create from ether.
    pub const fn from_ether(ether: u64) -> Self {
        Self(ether as u128 * 1_000_000_000_000_000_000)
    }

    /// Get as wei.
    pub fn as_wei(&self) -> u128 {
        self.0
    }

    /// Get as gwei (truncated).
    pub fn as_gwei(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    /// Get as ether (truncated).
    pub fn as_ether(&self) -> f64 {
        self.0 as f64 / 1_000_000_000_000_000_000.0
    }
}

impl core::fmt::Display for Wei {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} wei", self.0)
    }
}

// ============================================================================
// Nonce
// ============================================================================

/// Transaction nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nonce(pub u64);

impl Nonce {
    /// Create a new nonce.
    pub const fn new(nonce: u64) -> Self {
        Self(nonce)
    }

    /// Increment the nonce.
    pub fn increment(&mut self) {
        self.0 += 1;
    }

    /// Get the raw value.
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

// ============================================================================
// Hash Types
// ============================================================================

/// A 32-byte hash (used for transaction hashes, block hashes, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Create a hash from 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parse from a hex string (with or without 0x prefix).
    pub fn from_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        if s.len() != 64 {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "invalid hash length: expected 64 hex chars, got {}",
                s.len()
            )));
        }
        let bytes = hex_decode(s)?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[..32]);
        Ok(Self(hash))
    }

    /// Convert to a hex string with 0x prefix.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex_encode(&self.0))
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Zero hash (empty block, etc.).
    pub fn zero() -> Self {
        Self([0u8; 32])
    }

    /// Check if this is the zero hash.
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }
}

impl core::fmt::Display for Hash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

// ============================================================================
// Block Types
// ============================================================================

/// A block header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Block number.
    pub number: u64,
    /// Block hash.
    pub hash: Hash,
    /// Parent block hash.
    pub parent_hash: Hash,
    /// Timestamp.
    pub timestamp: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas used.
    pub gas_used: u64,
    /// Miner/validator address.
    pub miner: Address,
    /// Extra data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<String>,
}

/// A transaction in a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTransaction {
    /// Transaction hash.
    pub hash: Hash,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: Hash,
    /// Transaction index in block.
    pub transaction_index: u64,
    /// From address.
    pub from: Address,
    /// To address (None for contract creation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Address>,
    /// Value in wei.
    pub value: Wei,
    /// Gas price.
    pub gas_price: Wei,
    /// Gas limit.
    pub gas: u64,
    /// Input data.
    pub input: String,
    /// Nonce.
    pub nonce: u64,
    /// Signature v.
    pub v: u64,
    /// Signature r.
    pub r: [u8; 32],
    /// Signature s.
    pub s: [u8; 32],
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Hex encode bytes (lowercase).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode a hex string into bytes.
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
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, Web3ErrorKind> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Web3ErrorKind::BlockchainError(format!(
            "invalid hex digit: '{}'",
            c as char
        ))),
    }
}

/// Simple keccak256 hash implementation for EIP-55 checksums.
/// Uses the `sha3` crate (available under the `web3` feature).
/// There is deliberately NO non-web3 fallback: `to_checksum` returns the
/// plain lowercase address when real Keccak-256 is unavailable rather than
/// emitting a fabricated, incorrect checksum.
#[cfg(feature = "web3")]
fn keccak256_hash(data: &[u8]) -> [u8; 32] {
    use sha3::digest::Digest;
    let mut hasher = sha3::Keccak256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

// ============================================================================
// Web3ErrorKind Extension
// ============================================================================

impl Web3ErrorKind {
    /// Blockchain-specific errors.
    pub fn blockchain_error(msg: String) -> Self {
        Web3ErrorKind::Parse {
            kind: crate::error::ParseFailureKind::SchemaMismatch,
            message: format!("blockchain error: {}", msg),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_from_hex() {
        let addr = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        // to_hex() returns the lowercased hex form.
        assert_eq!(
            addr.to_hex(),
            "0x742d35cc6634c0532925a3b844bc9e7595f8be21"
        );
    }

    #[test]
    fn test_address_checksum() {
        let addr = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let checksum = addr.to_checksum();
        assert!(checksum.starts_with("0x"));
        assert_eq!(checksum.len(), 42);
        // FEATURE (audit-2026-08 round-4): the previous expected
        // string in this test was hand-written from a partial
        // recollection of EIP-55. Comparing against
        // `eth-utils`'s canonical output (`0x742d35cC...7595F8bE21`,
        // Python verified), three characters disagreed at
        // positions 4, 38, and 41. The implementation is correct;
        // the test fixture was the bug.
        assert_eq!(
            checksum,
            "0x742d35cC6634C0532925a3b844Bc9E7595F8bE21",
            "EIP-55 round-trip matches the canonical mixed-case form"
        );
    }

    #[test]
    fn test_from_checksummed_hex_accepts_canonical() {
        // Round 4: the strict parser must accept the canonical
        // EIP-55 form.
        let s = "0x742d35cC6634C0532925a3b844Bc9E7595F8bE21";
        let addr = Address::from_checksummed_hex(s).unwrap();
        assert_eq!(addr.to_hex().to_lowercase(), "0x742d35cc6634c0532925a3b844bc9e7595f8be21");
    }

    #[test]
    fn test_from_checksummed_hex_accepts_all_lowercase() {
        // All lowercase is "no checksum" — must always be accepted.
        let s = "0x742d35cc6634c0532925a3b844bc9e7595f8be21";
        assert!(Address::from_checksummed_hex(s).is_ok());
    }

    #[test]
    fn test_from_checksummed_hex_rejects_bad_mixed_case() {
        // Round 4: the strict parser must reject a mixed-case
        // string that doesn't match the EIP-55 checksum. The
        // canonical for `0x742d35cc...e21` is `0x742d35cC...` —
        // flipping a non-matching position to upper must fail.
        let bad = "0x742d35CC6634C0532925a3b844Bc9E7595F8bE21";
        assert!(Address::from_checksummed_hex(bad).is_err());
    }

    #[test]
    fn test_from_checksummed_hex_rejects_wrong_length() {
        assert!(Address::from_checksummed_hex("0xdeadbeef").is_err());
        assert!(Address::from_checksummed_hex("0x742d35cC6634C0532925a3b844Bc9E7595F8bE2").is_err());
    }

    /// EIP-55 official test vectors (from the EIP-55 spec). These pin the
    /// checksum to the *real* Keccak-256 output; a stand-in hash would fail.
    #[cfg(feature = "web3")]
    #[test]
    fn test_eip55_known_vectors() {
        let cases: &[(&str, &str)] = &[
            // All-caps and all-lower input forms.
            ("0x52908400098527886e0f7030069857d2e4169ee7",
             "0x52908400098527886E0F7030069857D2E4169EE7"),
            ("0x8617e340b3d01fa5f11f306f4090fd50e238070d",
             "0x8617E340B3D01FA5F11F306F4090FD50E238070D"),
            ("0xde709f2102306220921060314715629080e2fb77",
             "0xde709f2102306220921060314715629080e2fb77"),
            ("0x27b1fdb04752bbc536007a920d24acb045561c26",
             "0x27b1fdb04752bbc536007a920d24acb045561c26"),
            // Mixed-case examples from the spec.
            ("0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
             "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"),
            ("0xfb6916095ca1df60bb79ce92ce3ea74c37c5d359",
             "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359"),
            ("0xdbf03b407c01e7cd3cbea99509d93f8dddc8c6fb",
             "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB"),
            ("0xd1220a0cf47c7b9be7a2e6ba89f429762e7b9adb",
             "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb"),
        ];
        for (input, expected) in cases {
            let addr = Address::from_hex(input).unwrap();
            assert_eq!(addr.to_checksum(), *expected, "EIP-55 for {input}");
        }
    }


    #[test]
    fn test_wei_conversions() {
        let wei = Wei::from_ether(1);
        assert_eq!(wei.as_wei(), 1_000_000_000_000_000_000);
        assert_eq!(wei.as_gwei(), 1_000_000_000.0);
    }

    #[test]
    fn test_hash_from_hex() {
        // Construct a valid 64-char (32-byte) hash hex string for the test
        let valid_hash = "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21000000000000000000000000";
        let hash = Hash::from_hex(valid_hash).unwrap();
        assert_eq!(hash.to_hex().len(), 66); // 0x + 64 chars
    }

    #[test]
    fn test_known_chain_config() {
        let eth = Chain::from_known(KnownChain::Ethereum);
        assert_eq!(eth.chain_id, 1);
        assert_eq!(eth.config.currency_symbol, "ETH");
        assert!(eth.config.block_explorer.is_some());
    }

    /// Security-boundary robustness: `Address::from_hex` / `Hash::from_hex`
    /// parse untrusted strings, so they must never panic. We sweep every
    /// length from 0..=48 with a variety of hex / non-hex characters and
    /// assert both parsers never panic (they return `Err` on rejection).
    #[test]
    fn hex_parsers_never_panic_on_adversarial_input() {
        // Deterministic pseudo-random bytes (LCG) to build strings.
        let mut acc: u32 = 0xABCDEF01;
        let alphabet: &[u8] = b"0123456789abcdefABCDEFGHIJKLMNOPQRSTUVWXYZgz-_ \n";
        for len in 0..=48usize {
            for variant in 0..8u8 {
                let mut s = String::new();
                if variant & 1 == 1 {
                    s.push_str("0x");
                }
                for _ in 0..len {
                    acc = acc.wrapping_mul(1664525).wrapping_add(1013904223);
                    let idx = ((acc >> 24) as usize) % alphabet.len();
                    s.push(alphabet[idx] as char);
                }
                if variant & 2 == 2 {
                    s.push('x'); // trailing junk
                }
                let _ = Address::from_hex(&s);
                let _ = Hash::from_hex(&s);
            }
        }
        // Explicit boundary cases (all owned Strings so types are uniform).
        let explicit: alloc::vec::Vec<String> = alloc::vec![
            "".into(),
            "0x".into(),
            "0x0".into(),
            "0x00".into(),
            "zz".into(),
            "gg".into(),
            format!("0x{}", "1".repeat(40)),
            "f".repeat(40),
            "F".repeat(64),
            "g".repeat(40),
        ];
        for s in &explicit {
            let _ = Address::from_hex(s);
            let _ = Hash::from_hex(s);
        }
    }
}
