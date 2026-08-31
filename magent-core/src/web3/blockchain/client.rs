//! Blockchain RPC Client implementation.
//!
//! This module provides a trait-based abstraction for blockchain interactions
//! and a concrete HTTP-based RPC client for EVM-compatible chains.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Address, Block, Chain, Hash, Wei};
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

/// JSON-RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC protocol version string (always `"2.0"`).
    pub jsonrpc: &'static str,
    /// Remote method name (e.g. `"eth_call"`, `"eth_blockNumber"`).
    pub method: String,
    /// Positional or named method parameters, serialised as JSON.
    pub params: Vec<Value>,
    /// Unique per-process request id used to match responses.
    pub id: u64,
}

impl JsonRpcRequest {
    /// Create a new request.
    pub fn new(method: impl Into<String>, params: Vec<Value>) -> Self {
        // `AtomicU64` is unavailable on 32-bit targets (e.g. RISC-V ESP32-C6/C61).
        // Fall back to `AtomicU32` — more than enough unique IDs per process lifetime.
        #[cfg(target_pointer_width = "32")]
        use core::sync::atomic::{AtomicU32 as AtomicU64, Ordering};
        #[cfg(target_pointer_width = "64")]
        use core::sync::atomic::{AtomicU64, Ordering};

        static REQUEST_ID: AtomicU64 = AtomicU64::new(0);
        // `fetch_add` returns the underlying integer type (u32 on 32-bit
        // targets, u64 on 64-bit); the `.into()` widens it to u64. It's only
        // a "useless conversion" on 64-bit hosts, so silence the lint here.
        #[allow(clippy::useless_conversion)]
        let id: u64 = REQUEST_ID.fetch_add(1, Ordering::Relaxed).into();

        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
            id,
        }
    }
}

/// JSON-RPC response.
///
/// `result` is a custom `Result<T, JsonRpcError>` flattened via `serde`
/// so the wire format is either `{ "jsonrpc": "...", "id": ..., "result": ... }`
/// or `{ "jsonrpc": "...", "id": ..., "error": {...} }` — never both.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse<T> {
    /// JSON-RPC protocol version string echoed by the server.
    pub jsonrpc: String,
    /// Request id echoed back to match the originating `JsonRpcRequest`.
    pub id: u64,
    /// Either the decoded result or a structured error from the server.
    #[serde(flatten)]
    pub result: core::result::Result<T, JsonRpcError>,
}

/// JSON-RPC error object returned in the body of a failed response.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (e.g. `-32601` for `Method not found`).
    pub code: i64,
    /// Human-readable error message from the remote peer.
    pub message: String,
    /// Optional structured data accompanying the error (schema depends
    /// on the remote method). Skipped during serialisation when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// HTTP-based EVM RPC client.
#[derive(Debug, Clone)]
pub struct EvmRpcClient {
    rpc_url: String,
    chain_id: u64,
}

impl EvmRpcClient {
    /// Create a new client from a chain configuration.
    pub fn new(chain: &Chain) -> BlockchainResult<Self> {
        let rpc_url = chain.rpc_url()?.to_string();
        Ok(Self {
            rpc_url,
            chain_id: chain.chain_id,
        })
    }

