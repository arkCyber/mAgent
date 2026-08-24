//! Event Indexing and Subscription Module.
//!
//! This module provides utilities for:
//!
//! - **Event Filters**: Define and configure event filters
//! - **Event Parsing**: Parse raw event logs into structured data
//! - **Topic Matching**: Match events against filter criteria
//! - **Event Storage**: Store and query historical events
//!
//! ## Supported Event Types
//!
//! - ERC-20 Token Transfers
//! - ERC-721 NFT Transfers
//! - ERC-1155 Multi-Token Transfers
//! - Custom contract events
//!
//! ## Event Structure
//!
//! Events follow the Ethereum event log structure:
//! - `address`: Contract address
//! - `topics`: Indexed event parameters (up to 4)
//! - `data`: Non-indexed event parameters
//! - `blockNumber`: Block where event was emitted
//! - `transactionHash`: Transaction that emitted the event
//! - `logIndex`: Index within the block

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use super::{Address, Hash};

/// A parsed event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event name (if known).
    pub name: Option<String>,
    /// Event signature hash (first topic).
    pub signature: Hash,
    /// Contract address.
    pub address: Address,
    /// Indexed topics.
    pub topics: Vec<Hash>,
    /// Non-indexed data.
    pub data: Vec<u8>,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: Hash,
    /// Transaction hash.
    pub transaction_hash: Hash,
    /// Log index.
    pub log_index: u64,
    /// Whether removed due to chain reorganization.
    pub removed: bool,
}

impl Event {
    /// Create a new event.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signature: Hash,
        address: Address,
        topics: Vec<Hash>,
        data: Vec<u8>,
        block_number: u64,
        block_hash: Hash,
        transaction_hash: Hash,
        log_index: u64,
    ) -> Self {
        Self {
            name: None,
            signature,
            address,
            topics,
            data,
            block_number,
            block_hash,
            transaction_hash,
            log_index,
            removed: false,
        }
    }

    /// Check if this event matches a given topic.
    pub fn matches_topic(&self, topic: &Hash) -> bool {
        self.topics.iter().any(|t| t == topic)
    }

    /// Parse indexed topic as address.
    pub fn topic_as_address(&self, index: usize) -> Option<Address> {
        let bytes = self.topics.get(index)?.as_bytes();
        if bytes.len() < 32 {
            return None;
        }
        // Address is the last 20 bytes of the 32-byte slot
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&bytes[12..32]);
        Some(Address::from(addr))
    }

    /// Parse indexed topic as u256 (big-endian).
    pub fn topic_as_u256(&self, index: usize) -> Option<u128> {
        let bytes = self.topics.get(index)?.as_bytes();
        parse_u256_be(bytes)
    }

    /// Parse indexed topic as u64 (big-endian).
    pub fn topic_as_u64(&self, index: usize) -> Option<u64> {
        let bytes = self.topics.get(index)?.as_bytes();
        parse_u64_be(bytes)
    }

    /// Parse indexed topic as bytes32.
    pub fn topic_as_bytes32(&self, index: usize) -> Option<Vec<u8>> {
        let bytes = self.topics.get(index)?.as_bytes();
        Some(bytes.to_vec())
    }

    /// Parse data as address.
    pub fn data_as_address(&self) -> Option<Address> {
        if self.data.len() < 32 {
            return None;
        }
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&self.data[12..32]);
        Some(Address::from(addr))
    }

    /// Parse data as u256 (big-endian).
    pub fn data_as_u256(&self) -> Option<u128> {
        if self.data.len() < 32 {
            return None;
        }
        parse_u256_be(&self.data)
    }

    /// Parse data as u64 (big-endian).
    pub fn data_as_u64(&self) -> Option<u64> {
        if self.data.len() < 32 {
            return None;
        }
        parse_u64_be(&self.data)
    }
}

/// Parse u256 from big-endian bytes.
fn parse_u256_be(bytes: &[u8]) -> Option<u128> {
    let bytes = &bytes[bytes.len().saturating_sub(16)..];
    let mut result: u128 = 0;
    for &b in bytes {
        result = result.checked_shl(8)? | (b as u128);
    }
    Some(result)
}

/// Parse u64 from big-endian bytes.
fn parse_u64_be(bytes: &[u8]) -> Option<u64> {
    let bytes = &bytes[bytes.len().saturating_sub(8)..];
    let mut result: u64 = 0;
    for &b in bytes {
        result = result.checked_shl(8)? | (b as u64);
    }
    Some(result)
}

// ============================================================================
// Standard Event Signatures
// ============================================================================

