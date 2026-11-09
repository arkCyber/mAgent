//! Blockchain RPC client abstraction.
//!
//! This module defines the [`ChainClient`] trait — the shared interface for
//! blockchain RPC clients — and re-exports the receipt/log types its methods
//! return. The concrete HTTP-based EVM client that implements this trait is
//! [`HttpRpcClient`](super::http_client::HttpRpcClient), which is `std`-only.
//!
//! A second, non-functional `EvmRpcClient` placeholder used to live here. It
//! was dead code (never referenced anywhere, `#[allow(dead_code)]`-hidden)
//! whose every RPC method returned `Err("RPC not configured")`, and it
//! duplicated the working `HttpRpcClient`. It was removed in audit round 26.

use alloc::vec::Vec;

use super::{Address, Block, Hash, Wei};
use crate::error::Web3ErrorKind;

/// Chain identifier type.
pub type ChainId = u64;

/// Result type for blockchain operations.
pub type BlockchainResult<T> = core::result::Result<T, Web3ErrorKind>;

/// A trait for blockchain RPC clients.
///
/// Implement this trait to add support for different blockchain networks
/// (Ethereum, Solana, etc.).
///
/// Note: This trait is designed for single-threaded environments.
/// Implementations should not assume multi-threaded access.
pub trait ChainClient {
    /// Get the chain ID this client is connected to.
    fn chain_id(&self) -> ChainId;

    /// Get the RPC endpoint URL.
    fn rpc_url(&self) -> &str;

    /// Get account nonce (transaction count).
    fn get_nonce(&self, address: &Address) -> BlockchainResult<u64>;

    /// Get current gas price.
    fn get_gas_price(&self) -> BlockchainResult<Wei>;

    /// Get estimated gas for a transaction.
    fn estimate_gas(&self, to: &Address, data: &[u8], value: Wei) -> BlockchainResult<u64>;

    /// Get balance of an address.
    fn get_balance(&self, address: &Address) -> BlockchainResult<Wei>;

    /// Get code at an address (for contract verification).
    fn get_code(&self, address: &Address) -> BlockchainResult<Vec<u8>>;

    /// Get transaction receipt.
    fn get_transaction_receipt(
        &self,
        tx_hash: &Hash,
    ) -> BlockchainResult<Option<TransactionReceipt>>;

    /// Get block by number.
    fn get_block(&self, block_number: u64) -> BlockchainResult<Option<Block>>;

    /// Get latest block number.
    fn get_block_number(&self) -> BlockchainResult<u64>;

    /// Call a contract (read-only).
    fn call(&self, to: &Address, data: &[u8]) -> BlockchainResult<Vec<u8>>;

    /// Send a signed transaction.
    fn send_raw_transaction(&self, signed_tx: &[u8]) -> BlockchainResult<Hash>;

    /// Get logs (events).
    fn get_logs(&self, filter: &EventFilter) -> BlockchainResult<Vec<EventLog>>;

    /// Check if the client is connected and responsive.
    fn health_check(&self) -> BlockchainResult<()>;
}

/// Transaction receipt.
pub use super::transaction::TransactionReceipt;

/// Event log entry.
pub use super::events::EventLog;

/// Event filter for querying logs.
pub use super::events::EventFilter;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_filter_builder() {
        let contract = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let filter = EventFilter::new(contract).with_block_range(1000000, 2000000);

        assert!(filter.address.is_some());
        assert_eq!(filter.from_block, 1000000);
        assert_eq!(filter.to_block, 2000000);
    }
}