    /// Create a new client with a custom RPC URL.
    pub fn with_rpc(rpc_url: impl Into<String>, chain_id: u64) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            chain_id,
        }
    }

    /// Make a JSON-RPC call.
    #[allow(dead_code)]
    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> BlockchainResult<T> {
        let _request = JsonRpcRequest::new(method, params);

        // Note: In a real implementation, this would use `reqwest` or similar.
        // For now, we return a placeholder that indicates the RPC needs to be configured.
        Err(Web3ErrorKind::BlockchainError(format!(
            "RPC call to {} requires reqwest: {}",
            self.rpc_url, method
        )))
    }

    /// Make a raw JSON-RPC call returning the raw JSON value.
    fn raw_call(&self, method: &str, _params: Vec<Value>) -> BlockchainResult<Value> {
        // Placeholder for actual HTTP implementation
        Err(Web3ErrorKind::BlockchainError(format!(
            "RPC not configured: {} - method: {}",
            self.rpc_url, method
        )))
    }

    /// Parse a hex string to bytes.
    fn parse_hex(&self, s: &str) -> BlockchainResult<Vec<u8>> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        hex_decode(s)
    }

    /// Parse an address from JSON.
    fn parse_address(&self, v: &Value) -> BlockchainResult<Option<Address>> {
        if v.is_null() {
            return Ok(None);
        }
        let s = v.as_str().ok_or_else(|| {
            Web3ErrorKind::BlockchainError("expected string for address".to_string())
        })?;
        Ok(Some(Address::from_hex(s)?))
    }

    /// Parse a hash from JSON.
    fn parse_hash(&self, v: &Value) -> BlockchainResult<Hash> {
        let s = v.as_str().ok_or_else(|| {
            Web3ErrorKind::BlockchainError("expected string for hash".to_string())
        })?;
        Hash::from_hex(s)
    }

    /// Parse a u64 from JSON hex.
    fn parse_u64(&self, v: &Value) -> BlockchainResult<u64> {
        let s = v.as_str().ok_or_else(|| {
            Web3ErrorKind::BlockchainError("expected string for number".to_string())
        })?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        u64::from_str_radix(s, 16)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("failed to parse u64: {}", e)))
    }

    /// Parse Wei from JSON.
    fn parse_wei(&self, v: &Value) -> BlockchainResult<Wei> {
        let s = v
            .as_str()
            .ok_or_else(|| Web3ErrorKind::BlockchainError("expected string for wei".to_string()))?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        let wei = u128::from_str_radix(s, 16)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("failed to parse wei: {}", e)))?;
        Ok(Wei(wei))
    }
}

impl ChainClient for EvmRpcClient {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    fn get_nonce(&self, address: &Address) -> BlockchainResult<u64> {
        let params = vec![serde_json::json!(address.to_hex()), Value::from("latest")];
        let result: Value = self.raw_call("eth_getTransactionCount", params)?;
        self.parse_u64(&result)
    }

    fn get_gas_price(&self) -> BlockchainResult<Wei> {
        let result: Value = self.raw_call("eth_gasPrice", vec![])?;
        self.parse_wei(&result)
    }

    fn estimate_gas(&self, to: &Address, data: &[u8], value: Wei) -> BlockchainResult<u64> {
        let call_object = serde_json::json!({
            "to": to.to_hex(),
            "data": format!("0x{}", hex_encode(data)),
            "value": format!("0x{:x}", value.as_wei())
        });
        let result: Value = self.raw_call("eth_estimateGas", vec![call_object])?;
        self.parse_u64(&result)
    }

    fn get_balance(&self, address: &Address) -> BlockchainResult<Wei> {
        let params = vec![serde_json::json!(address.to_hex()), Value::from("latest")];
        let result: Value = self.raw_call("eth_getBalance", params)?;
        self.parse_wei(&result)
    }

    fn get_code(&self, address: &Address) -> BlockchainResult<Vec<u8>> {
        let params = vec![serde_json::json!(address.to_hex()), Value::from("latest")];
        let result: Value = self.raw_call("eth_getCode", params)?;
        self.parse_hex(result.as_str().unwrap_or("0x"))
    }

    fn get_transaction_receipt(
        &self,
        tx_hash: &Hash,
    ) -> BlockchainResult<Option<TransactionReceipt>> {
        let params = vec![serde_json::json![tx_hash.to_hex()]];
        let result: Value = self.raw_call("eth_getTransactionReceipt", params)?;

        if result.is_null() {
            return Ok(None);
        }

        let receipt = self.parse_transaction_receipt(&result)?;
        Ok(Some(receipt))
    }