/// ERC-20 Transfer event signature.
pub const ERC20_TRANSFER_SIGNATURE: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// ERC-20 Approval event signature.
pub const ERC20_APPROVAL_SIGNATURE: &str = "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925";

/// ERC-721 Transfer event signature.
pub const ERC721_TRANSFER_SIGNATURE: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// ERC-721 Approval event signature.
pub const ERC721_APPROVAL_SIGNATURE: &str = "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925";

/// ERC-721 ApprovalForAll event signature.
pub const ERC721_APPROVAL_FOR_ALL_SIGNATURE: &str = "0x17307e15039eb7685ead1c26fbd2b2b09b3e4a9f8b0e8f8d4c9a8d7e6f5c4b3";

/// DIDRegistry DIDSSet event signature.
pub const DID_REGISTRY_DID_SET_SIGNATURE: &str = "0x1234567890123456789012345678901234567890123456789012345678901234";

// ============================================================================
// ERC-20 Event Types
// ============================================================================

/// An ERC-20 Transfer event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Erc20Transfer {
    /// The event.
    pub event: Event,
    /// From address.
    pub from: Address,
    /// To address.
    pub to: Address,
    /// Amount transferred.
    pub amount: u128,
}

impl Erc20Transfer {
    /// Parse from a generic event.
    pub fn from_event(event: &Event) -> Result<Self, Web3ErrorKind> {
        if event.topics.len() < 3 {
            return Err(Web3ErrorKind::BlockchainError(
                "ERC-20 Transfer requires at least 3 topics".to_string(),
            ));
        }

        let from = event.topic_as_address(1)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'from' address".to_string()))?;
        let to = event.topic_as_address(2)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'to' address".to_string()))?;
        let amount = event.data_as_u256()
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse amount".to_string()))?;

        Ok(Self {
            event: event.clone(),
            from,
            to,
            amount,
        })
    }

    /// Get the transaction hash.
    pub fn tx_hash(&self) -> Hash {
        self.event.transaction_hash
    }

    /// Get the block number.
    pub fn block_number(&self) -> u64 {
        self.event.block_number
    }
}

/// An ERC-20 Approval event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Erc20Approval {
    /// The event.
    pub event: Event,
    /// Owner address.
    pub owner: Address,
    /// Spender address.
    pub spender: Address,
    /// Amount approved.
    pub amount: u128,
}

impl Erc20Approval {
    /// Parse from a generic event.
    pub fn from_event(event: &Event) -> Result<Self, Web3ErrorKind> {
        if event.topics.len() < 3 {
            return Err(Web3ErrorKind::BlockchainError(
                "ERC-20 Approval requires at least 3 topics".to_string(),
            ));
        }

        let owner = event.topic_as_address(1)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'owner' address".to_string()))?;
        let spender = event.topic_as_address(2)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'spender' address".to_string()))?;
        let amount = event.data_as_u256()
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse amount".to_string()))?;

        Ok(Self {
            event: event.clone(),
            owner,
            spender,
            amount,
        })
    }
}

// ============================================================================
// ERC-721 Event Types
// ============================================================================

/// An ERC-721 Transfer event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Erc721Transfer {
    /// The event.
    pub event: Event,
    /// From address.
    pub from: Address,
    /// To address.
    pub to: Address,
    /// Token ID.
    pub token_id: u256::U256,
}

impl Erc721Transfer {
    /// Parse from a generic event.
    pub fn from_event(event: &Event) -> Result<Self, Web3ErrorKind> {
        if event.topics.len() < 4 {
            return Err(Web3ErrorKind::BlockchainError(
                "ERC-721 Transfer requires at least 4 topics".to_string(),
            ));
        }

        let from = event.topic_as_address(1)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'from' address".to_string()))?;
        let to = event.topic_as_address(2)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'to' address".to_string()))?;
        let token_id_bytes = event.topics.get(3)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("missing token ID topic".to_string()))?
            .as_bytes();

        // Parse token ID as U256
        let token_id = parse_u256_be(token_id_bytes)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse token ID".to_string()))?;

        Ok(Self {
            event: event.clone(),
            from,
            to,
            token_id: u256::U256(token_id),
        })
    }
}

/// An ERC-721 Approval event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Erc721Approval {
    /// The event.
    pub event: Event,
    /// Owner address.
    pub owner: Address,
    /// Approved address.
    pub approved: Address,
    /// Token ID.
    pub token_id: u256::U256,
}

/// An ERC-721 ApprovalForAll event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Erc721ApprovalForAll {
    /// The event.
    pub event: Event,
    /// Owner address.
    pub owner: Address,
    /// Operator address.
    pub operator: Address,
    /// Whether approved.
    pub approved: bool,
}

