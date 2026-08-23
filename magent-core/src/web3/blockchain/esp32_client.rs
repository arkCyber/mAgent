//! ESP32-compatible Blockchain Polling Client.
//!
//! This module provides a blocking, polling-based blockchain client for ESP32
//! and other single-threaded embedded environments. Instead of async/await,
//! it uses cooperative multitasking with configurable poll intervals.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ESP32 Main Loop                          │
//! │  ┌─────────────────────────────────────────────────────┐    │
//! │  │           Blockchain Polling State Machine             │    │
//! │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌────────┐ │    │
//! │  │  │ IDLE    │→ │ PENDING │→ │ POLLING │→ │ CONFIRM│ │    │
//! │  │  └─────────┘  └─────────┘  └─────────┘  └────────┘ │    │
//! │  └─────────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────────┘
//!                            │                                    │
//!                            ▼                                    │
//!                 ┌─────────────────────┐                        │
//!                 │   HTTP (reqwest)    │                        │
//!                 │   or WiFi Client    │                        │
//!                 └─────────────────────┘                        │
//!                            │                                    │
//!                            ▼                                    │
//!                 ┌─────────────────────┐                        │
//!                 │  Ethereum RPC API   │                        │
//!                 └─────────────────────┘                        │
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use magent_core::web3::blockchain::esp32::Esp32BlockchainClient;
//!
//! let mut client = Esp32BlockchainClient::new(
//!     "https://eth.llamarpc.com",
//!     1, // chain_id
//! );
//!
//! // In your main loop:
//! loop {
//!     match client.poll() {
//!         BlockchainPollResult::Completed(tx_hash) => {
//!             // Transaction confirmed!
//!         }
//!         BlockchainPollResult::Waiting { attempts, next_in_ms } => {
//!             // Still waiting, try again later
//!             delay_ms(next_in_ms);
//!         }
//!         BlockchainPollResult::Error(e) => {
//!             // Handle error
//!         }
//!     }
//! }
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Debug;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use super::{Address, Hash, Wei};
#[allow(unused_imports)]
use super::client::{ChainClient, TransactionReceipt};

/// Maximum retry attempts before giving up
pub const MAX_POLL_ATTEMPTS: usize = 60;

/// Default poll interval in milliseconds
pub const DEFAULT_POLL_INTERVAL_MS: u32 = 5000;

/// Maximum poll interval (60 seconds)
pub const MAX_POLL_INTERVAL_MS: u32 = 60000;

/// Blockchain polling state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockchainState {
    /// Idle, no pending operations
    Idle,
    /// Waiting for transaction confirmation
    WaitingForConfirmation {
        /// Hash of the transaction we are waiting on.
        tx_hash: Hash,
        /// Wall-clock time (ms since boot) when the transaction was first submitted.
        submitted_at_ms: u64,
        /// Wall-clock time (ms since boot) of the most recent status poll.
        last_check_at_ms: u64,
        /// Number of status polls performed so far.
        poll_count: usize,
    },
    /// Checking transaction status
    CheckingStatus,
    /// Operation completed successfully
    Completed,
    /// Operation failed
    Failed,
}

/// Result of a blockchain poll operation
#[derive(Debug, Clone)]
pub enum BlockchainPollResult<T> {
    /// Operation completed successfully
    Completed(T),
    /// Still in progress, with timing info
    Waiting {
        /// Number of polls performed so far.
        attempts: usize,
        /// Suggested delay before the next poll (in milliseconds).
        next_poll_in_ms: u32,
    },
    /// An error occurred
    Error(Web3ErrorKind),
    /// No operation in progress
    Idle,
}

/// Blockchain operation being tracked
#[derive(Debug, Clone)]
pub enum BlockchainOperation {
    /// Waiting for transaction confirmation
    TransactionConfirm {
        /// Hash of the transaction we are tracking.
        tx_hash: Hash,
        /// Expected sender address (if known; used to disambiguate
        /// same-nonce replays).
        expected_from: Option<Address>,
        /// Expected recipient address (if known).
        expected_to: Option<Address>,
    },
    /// Waiting for balance update
    BalanceCheck {
        /// Address whose balance is being polled.
        address: Address,
    },
    /// Waiting for nonce update
    NonceCheck {
        /// Address whose nonce is being polled.
        address: Address,
    },
}