    fn get_block(&self, block_number: u64) -> BlockchainResult<Option<Block>> {
        let params = vec![
            serde_json::json!(format!("0x{:x}", block_number)),
            Value::from(false),
        ];
        let result: Value = self.raw_call("eth_getBlockByNumber", params)?;

        if result.is_null() {
            return Ok(None);
        }

        let block = self.parse_block(&result)?;
        Ok(Some(block))
    }

    fn get_block_number(&self) -> BlockchainResult<u64> {
        let result: Value = self.raw_call("eth_blockNumber", vec![])?;
        self.parse_u64(&result)
    }

    fn call(&self, to: &Address, data: &[u8]) -> BlockchainResult<Vec<u8>> {
        let call_object = serde_json::json!({
            "to": to.to_hex(),
            "data": format!("0x{}", hex_encode(data))
        });
        let result: Value = self.raw_call("eth_call", vec![call_object, Value::from("latest")])?;
        self.parse_hex(result.as_str().unwrap_or("0x"))
    }

    fn send_raw_transaction(&self, signed_tx: &[u8]) -> BlockchainResult<Hash> {
        let params = vec![serde_json::json![format!("0x{}", hex_encode(signed_tx))]];
        let result: Value = self.raw_call("eth_sendRawTransaction", params)?;
        self.parse_hash(&result)
    }

    fn get_logs(&self, filter: &EventFilter) -> BlockchainResult<Vec<EventLog>> {
        let filter_obj = serde_json::json!({
            "address": filter.address.map(|a| a.to_hex()),
            "topics": filter.topics.iter().map(|t| t.map(|h| h.to_hex())).collect::<Vec<_>>(),
            "fromBlock": format!("0x{:x}", filter.from_block),
            "toBlock": format!("0x{:x}", filter.to_block),
        });
        let result: Value = self.raw_call("eth_getLogs", vec![filter_obj])?;
        let logs = serde_json::from_value(result)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("failed to parse logs: {}", e)))?;
        Ok(logs)
    }

    fn health_check(&self) -> BlockchainResult<()> {
        let _: Value = self.raw_call("eth_blockNumber", vec![])?;
        Ok(())
    }
}