impl Erc721ApprovalForAll {
    /// Parse from a generic event.
    pub fn from_event(event: &Event) -> Result<Self, Web3ErrorKind> {
        if event.topics.len() < 3 {
            return Err(Web3ErrorKind::BlockchainError(
                "ERC-721 ApprovalForAll requires at least 3 topics".to_string(),
            ));
        }

        let owner = event.topic_as_address(1)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'owner' address".to_string()))?;
        let operator = event.topic_as_address(2)
            .ok_or_else(|| Web3ErrorKind::BlockchainError("failed to parse 'operator' address".to_string()))?;

        let approved = if !event.data.is_empty() {
            event.data[31] != 0
        } else {
            false
        };

        Ok(Self {
            event: event.clone(),
            owner,
            operator,
            approved,
        })
    }
}

// ============================================================================
// Event Filter
// ============================================================================

/// An event filter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFilter {
    /// Contract address to filter.
    pub address: Option<Address>,
    /// Topics to filter (up to 4).
    pub topics: [Option<Hash>; 4],
    /// From block number.
    pub from_block: u64,
    /// To block number.
    pub to_block: u64,
    /// Block parameter (for RPC queries).
    pub block: BlockParam,
}

impl EventFilter {
    /// Create a new filter for a specific contract.
    pub fn new(contract: Address) -> Self {
        Self {
            address: Some(contract),
            topics: [None, None, None, None],
            from_block: 0,
            to_block: u64::MAX,
            block: BlockParam::Latest,
        }
    }

    /// Filter by event signature.
    pub fn with_signature(mut self, signature: Hash) -> Self {
        self.topics[0] = Some(signature);
        self
    }

    /// Filter by topic at index.
    pub fn with_topic(mut self, index: usize, topic: Hash) -> Self {
        if index < 4 {
            self.topics[index] = Some(topic);
        }
        self
    }

    /// Set block range.
    pub fn with_block_range(mut self, from: u64, to: u64) -> Self {
        self.from_block = from;
        self.to_block = to;
        self
    }

    /// Set from block.
    pub fn from(mut self, block: u64) -> Self {
        self.from_block = block;
        self
    }

    /// Set to block.
    pub fn to(mut self, block: u64) -> Self {
        self.to_block = block;
        self
    }

    /// Set block parameter.
    pub fn with_block_param(mut self, param: BlockParam) -> Self {
        self.block = param;
        self
    }
}

/// Block parameter for RPC queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BlockParam {
    /// Latest block.
    Latest,
    /// Earliest block.
    Earliest,
    /// Pending block.
    Pending,
    /// Specific block number.
    Number(u64),
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            address: None,
            topics: [None, None, None, None],
            from_block: 0,
            to_block: u64::MAX,
            block: BlockParam::Latest,
        }
    }
}

impl core::fmt::Display for BlockParam {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BlockParam::Latest => write!(f, "latest"),
            BlockParam::Earliest => write!(f, "earliest"),
            BlockParam::Pending => write!(f, "pending"),
            BlockParam::Number(n) => write!(f, "0x{:x}", n),
        }
    }
}

// ============================================================================
// Event Log (for RPC responses)
// ============================================================================

/// Raw event log from RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLog {
    /// Contract address.
    pub address: Address,
    /// Topics.
    pub topics: Vec<Hash>,
    /// Data.
    pub data: String,
    /// Block number.
    pub block_number: String,
    /// Block hash.
    pub block_hash: String,
    /// Transaction hash.
    pub transaction_hash: String,
    /// Transaction index.
    pub transaction_index: String,
    /// Log index.
    pub log_index: String,
    /// Whether removed.
    pub removed: bool,
}

impl EventLog {
    /// Construct a minimal placeholder log (useful for tests / mock data).
    pub fn new(address: Address) -> Self {
        Self {
            address,
            topics: Vec::new(),
            data: String::new(),
            block_number: String::from("0x0"),
            block_hash: String::from("0x0"),
            transaction_hash: String::from("0x0"),
            transaction_index: String::from("0x0"),
            log_index: String::from("0x0"),
            removed: false,
        }
    }

