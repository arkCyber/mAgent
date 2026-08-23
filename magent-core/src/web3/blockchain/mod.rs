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
            .ok_or(Web3ErrorKind::BlockchainError(
                "RPC URL not configured for this chain".to_string(),
            ))
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
    pub fn to_checksum(&self) -> String {
        let addr_hex = hex_encode(&self.0);
        let addr_hash = keccak256_hash(addr_hex.as_bytes());
        let hash_hex = hex_encode(&addr_hash);

        let mut result = String::with_capacity(42);
        result.push_str("0x");

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
/// Uses a fixed-function implementation suitable for no_std environments.
/// Note: This is a simplified implementation. For production use,
/// add the `tiny-keccak` crate to Cargo.toml.
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

#[cfg(all(feature = "std", not(feature = "web3")))]
fn keccak256_hash(data: &[u8]) -> [u8; 32] {
    // No-crypto fallback: deterministic placeholder hash.
    let mut result = [0u8; 32];
    for (i, &b) in data.iter().enumerate().take(32) {
        result[i] = b.wrapping_add((i as u8).wrapping_mul(31));
    }
    for i in 32..data.len().min(64) {
        let j = (i - 32) % 32;
        result[j] = result[j].wrapping_add(data[i]);
    }
    result
}

#[cfg(not(any(feature = "std", feature = "web3")))]
fn keccak256_hash(data: &[u8]) -> [u8; 32] {
    // Pure no_std fallback.
    let mut result = [0u8; 32];
    for (i, &b) in data.iter().enumerate() {
        let idx = i % 32;
        result[idx] = result[idx].wrapping_add(b);
        result[(idx + 1) % 32] = result[(idx + 1) % 32].wrapping_mul(31).wrapping_add(b);
    }
    for i in 0..32 {
        result[i] = result[i].wrapping_add(result[(i + 7) % 32]).wrapping_mul(3);
    }
    result
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
}