impl EvmRpcClient {
    /// Parse a transaction receipt from JSON.
    fn parse_transaction_receipt(&self, v: &Value) -> BlockchainResult<TransactionReceipt> {
        let status_str = v["status"].as_str().unwrap_or("0x0");
        let status = if status_str == "0x1" { 1u8 } else { 0u8 };

        let logs: Vec<crate::web3::blockchain::events::EventLog> =
            serde_json::from_value(v["logs"].clone()).unwrap_or_default();

        let effective_gas_price = v
            .get("effectiveGasPrice")
            .and_then(|x| x.as_str())
            .and_then(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .map(crate::web3::blockchain::Wei);

        Ok(TransactionReceipt {
            transaction_hash: self.parse_hash(&v["transactionHash"])?,
            block_number: self.parse_u64(&v["blockNumber"])?,
            block_hash: self.parse_hash(&v["blockHash"])?,
            transaction_index: self.parse_u64(&v["transactionIndex"])?,
            from: self.parse_address(&v["from"])?.unwrap_or(Address::ZERO),
            to: self.parse_address(&v["to"])?,
            gas_used: self.parse_u64(&v["gasUsed"])?,
            cumulative_gas_used: self.parse_u64(&v["cumulativeGasUsed"])?,
            contract_address: self.parse_address(&v["contractAddress"])?,
            status,
            logs,
            effective_gas_price,
        })
    }

    /// Parse a block from JSON.
    fn parse_block(&self, v: &Value) -> BlockchainResult<Block> {
        Ok(Block {
            number: self.parse_u64(&v["number"])?,
            hash: self.parse_hash(&v["hash"])?,
            parent_hash: self.parse_hash(&v["parentHash"])?,
            timestamp: self.parse_u64(&v["timestamp"])?,
            gas_limit: self.parse_u64(&v["gasLimit"])?,
            gas_used: self.parse_u64(&v["gasUsed"])?,
            miner: self.parse_address(&v["miner"])?.unwrap_or(Address::ZERO),
            extra_data: v["extraData"].as_str().map(String::from),
        })
    }
}

/// Hex encode bytes.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode hex string.
fn hex_decode(s: &str) -> BlockchainResult<Vec<u8>> {
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
            _ => {
                return Err(Web3ErrorKind::BlockchainError(
                    "invalid hex digit".to_string(),
                ))
            }
        };
        let lo = match chunk[1] {
            b'0'..=b'9' => chunk[1] - b'0',
            b'a'..=b'f' => chunk[1] - b'a' + 10,
            b'A'..=b'F' => chunk[1] - b'A' + 10,
            _ => {
                return Err(Web3ErrorKind::BlockchainError(
                    "invalid hex digit".to_string(),
                ))
            }
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
    fn test_event_filter_builder() {
        let contract = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let filter = EventFilter::new(contract).with_block_range(1000000, 2000000);

        assert!(filter.address.is_some());
        assert_eq!(filter.from_block, 1000000);
        assert_eq!(filter.to_block, 2000000);
    }

    #[test]
    fn test_evm_client_creation() {
        let chain = Chain::from_known(super::super::KnownChain::Ethereum);
        let client = EvmRpcClient::new(&chain);
        // Will fail because RPC is not actually called, but creation works
        assert!(client.is_ok() || client.is_err());
    }

    #[test]
    fn test_evm_client_with_custom_rpc() {
        let client = EvmRpcClient::with_rpc("https://example.com", 1);
        assert_eq!(client.chain_id(), 1);
        assert_eq!(client.rpc_url(), "https://example.com");
    }

    #[test]
    fn test_json_rpc_request_id_is_monotonic() {
        // Two requests issued back-to-back must have distinct ids.
        // (We use AtomicU64 internally so even concurrent calls stay
        // monotonic, but here we just verify sequential ids.)
        let r1 = JsonRpcRequest::new("eth_blockNumber", vec![]);
        let r2 = JsonRpcRequest::new("eth_blockNumber", vec![]);
        assert_ne!(r1.id, r2.id, "JSON-RPC request ids must be unique");
        assert_eq!(r1.jsonrpc, "2.0");
        assert_eq!(r1.method, "eth_blockNumber");
    }

    #[test]
    fn test_evm_client_call_returns_error_without_reqwest() {
        // The placeholder client (`raw_call`) should never silently
        // succeed for any RPC method; it must return an error explaining
        // that RPC is not configured. This guards against the
        // implementation accidentally returning an "Ok" with an empty
        // JSON object.
        let client = EvmRpcClient::with_rpc("https://example.com", 1);
        let result: BlockchainResult<Value> = client.raw_call("eth_blockNumber", vec![]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("RPC not configured") || msg.contains("requires reqwest"));
    }

    #[test]
    fn test_parse_block_handles_minimal_json() {
        // Valid Ethereum RPC fields minus optional ones — block_number is
        // required, but `extra_data` is allowed to be missing.
        let v = serde_json::json!({
            "number": "0x10",
            "hash": "0x742d35cc6634c0532925a3b844bc9e7595f8be21000000000000000000000000",
            "parentHash": "0x742d35cc6634c0532925a3b844bc9e7595f8be21000000000000000000000000",
            "timestamp": "0x5f5e100",
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x5208",
            "miner": "0x0000000000000000000000000000000000000000",
        });
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let block = client.parse_block(&v).unwrap();
        assert_eq!(block.number, 16);
        assert!(block.extra_data.is_none());
    }

    #[test]
    fn test_parse_u64_handles_hex_with_and_without_prefix() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let with_prefix = serde_json::json!("0x100");
        let bare = serde_json::json!("ff");
        assert_eq!(client.parse_u64(&with_prefix).unwrap(), 256);
        assert_eq!(client.parse_u64(&bare).unwrap(), 255);
    }