/// ESP32-compatible polling blockchain client
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Esp32BlockchainClient {
    /// RPC endpoint URL (e.g. `https://mainnet.infura.io/v3/...`).
    rpc_url: String,
    /// EVM chain id (1 = mainnet, 137 = polygon, …).
    chain_id: u64,
    /// Current poll state of the in-flight operation, if any.
    state: BlockchainState,
    /// Operation the client is currently tracking, if any.
    operation: Option<BlockchainOperation>,
    /// Interval between consecutive status polls, in milliseconds.
    poll_interval_ms: u32,
    /// Latest poll result (cached for callers that poll less often
    /// than the client itself).
    result: Option<BlockchainPollResult<Hash>>,
}

impl Esp32BlockchainClient {
    /// Create a new ESP32 blockchain client
    pub fn new(rpc_url: impl Into<String>, chain_id: u64) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            chain_id,
            state: BlockchainState::Idle,
            operation: None,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            result: None,
        }
    }

    /// Set the poll interval
    pub fn with_poll_interval(mut self, interval_ms: u32) -> Self {
        self.poll_interval_ms = interval_ms.min(MAX_POLL_INTERVAL_MS);
        self
    }

    /// Get the current state
    pub fn state(&self) -> BlockchainState {
        self.state.clone()
    }

    /// Check if client is idle
    pub fn is_idle(&self) -> bool {
        matches!(self.state, BlockchainState::Idle)
    }

    /// Start tracking a transaction for confirmation
    pub fn watch_transaction(
        &mut self,
        tx_hash: Hash,
        expected_from: Option<Address>,
        expected_to: Option<Address>,
    ) {
        self.state = BlockchainState::WaitingForConfirmation {
            tx_hash,
            submitted_at_ms: 0, // Caller should set actual time
            last_check_at_ms: 0,
            poll_count: 0,
        };
        self.operation = Some(BlockchainOperation::TransactionConfirm {
            tx_hash,
            expected_from,
            expected_to,
        });
        self.result = Some(BlockchainPollResult::Waiting {
            attempts: 0,
            next_poll_in_ms: 0,
        });
    }

    /// Get current time in milliseconds (stub - replace with actual timer)
    fn current_time_ms(&self) -> u64 {
        // `AtomicU64` is unavailable on 32-bit targets (e.g. RISC-V ESP32-C6/C61).
        // The stub returns a counter, so `AtomicU32` is sufficient.
        #[cfg(target_pointer_width = "64")]
        use core::sync::atomic::{AtomicU64, Ordering};
        #[cfg(target_pointer_width = "32")]
        use core::sync::atomic::{AtomicU32 as AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(100, Ordering::Relaxed).into()
    }

    /// Main polling function - call this in your main loop
    ///
    /// Returns immediately with current status.
    /// Call again after the suggested delay.
    pub fn poll(&mut self) -> BlockchainPollResult<Hash> {
        match &self.state {
            BlockchainState::Idle => {
                self.result = Some(BlockchainPollResult::Idle);
                return BlockchainPollResult::Idle;
            }
            BlockchainState::WaitingForConfirmation { tx_hash: _, poll_count, .. } => {
                if *poll_count >= MAX_POLL_ATTEMPTS {
                    self.state = BlockchainState::Failed;
                    let err = Web3ErrorKind::BlockchainError(
                        "Transaction confirmation timeout".to_string(),
                    );
                    self.result = Some(BlockchainPollResult::Error(err));
                    return self.result.clone().unwrap();
                }

                // Check if enough time has passed since last poll
                if *poll_count > 0 {
                    // In real implementation, check actual elapsed time
                    // For now, always allow next poll
                }
            }
            BlockchainState::CheckingStatus => {
                // Already checking, return waiting
            }
            BlockchainState::Completed => {
                if let Some(BlockchainPollResult::Completed(_)) = &self.result {
                    return self.result.clone().unwrap();
                }
            }
            BlockchainState::Failed => {
                if let Some(BlockchainPollResult::Error(e)) = &self.result {
                    return BlockchainPollResult::Error(e.clone());
                }
                return BlockchainPollResult::Error(Web3ErrorKind::BlockchainError(
                    "Operation failed".to_string(),
                ));
            }
        }

        // Perform the actual RPC call
        match self.check_transaction_status() {
            Ok(Some(_receipt)) => {
                // Transaction confirmed!
                if let BlockchainState::WaitingForConfirmation { tx_hash, .. } = self.state {
                    self.state = BlockchainState::Completed;
                    let result = BlockchainPollResult::Completed(tx_hash);
                    self.result = Some(result.clone());
                    result
                } else {
                    BlockchainPollResult::Idle
                }
            }
            Ok(None) => {
                // Transaction not yet confirmed
                if let BlockchainState::WaitingForConfirmation {
                    tx_hash,
                    submitted_at_ms,
                    poll_count,
                    ..
                } = self.state
                {
                    let new_count = poll_count + 1;
                    self.state = BlockchainState::WaitingForConfirmation {
                        tx_hash,
                        submitted_at_ms,
                        last_check_at_ms: self.current_time_ms(),
                        poll_count: new_count,
                    };

                    // Exponential backoff with cap
                    let next_interval = self.poll_interval_ms
                        .saturating_mul(1 << (new_count.min(3) / 2))
                        .min(MAX_POLL_INTERVAL_MS);

                    self.result = Some(BlockchainPollResult::Waiting {
                        attempts: new_count,
                        next_poll_in_ms: next_interval,
                    });
                    BlockchainPollResult::Waiting {
                        attempts: new_count,
                        next_poll_in_ms: next_interval,
                    }
                } else {
                    BlockchainPollResult::Idle
                }
            }
            Err(e) => {
                self.state = BlockchainState::Failed;
                self.result = Some(BlockchainPollResult::Error(e.clone()));
                BlockchainPollResult::Error(e)
            }
        }
    }

    /// Check the status of the tracked transaction
    fn check_transaction_status(&mut self) -> Result<Option<TransactionReceipt>, Web3ErrorKind> {
        let tx_hash = match &self.state {
            BlockchainState::WaitingForConfirmation { tx_hash, .. } => *tx_hash,
            _ => return Ok(None),
        };

        self.state = BlockchainState::CheckingStatus;

        // In real implementation, this would use esp-idf HTTP client or similar
        // For now, we return a placeholder that indicates RPC needs configuration
        let _ = tx_hash;

        Err(Web3ErrorKind::BlockchainError(
            "ESP32 HTTP client not configured - integrate esp-idf HTTP".to_string(),
        ))
    }

    /// Get balance (blocking, single call)
    pub fn get_balance_blocking(&self, _address: &Address) -> Result<Wei, Web3ErrorKind> {
        // In real implementation, perform single HTTP request
        // This is a placeholder
        Err(Web3ErrorKind::BlockchainError(
            "Use Esp32HttpClient for ESP32".to_string(),
        ))
    }

    /// Get nonce (blocking, single call)
    pub fn get_nonce_blocking(&self, _address: &Address) -> Result<u64, Web3ErrorKind> {
        // In real implementation, perform single HTTP request
        Err(Web3ErrorKind::BlockchainError(
            "Use Esp32HttpClient for ESP32".to_string(),
        ))
    }

    /// Send raw transaction (blocking, returns tx hash)
    pub fn send_raw_blocking(&self, _signed_tx: &[u8]) -> Result<Hash, Web3ErrorKind> {
        // In real implementation, perform single HTTP request
        Err(Web3ErrorKind::BlockchainError(
            "Use Esp32HttpClient for ESP32".to_string(),
        ))
    }
}