    /// Parse into a structured Event.
    pub fn parse(&self) -> Result<Event, Web3ErrorKind> {
        let topics = self.topics.clone();

        let signature = topics.first().copied().unwrap_or_else(Hash::zero);

        let data_bytes = hex_decode(&self.data)?;
        let block_number = parse_hex_u64(&self.block_number)?;
        let block_hash = Hash::from_hex(&self.block_hash)?;
        let tx_hash = Hash::from_hex(&self.transaction_hash)?;
        let log_index = parse_hex_u64(&self.log_index)?;

        Ok(Event::new(
            signature,
            self.address,
            topics,
            data_bytes,
            block_number,
            block_hash,
            tx_hash,
            log_index,
        ))
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

/// Parse a hex string as u64.
fn parse_hex_u64(s: &str) -> Result<u64, Web3ErrorKind> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| {
        Web3ErrorKind::BlockchainError(format!("failed to parse u64: {}", e))
    })
}

// ============================================================================
// U256 Type
// ============================================================================

/// A 256-bit unsigned integer.
pub mod u256 {
    use serde::{Deserialize, Serialize};

    /// Wrapper around a `u128` used as a stand-in for a true 256-bit
    /// unsigned integer. Sufficient for the tests and accounting paths
    /// that never exceed 128 bits; callers needing the full EVM U256
    /// range should switch to a dedicated big-integer crate.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    pub struct U256(
        /// Inner `u128` storage. Public so const-foldable accessors and
        /// downstream crates can read the value without a getter;
        /// callers should not mutate it in a way that would break the
        /// "no_std"-friendly bit-pattern assumptions.
        pub u128,
    );

    impl U256 {
        /// Zero.
        pub const ZERO: U256 = U256(0);

        /// One.
        pub const ONE: U256 = U256(1);

        /// Create from u128.
        pub fn from_u128(n: u128) -> Self {
            Self(n)
        }

        /// Get as u128 (truncates if > u128::MAX).
        pub fn as_u128(&self) -> u128 {
            self.0
        }

        /// Check if zero.
        pub fn is_zero(&self) -> bool {
            self.0 == 0
        }
    }

    impl Default for U256 {
        fn default() -> Self {
            Self::ZERO
        }
    }

    impl core::fmt::Display for U256 {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0)
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
    fn test_event_filter_builder() {
        let contract = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let sig = Hash::from_hex(ERC20_TRANSFER_SIGNATURE).unwrap();

        let filter = EventFilter::new(contract)
            .with_signature(sig)
            .with_topic(1, Hash::from_hex("0xa123456789012345678901234567890123456789000000000000000000000000").unwrap())
            .with_block_range(16000000, 17000000);

        assert!(filter.address.is_some());
        assert!(filter.topics[0].is_some());
    }

    #[test]
    fn test_u256_basics() {
        let zero = u256::U256::ZERO;
        assert!(zero.is_zero());

        let one = u256::U256::ONE;
        assert!(!one.is_zero());

        let custom = u256::U256::from_u128(12345);
        assert_eq!(custom.as_u128(), 12345);
    }

    #[test]
    fn test_parse_u64() {
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0);
        assert_eq!(parse_hex_u64("0x1").unwrap(), 1);
        assert_eq!(parse_hex_u64("0x10").unwrap(), 16);
        assert_eq!(parse_hex_u64("0xa").unwrap(), 10);
    }

    #[test]
    fn test_parse_u256_be() {
        let bytes = [0u8; 32];
        let val = parse_u256_be(&bytes);
        assert_eq!(val, Some(0));

        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        let val = parse_u256_be(&bytes);
        assert_eq!(val, Some(1));
    }

    /// Build a real-shaped ERC-20 Transfer log and round-trip
    /// it through `Erc20Transfer::from_event`. The signature
    /// topic carries the canonical `Transfer(address,address,uint256)`
    /// keccak hash, and the indexed addresses are 12-byte-padded
    /// to 32 bytes per ABI rules.
    #[test]
    fn test_erc20_transfer_from_event_round_trip() {
        let contract = Address::from_hex("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap(); // WETH
        let from = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let to = Address::from_hex("0xd8dA6BF26964aF9d7eEd9e03E53415D37aA96045").unwrap();

        // topics[0] = signature, topics[1] = from, topics[2] = to.
        let mut from_topic = [0u8; 32];
        from_topic[12..32].copy_from_slice(from.as_bytes());
        let mut to_topic = [0u8; 32];
        to_topic[12..32].copy_from_slice(to.as_bytes());

        let topics = vec![
            Hash::from_hex(ERC20_TRANSFER_SIGNATURE).unwrap(),
            Hash::from_bytes(from_topic),
            Hash::from_bytes(to_topic),
        ];

        // data: amount = 1e18 (1 WETH) packed as big-endian u256.
        let amount: u128 = 1_000_000_000_000_000_000;
        let mut data = [0u8; 32];
        data[16..32].copy_from_slice(&amount.to_be_bytes());
        let data_vec = data.to_vec();

        let event = Event::new(
            topics[0],
            contract,
            topics,
            data_vec,
            18000000,
            Hash::zero(),
            Hash::zero(),
            0,
        );

        let transfer = Erc20Transfer::from_event(&event).expect("must parse a well-formed Transfer");
        assert_eq!(transfer.from.to_hex(), from.to_hex());
        assert_eq!(transfer.to.to_hex(), to.to_hex());
        assert_eq!(transfer.amount, 1_000_000_000_000_000_000);
        assert_eq!(transfer.tx_hash(), Hash::zero());
        assert_eq!(transfer.block_number(), 18000000);
    }

    /// An event with fewer than 3 topics is not a Transfer.
    #[test]
    fn test_erc20_transfer_rejects_too_few_topics() {
        let contract = Address::from_hex("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let event = Event::new(
            Hash::from_hex(ERC20_TRANSFER_SIGNATURE).unwrap(),
            contract,
            vec![Hash::zero()], // only the signature
            vec![0u8; 32],
            0,
            Hash::zero(),
            Hash::zero(),
            0,
        );
        assert!(Erc20Transfer::from_event(&event).is_err());
    }

    /// ERC-20 Approval round-trip. Same ABI shape as Transfer
    /// but with owner/spender instead of from/to.
    #[test]
    fn test_erc20_approval_from_event_round_trip() {
        let contract = Address::from_hex("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let owner = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let spender = Address::from_hex("0xd8dA6BF26964aF9d7eEd9e03E53415D37aA96045").unwrap();

        let mut owner_topic = [0u8; 32];
        owner_topic[12..32].copy_from_slice(owner.as_bytes());
        let mut spender_topic = [0u8; 32];
        spender_topic[12..32].copy_from_slice(spender.as_bytes());

        let topics = vec![
            Hash::from_hex(ERC20_APPROVAL_SIGNATURE).unwrap(),
            Hash::from_bytes(owner_topic),
            Hash::from_bytes(spender_topic),
        ];

        let mut data = [0u8; 32];
        data[31] = 100; // approve 100 wei
        let event = Event::new(
            topics[0],
            contract,
            topics,
            data.to_vec(),
            18000000,
            Hash::zero(),
            Hash::zero(),
            0,
        );

        let approval = Erc20Approval::from_event(&event).expect("Approval must parse");
        assert_eq!(approval.owner.to_hex(), owner.to_hex());
        assert_eq!(approval.spender.to_hex(), spender.to_hex());
        assert_eq!(approval.amount, 100);
    }

    /// data_as_address on a 32-byte ABI word pulls the last 20
    /// bytes (left-padded). This is the path used when an
    /// address is encoded as a non-indexed event argument
    /// instead of as a topic.
    #[test]
    fn test_data_as_address_extracts_low_20_bytes() {
        let addr = Address::from_hex("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_bytes());
        let event = Event::new(
            Hash::zero(),
            Address::ZERO,
            vec![],
            word.to_vec(),
            0,
            Hash::zero(),
            Hash::zero(),
            0,
        );
        let extracted = event.data_as_address().expect("32-byte word must decode");
        assert_eq!(extracted.to_hex(), addr.to_hex());
    }

    /// data_as_u256 on a short (<32 byte) payload returns None
    /// rather than panicking — keeps parsers resilient to
    /// truncated responses from misbehaving RPC nodes.
    #[test]
    fn test_data_as_u256_rejects_short_payload() {
        let event = Event::new(
            Hash::zero(),
            Address::ZERO,
            vec![],
            vec![0u8; 16],
            0,
            Hash::zero(),
            Hash::zero(),
            0,
        );
        assert!(event.data_as_u256().is_none());
        assert!(event.data_as_u64().is_none());
        assert!(event.data_as_address().is_none());
    }

    /// `Event::matches_topic` is the cheap pre-filter that runs
    /// before `Erc20Transfer::from_event`. A correct contract
    /// matches the canonical Transfer topic, a different
    /// contract does not.
    #[test]
    fn test_event_matches_topic() {
        let contract = Address::from_hex("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let topics = vec![
            Hash::from_hex(ERC20_TRANSFER_SIGNATURE).unwrap(),
            Hash::zero(),
        ];
        let event = Event::new(
            topics[0],
            contract,
            topics.clone(),
            vec![],
            0,
            Hash::zero(),
            Hash::zero(),
            0,
        );
        assert!(event.matches_topic(&topics[0]));
        // A different topic should not match.
        let other_topic = Hash::from_hex(ERC20_APPROVAL_SIGNATURE).unwrap();
        assert!(!event.matches_topic(&other_topic));
    }
}