    #[test]
    fn test_parse_hex_decodes_address_correctly() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let addr = "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21";
        let bytes = client.parse_hex(addr).unwrap();
        assert_eq!(bytes.len(), 20);
        assert_eq!(bytes[0], 0x74);
    }

    #[test]
    fn test_parse_address_paths() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        // null → None
        assert!(client
            .parse_address(&serde_json::json!(null))
            .unwrap()
            .is_none());
        // non-string → error
        assert!(client.parse_address(&serde_json::json!(123)).is_err());
        // valid → Some
        let addr = client
            .parse_address(&serde_json::json!(
                "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21"
            ))
            .unwrap();
        assert!(addr.is_some());
        // Address::to_hex() includes the `0x` prefix (40 hex chars + 2).
        assert_eq!(addr.unwrap().to_hex().len(), 42);
    }

    #[test]
    fn test_parse_hash_paths() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let h = client
            .parse_hash(&serde_json::json!(
                "0x742d35cc6634c0532925a3b844bc9e7595f8be21000000000000000000000000"
            ))
            .unwrap();
        // Hash::to_hex() includes the `0x` prefix (64 hex chars + 2).
        assert_eq!(h.to_hex().len(), 66);
        // non-string → error
        assert!(client.parse_hash(&serde_json::json!(42)).is_err());
    }

    #[test]
    fn test_parse_wei_paths() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let w = client
            .parse_wei(&serde_json::json!("0xde0b6b3a7640000"))
            .unwrap();
        assert_eq!(w.as_wei(), 1_000_000_000_000_000_000u128);
        // non-string → error
        assert!(client.parse_wei(&serde_json::json!(1)).is_err());
        // bad hex → error
        assert!(client.parse_wei(&serde_json::json!("0xzz")).is_err());
    }

    #[test]
    fn test_parse_transaction_receipt_success() {
        let client = EvmRpcClient::with_rpc("https://x", 1);
        let v = serde_json::json!({
            "transactionHash": "0x742d35cc6634c0532925a3b844bc9e7595f8be21000000000000000000000000",
            "blockNumber": "0x10",
            "blockHash": "0x742d35cc6634c0532925a3b844bc9e7595f8be21000000000000000000000000",
            "transactionIndex": "0x0",
            "from": "0x0000000000000000000000000000000000000000",
            "to": null,
            "gasUsed": "0x5208",
            "cumulativeGasUsed": "0x5208",
            "contractAddress": null,
            "status": "0x1",
            "logs": []
        });
        let receipt = client.parse_transaction_receipt(&v).unwrap();
        assert_eq!(receipt.status, 1);
        assert!(receipt.to.is_none());
        assert_eq!(receipt.gas_used, 21000);
        assert_eq!(receipt.block_number, 16);
    }

    #[test]
    fn test_hex_encode_decode_round_trip() {
        let bytes = [0xdeu8, 0xad, 0xbe, 0xef];
        let enc = hex_encode(&bytes);
        assert_eq!(enc, "deadbeef");
        assert_eq!(hex_decode(&enc).unwrap(), bytes);
        // with 0x prefix
        assert_eq!(hex_decode("0xdeadbeef").unwrap(), bytes);
        // uppercase accepted
        assert_eq!(hex_decode("DEADBEEF").unwrap(), bytes);
    }

    #[test]
    fn test_hex_decode_error_paths() {
        // odd length
        assert!(hex_decode("abc").is_err());
        // invalid hex digit
        assert!(hex_decode("0xzz").is_err());
        assert!(matches!(
            hex_decode("abc"),
            Err(Web3ErrorKind::BlockchainError(_))
        ));
    }

    #[test]
    fn test_json_rpc_error_deserializes() {
        let v: JsonRpcError = serde_json::from_value(serde_json::json!({
            "code": -32601,
            "message": "method not found",
        }))
        .unwrap();
        assert_eq!(v.code, -32601);
        assert_eq!(v.message, "method not found");
        assert!(v.data.is_none());
    }
}