impl Default for Esp32BlockchainClient {
    fn default() -> Self {
        Self::new("https://eth.llamarpc.com", 1)
    }
}

// ============================================================================
// JSON-RPC Types for ESP32
// ============================================================================

/// JSON-RPC request body
#[derive(Debug, Clone, Serialize)]
pub struct RpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: Vec<serde_json::Value>,
    id: u32,
}

impl RpcRequest {
    /// Create a new RPC request
    pub fn new(method: impl Into<String>, params: Vec<serde_json::Value>) -> Self {
        use core::sync::atomic::{AtomicU32, Ordering};

        static REQUEST_ID: AtomicU32 = AtomicU32::new(0);
        let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);

        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
            id,
        }
    }
}

/// JSON-RPC response
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResponse<T> {
    /// Flattened success/error union — exactly one of `RpcResult::Success`
    /// or `RpcResult::Error` is populated after deserialisation.
    #[serde(flatten)]
    pub result: RpcResult<T>,
}

/// JSON-RPC success/error union. The `serde(untagged)` attribute lets
/// the parser pick the right variant based on which fields are present
/// in the wire response (a `result` field means success, an `error`
/// field means failure).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RpcResult<T> {
    /// Successful response carrying the decoded payload.
    Success {
        /// Request id echoed by the server (used to correlate
        /// out-of-order responses).
        id: u32,
        /// Decoded payload.
        result: T,
    },
    /// Failed response carrying the structured error.
    Error {
        /// Request id echoed by the server.
        id: u32,
        /// Error reported by the server.
        error: RpcError,
    },
}

/// JSON-RPC error object decoded from a failed response.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct RpcError {
    /// Numeric error code (e.g. `-32601` for `Method not found`).
    code: i32,
    /// Human-readable error message from the remote peer.
    message: String,
}

/// Parse hex string to bytes
pub fn parse_hex(s: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err(Web3ErrorKind::BlockchainError(
            "odd hex length".to_string(),
        ));
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
    fn test_client_creation() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        assert!(client.is_idle());
        assert!(matches!(client.state(), BlockchainState::Idle));
    }

    #[test]
    fn test_client_default() {
        let client = Esp32BlockchainClient::default();
        assert!(client.is_idle());
    }

    #[test]
    fn test_watch_transaction() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let tx_hash = Hash::zero();
        client.watch_transaction(tx_hash, None, None);
        assert!(!client.is_idle());
        assert!(matches!(client.state(), BlockchainState::WaitingForConfirmation { .. }));
    }

    #[test]
    fn test_watch_transaction_with_addresses() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let tx_hash = Hash::zero();
        let from = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let to = Address::from_hex("0x8626f6940E2eb28930eFb4CeF49B2d1F2C9C1199").unwrap();
        client.watch_transaction(tx_hash, Some(from), Some(to));
        assert!(!client.is_idle());
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex("0x1234").unwrap(), &[0x12, 0x34]);
        assert_eq!(parse_hex("1234").unwrap(), &[0x12, 0x34]);
        assert_eq!(parse_hex("0x").unwrap(), &[] as &[u8]);
        assert_eq!(parse_hex("").unwrap(), &[] as &[u8]);
        assert!(parse_hex("0x12x").is_err());
        assert!(parse_hex("0x1").is_err());
        assert!(parse_hex("xyz").is_err());
    }

    #[test]
    fn test_parse_hex_uppercase() {
        assert_eq!(parse_hex("0xABCD").unwrap(), &[0xAB, 0xCD]);
    }

    #[test]
    fn test_poll_interval() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1)
            .with_poll_interval(10000);
        assert_eq!(client.poll_interval_ms, 10000);
    }

    #[test]
    fn test_poll_interval_capped() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1)
            .with_poll_interval(100000);
        assert_eq!(client.poll_interval_ms, MAX_POLL_INTERVAL_MS);
    }

    #[test]
    fn test_poll_interval_zero() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1)
            .with_poll_interval(0);
        assert_eq!(client.poll_interval_ms, 0);
    }

    #[test]
    fn test_poll_idle_returns_idle() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let result = client.poll();
        assert!(matches!(result, BlockchainPollResult::Idle));
    }

    #[test]
    fn test_poll_max_attempts() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let tx_hash = Hash::zero();
        client.watch_transaction(tx_hash, None, None);

        // Poll multiple times to exceed MAX_POLL_ATTEMPTS
        // Note: In real scenario, each poll would check the transaction
        // Here we just verify the client can handle many polls
        for _ in 0..MAX_POLL_ATTEMPTS {
            client.poll();
        }
        // After exceeding max, should be in Failed state
        assert!(matches!(client.state(), BlockchainState::Failed));
    }

    #[test]
    fn test_state_machine_flow() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);

        // Start with Idle
        assert!(client.is_idle());

        // Watch a transaction -> WaitingForConfirmation
        let tx_hash = Hash::zero();
        client.watch_transaction(tx_hash, None, None);
        assert!(!client.is_idle());

        // Calling poll should transition to CheckingStatus then back
        let _ = client.poll();
        // State transitions based on RPC result
    }

    #[test]
    fn test_rpc_request_new() {
        let req = RpcRequest::new("eth_getBalance", vec![]);
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "eth_getBalance");
    }

    #[test]
    fn test_rpc_request_with_params() {
        let params = vec![
            serde_json::json!("0x1234"),
            serde_json::json!("latest"),
        ];
        let req = RpcRequest::new("eth_getBalance", params);
        assert_eq!(req.params.len(), 2);
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_POLL_ATTEMPTS, 60);
        assert_eq!(DEFAULT_POLL_INTERVAL_MS, 5000);
        assert_eq!(MAX_POLL_INTERVAL_MS, 60000);
    }

    #[test]
    fn test_blockchain_state_derive() {
        // Test that all state variants can be cloned/copied
        let state1 = BlockchainState::Idle;
        let state2 = state1;
        assert_eq!(state1, state2);

        let state3 = BlockchainState::Completed;
        assert_ne!(state1, state3);
    }

    #[test]
    fn test_blockchain_poll_result_debug() {
        let result: BlockchainPollResult<Hash> = BlockchainPollResult::Idle;
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("Idle"));
    }

    #[test]
    fn test_blockchain_operation_debug() {
        let op = BlockchainOperation::BalanceCheck {
            address: Address::zero(),
        };
        let debug_str = format!("{:?}", op);
        assert!(debug_str.contains("BalanceCheck"));
    }

    #[test]
    fn test_parse_hex_empty() {
        // Empty string should return empty vec
        let result = parse_hex("");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_hex_0x_prefix() {
        let result = parse_hex("0x").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_hex_mixed_case() {
        // Test mixed case handling
        let result = parse_hex("0xAbCdEf").unwrap();
        assert_eq!(result, &[0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn test_parse_hex_all_digits() {
        let result = parse_hex("0123456789").unwrap();
        assert_eq!(result, &[0x01, 0x23, 0x45, 0x67, 0x89]);
    }

    #[test]
    fn test_parse_hex_all_letters() {
        let result = parse_hex("abcdef").unwrap();
        assert_eq!(result, &[0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn test_parse_hex_error_odd_length() {
        // Odd length hex should fail
        let result = parse_hex("0x123");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_hex_error_invalid_char() {
        let result = parse_hex("0xGG");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_hex_very_long() {
        // Test with a longer hex string
        let hex = "0x".to_string() + &"12".repeat(100);
        let result = parse_hex(&hex);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 100);
    }

    #[test]
    fn test_esp32_client_clone() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let _cloned = client.clone();
        // Should compile - Clone trait is derived
    }

    #[test]
    fn test_watch_transaction_clears_pending_result() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);

        // First poll returns Idle
        assert!(matches!(client.poll(), BlockchainPollResult::Idle));

        // Watch a transaction
        client.watch_transaction(Hash::zero(), None, None);

        // Should now return Waiting, not Idle
        let result = client.poll();
        assert!(!matches!(result, BlockchainPollResult::Idle));
    }

    #[test]
    fn test_multiple_watch_overwrites_previous() {
        let mut client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);

        let hash1 = Hash::from_hex("0x1111111111111111111111111111111111111111111111111111111111111111").unwrap();
        let hash2 = Hash::from_hex("0x2222222222222222222222222222222222222222222222222222222222222222").unwrap();

        client.watch_transaction(hash1, None, None);

        // Watch second transaction - should overwrite
        client.watch_transaction(hash2, None, None);

        // State should be waiting for confirmation
        assert!(matches!(
            client.state(),
            BlockchainState::WaitingForConfirmation { .. }
        ));
    }

    #[test]
    fn test_get_balance_blocking_placeholder() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let addr = Address::zero();
        let result = client.get_balance_blocking(&addr);
        // Should return error (not implemented placeholder)
        assert!(result.is_err());
    }

    #[test]
    fn test_get_nonce_blocking_placeholder() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let addr = Address::zero();
        let result = client.get_nonce_blocking(&addr);
        assert!(result.is_err());
    }

    #[test]
    fn test_send_raw_blocking_placeholder() {
        let client = Esp32BlockchainClient::new("https://eth.llamarpc.com", 1);
        let result = client.send_raw_blocking(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_rpc_error_struct() {
        let error = RpcError {
            code: -32600,
            message: "Invalid Request".to_string(),
        };
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "Invalid Request");
    }

    #[test]
    fn test_rpc_response_deserialize() {
        // Test successful response
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"0x100"}"#;
        let response: RpcResponse<String> = serde_json::from_str(json).unwrap();
        match response.result {
            RpcResult::Success { id, result } => {
                assert_eq!(id, 1);
                assert_eq!(result, "0x100");
            }
            RpcResult::Error { .. } => panic!("expected success"),
        }
    }

    #[test]
    fn test_rpc_response_error_deserialize() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#;
        let response: RpcResponse<String> = serde_json::from_str(json).unwrap();
        match response.result {
            RpcResult::Success { .. } => panic!("expected error"),
            RpcResult::Error { id, error } => {
                assert_eq!(id, 1);
                assert_eq!(error.code, -32600);
            }
        }
    }

    #[test]
    fn test_poll_result_waiting_fields() {
        let result = BlockchainPollResult::<Hash>::Waiting {
            attempts: 5,
            next_poll_in_ms: 10000,
        };
        if let BlockchainPollResult::Waiting { attempts, next_poll_in_ms } = result {
            assert_eq!(attempts, 5);
            assert_eq!(next_poll_in_ms, 10000);
        } else {
            panic!("expected Waiting");
        }
    }

    #[test]
    fn test_poll_result_error_contains_message() {
        let result = BlockchainPollResult::<Hash>::Error(
            Web3ErrorKind::BlockchainError("test error".to_string())
        );
        if let BlockchainPollResult::Error(e) = &result {
            match e {
                Web3ErrorKind::BlockchainError(msg) => {
                    assert!(msg.contains("test error"));
                }
                _ => panic!("expected BlockchainError"),
            }
        } else {
            panic!("expected Error");
        }
    }
}
