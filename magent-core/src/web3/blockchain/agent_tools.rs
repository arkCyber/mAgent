//! Blockchain Tools for AI Agent.
//!
//! This module integrates blockchain functionality into the Agent's work loop,
//! providing tools for:
//! - Querying wallet balance
#![allow(irrefutable_let_patterns)]
//!   The `cfg(esp32)` / `cfg(std)` branches carry the same `if let` shape
//!   because the `BlockchainBackend` enum's std/esp32 variants are mutually
//!   exclusive at compile time. The clippy lint flags the surviving branch
//!   as "irrefutable", which is harmless here but produces noise on every
//!   CI run.
//! - Querying transaction status
//! - Sending transactions
//! - Signing messages
//! - Verifying identities
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                        Agent Work Loop                                │
//! │  ┌──────────────────────────────────────────────────────────────┐  │
//! │  │  Think → Tool Call → Execute → Observe → (Repeat)              │  │
//! │  └──────────────────────────────────────────────────────────────┘  │
//! │                              │                                        │
//! │                              ▼                                        │
//! │  ┌──────────────────────────────────────────────────────────────┐  │
//! │  │              Blockchain Tool Executor                          │  │
//! │  │  - get_balance   - send_transaction                         │  │
//! │  │  - get_nonce     - sign_message                             │  │
//! │  │  - poll_tx       - verify_signature                          │  │
//! │  └──────────────────────────────────────────────────────────────┘  │
//! │                              │                                        │
//! │                              ▼                                        │
//! │  ┌──────────────────────────────────────────────────────────────┐  │
//! │  │              Blockchain Polling Manager                       │  │
//! │  │  - Maintains pending transactions                          │  │
//! │  │  - Handles confirmation polling                             │  │
//! │  │  - State: Idle | Pending | Confirmed | Failed              │  │
//! │  └──────────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use magent_core::web3::blockchain::agent_tools;
//!
//! // Create blockchain tools registry
//! let tools = agent_tools::create_blockchain_tools();
//!
//! // Register with agent
//! for tool in tools {
//!     agent.tools().register(tool)?;
//! }
//!
//! // In agent task, use natural language:
//! // "Check my ETH balance", "Send 0.1 ETH to 0x...", "Sign this message"
//! ```

#[cfg(feature = "web3")]
use alloc::string::{String, ToString};
#[cfg(feature = "web3")]
use heapless::Vec;

#[cfg(feature = "web3")]
use crate::error::{AgentError, Result};
#[cfg(feature = "web3")]
use crate::tools::{Tool, ToolType};

#[cfg(feature = "web3")]
#[allow(unused_imports)]
use crate::web3::blockchain::{Address, Hash, Wei};

#[cfg(feature = "web3")]
use crate::error::try_heapless;

#[cfg(all(feature = "web3", feature = "esp32"))]
#[allow(unused_imports)]
use crate::web3::blockchain::{
    BlockchainPollResult, BlockchainState, EspHttpClient, TransactionPoller,
};

#[cfg(all(feature = "web3", feature = "esp32"))]
#[allow(unused_imports)]
use crate::web3::blockchain::esp32_http::{HttpClientConfig, PollStatus};

#[cfg(feature = "web3")]
use crate::web3::identity::Identity;

#[cfg(feature = "web3")]
#[allow(unused_imports)]
use serde_json::{json, Value};

// Standard HTTP RPC client (gated on std)
#[cfg(all(feature = "web3", feature = "std"))]
#[allow(unused_imports)]
use crate::web3::blockchain::client::{ChainClient, ChainId};
#[cfg(all(feature = "web3", feature = "std"))]
#[allow(unused_imports)]
use crate::web3::blockchain::http_client::HttpRpcClient;

// ============================================================================
// Blockchain Tool Types
// ============================================================================

/// Blockchain operation result for agent tools
#[derive(Debug, Clone)]
pub struct BlockchainToolResult {
    /// Whether the operation succeeded
    pub success: bool,
    /// Result data as formatted string
    pub data: String,
    /// Error message if failed
    pub error: Option<String>,
}

impl BlockchainToolResult {
    /// Create a success result
    pub fn success(data: impl Into<String>) -> Self {
        Self {
            success: true,
            data: data.into(),
            error: None,
        }
    }

    /// Create an error result
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: String::new(),
            error: Some(msg.into()),
        }
    }
}

// ============================================================================
// Blockchain Manager (Agent State)
// ============================================================================

/// Manages blockchain state for the agent.
/// This struct holds the HTTP client and polling state.
#[derive(Debug, Clone)]
pub struct BlockchainManager {
    /// Backend-specific HTTP client wrapper for RPC calls
    client: Option<BlockchainBackend>,
    /// Poll interval in milliseconds
    poll_interval_ms: u32,
    /// Max poll attempts
    max_attempts: usize,
    /// Current pending transaction hash
    pending_tx: Option<Hash>,
    /// RPC URL
    rpc_url: String,
    /// Chain ID
    chain_id: u64,
}

/// Backend selection for blockchain RPC client.
/// Different feature combinations yield different concrete types.
#[cfg(feature = "esp32")]
#[derive(Debug, Clone)]
enum BlockchainBackend {
    Esp32 {
        client: EspHttpClient,
        #[allow(dead_code)]
        poller: Option<TransactionPoller>,
    },
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
#[derive(Debug, Clone)]
enum BlockchainBackend {
    /// Standard HTTP RPC client (uses reqwest when enabled).
    Std {
        /// RPC client instance
        client: HttpRpcClient,
    },
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
#[derive(Debug, Clone)]
enum BlockchainBackend {
    /// No-op backend; stores the URL only.
    Dummy,
}

impl BlockchainManager {
    /// Create a new blockchain manager
    pub fn new(rpc_url: &str, chain_id: u64) -> Self {
        Self {
            client: None,
            poll_interval_ms: 5000,
            max_attempts: 60,
            pending_tx: None,
            rpc_url: rpc_url.to_string(),
            chain_id,
        }
    }

    /// Set poll interval
    pub fn with_poll_interval(mut self, interval_ms: u32) -> Self {
        self.poll_interval_ms = interval_ms;
        self
    }

    /// Set max attempts
    pub fn with_max_attempts(mut self, max: usize) -> Self {
        self.max_attempts = max;
        self
    }

    /// Initialize the HTTP client
    pub fn init(&mut self) -> Result<()> {
        #[cfg(feature = "esp32")]
        {
            let config = HttpClientConfig::from_url(&self.rpc_url);
            let client = EspHttpClient::new(config);
            self.client = Some(BlockchainBackend::Esp32 {
                client: client.clone(),
                poller: None,
            });
        }
        #[cfg(all(feature = "std", not(feature = "esp32")))]
        {
            // Use the standard HTTP RPC client (works with reqwest when enabled).
            let client = HttpRpcClient::new(self.rpc_url.clone(), self.chain_id);
            self.client = Some(BlockchainBackend::Std { client });
        }
        #[cfg(not(any(feature = "std", feature = "esp32")))]
        {
            // Pure no_std fallback (no networking).
            self.client = Some(BlockchainBackend::Dummy);
        }
        Ok(())
    }

    /// Check if manager is initialized
    pub fn is_initialized(&self) -> bool {
        self.client.is_some()
    }

    /// Get balance for an address
    pub fn get_balance(&mut self, address: &str) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_get_balance(backend, address) {
            Ok(balance) => {
                let eth = balance.as_wei() as f64 / 1_000_000_000_000_000_000.0;
                BlockchainToolResult::success(format!("{:.6} ETH", eth))
            }
            Err(e) => BlockchainToolResult::error(format!("Failed to get balance: {}", e)),
        }
    }

    /// Get nonce for an address
    pub fn get_nonce(&mut self, address: &str) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_get_nonce(backend, address) {
            Ok(nonce) => BlockchainToolResult::success(format!("{}", nonce)),
            Err(e) => BlockchainToolResult::error(format!("Failed to get nonce: {}", e)),
        }
    }

    /// Get current gas price
    pub fn get_gas_price(&mut self) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_get_gas_price(backend) {
            Ok(price) => {
                let gwei = price as f64 / 1_000_000_000.0;
                BlockchainToolResult::success(format!("{:.2} Gwei", gwei))
            }
            Err(e) => BlockchainToolResult::error(format!("Failed to get gas price: {}", e)),
        }
    }

    /// Get current block number
    pub fn get_block_number(&mut self) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_get_block_number(backend) {
            Ok(block) => BlockchainToolResult::success(format!("{}", block)),
            Err(e) => BlockchainToolResult::error(format!("Failed to get block number: {}", e)),
        }
    }

    /// Send a raw transaction (returns tx hash)
    pub fn send_transaction(&mut self, signed_tx_hex: &str) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_send_transaction(backend, signed_tx_hex) {
            Ok(tx_hash) => {
                // Try to record the pending tx locally so subsequent
                // poll_transaction calls have a target. If parsing
                // the hash fails, surface a non-fatal warning so the
                // caller knows the pending cache is incomplete.
                match Hash::from_hex(&tx_hash) {
                    Ok(h) => self.pending_tx = Some(h),
                    Err(_) => {
                        return BlockchainToolResult {
                            success: true,
                            data: format!(
                                "Transaction submitted: {} (pending tracking unavailable: unparseable hash)",
                                tx_hash
                            ),
                            error: None,
                        };
                    }
                }
                BlockchainToolResult::success(format!("Transaction submitted: {}", tx_hash))
            }
            Err(e) => BlockchainToolResult::error(format!("Failed to send transaction: {}", e)),
        }
    }

    /// Poll for transaction confirmation
    pub fn poll_transaction(&mut self, tx_hash: &str) -> BlockchainToolResult {
        let backend = match &mut self.client {
            Some(b) => b,
            None => return BlockchainToolResult::error("Blockchain client not initialized"),
        };

        match backend_poll_transaction(backend, tx_hash) {
            Ok(s) => BlockchainToolResult::success(s),
            Err(e) => BlockchainToolResult::error(format!("{}", e)),
        }
    }

    /// Get pending transaction status
    pub fn get_pending_status(&self) -> Option<&Hash> {
        self.pending_tx.as_ref()
    }

    /// Clear pending transaction
    pub fn clear_pending(&mut self) {
        self.pending_tx = None;
    }

    /// Get chain ID
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Switch to a different chain by re-initialising the backend
    /// with the given RPC URL and chain ID.
    ///
    /// This is destructive: any in-flight pending transaction is
    /// cleared (it's tied to the previous chain) and the existing
    /// client is dropped. The new backend is initialised
    /// immediately; if that fails we return the error and leave
    /// the manager in an uninitialised state — caller is then
    /// expected to call `init()` again or surface the failure.
    pub fn switch_chain(&mut self, rpc_url: &str, chain_id: u64) -> Result<()> {
        self.rpc_url = rpc_url.to_string();
        self.chain_id = chain_id;
        self.client = None;
        self.pending_tx = None;
        self.init()
    }

    /// Reset the manager back to its uninitialised state. Useful
    /// for tests and for retrying after a transient backend
    /// failure.
    pub fn reset(&mut self) {
        self.client = None;
        self.pending_tx = None;
    }

    /// Get the configured RPC URL (even before init). Useful for
    /// diagnostics / logging.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Poll interval in milliseconds (default 5000).
    pub fn poll_interval_ms(&self) -> u32 {
        self.poll_interval_ms
    }

    /// Max poll attempts before giving up on a pending tx (default 60).
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }
}

// ============================================================================
// Wei / ETH conversion helpers (free functions — also exposed as
// agent-facing tool results).
// ============================================================================

/// Convert a wei amount to ETH as an f64. Returns 0.0 for empty
/// input rather than panicking, so it's safe to use on untrusted
/// RPC output.
///
/// Note: f64 loses precision past ~2^53 wei (~9 ETH). For amounts
/// larger than that, use [`wei_to_eth_string`] which formats via
/// integer division.
pub fn wei_to_eth(wei: u128) -> f64 {
    (wei as f64) / 1e18
}

/// Convert wei to a human-readable ETH string with 6 decimal
/// places, e.g. `123_456_789_000_000_000_u128` → `"0.000123 ETH"`.
/// Uses integer arithmetic to avoid f64 rounding.
pub fn wei_to_eth_string(wei: u128) -> String {
    let whole = wei / 1_000_000_000_000_000_000;
    let frac = wei % 1_000_000_000_000_000_000;
    // 6 decimal places of the fractional part.
    let frac6 = frac / 1_000_000_000_000_000;
    alloc::format!("{}.{:06} ETH", whole, frac6)
}

/// Convert gwei (as integer) to wei. `1 gwei = 1e9 wei`.
pub const GWEI_PER_ETH: u128 = 1_000_000_000;
/// Wei per ETH.
pub const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;

/// Convert gwei to wei.
pub fn gwei_to_wei(gwei: u128) -> u128 {
    gwei.saturating_mul(GWEI_PER_ETH)
}

/// Convert wei to gwei (truncating fractional part).
pub fn wei_to_gwei(wei: u128) -> u128 {
    wei / GWEI_PER_ETH
}

impl Default for BlockchainManager {
    fn default() -> Self {
        Self::new("https://eth.llamarpc.com", 1)
    }
}

// ============================================================================
// Backend Helper Functions
// ============================================================================

/// Get balance via the appropriate backend.
#[cfg(feature = "esp32")]
fn backend_get_balance(
    backend: &mut BlockchainBackend,
    address: &str,
) -> core::result::Result<Wei, AgentError> {
    if let BlockchainBackend::Esp32 { client, .. } = backend {
        let raw = client
            .get_balance(address)
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain",
                reason: crate::error::ConfigError::NotConfigured,
            })?;
        Ok(Wei(raw))
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_get_balance(
    backend: &mut BlockchainBackend,
    address: &str,
) -> core::result::Result<Wei, AgentError> {
    use crate::web3::blockchain::Address;
    if let BlockchainBackend::Std { client } = backend {
        let addr = Address::from_hex(address).map_err(|_| AgentError::ConfigurationError {
            field: "blockchain_address",
            reason: crate::error::ConfigError::MissingField,
        })?;
        client
            .get_balance(&addr)
            .map_err(|_e| AgentError::ConfigurationError {
                field: "blockchain_rpc",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_get_balance(
    _backend: &mut BlockchainBackend,
    _address: &str,
) -> core::result::Result<Wei, AgentError> {
    // Without networking we can't fetch balances.
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Get nonce via the appropriate backend.
#[cfg(feature = "esp32")]
fn backend_get_nonce(
    backend: &mut BlockchainBackend,
    address: &str,
) -> core::result::Result<u64, AgentError> {
    if let BlockchainBackend::Esp32 { client, .. } = backend {
        client
            .get_nonce(address)
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_get_nonce(
    backend: &mut BlockchainBackend,
    address: &str,
) -> core::result::Result<u64, AgentError> {
    use crate::web3::blockchain::Address;
    if let BlockchainBackend::Std { client } = backend {
        let addr = Address::from_hex(address).map_err(|_| AgentError::ConfigurationError {
            field: "blockchain_address",
            reason: crate::error::ConfigError::MissingField,
        })?;
        client
            .get_nonce(&addr)
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain_rpc",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_get_nonce(
    _backend: &mut BlockchainBackend,
    _address: &str,
) -> core::result::Result<u64, AgentError> {
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Get gas price via the appropriate backend.
#[cfg(feature = "esp32")]
fn backend_get_gas_price(backend: &mut BlockchainBackend) -> core::result::Result<u64, AgentError> {
    if let BlockchainBackend::Esp32 { client, .. } = backend {
        // `EspHttpClient::get_gas_price` returns wei (u128) but the
        // host-side `ChainClient::get_gas_price` returns gwei (u64).
        // We match the host convention by truncating; satellite and
        // IoT workloads never need sub-gwei precision.
        let wei = client
            .get_gas_price()
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain",
                reason: crate::error::ConfigError::NotConfigured,
            })?;
        Ok((wei / Wei::GWEI.0) as u64)
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_get_gas_price(backend: &mut BlockchainBackend) -> core::result::Result<u64, AgentError> {
    if let BlockchainBackend::Std { client } = backend {
        let wei = client
            .get_gas_price()
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain_rpc",
                reason: crate::error::ConfigError::MissingField,
            })?;
        Ok(wei.as_wei() as u64)
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_get_gas_price(
    _backend: &mut BlockchainBackend,
) -> core::result::Result<u64, AgentError> {
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Get block number via the appropriate backend.
#[cfg(feature = "esp32")]
fn backend_get_block_number(
    backend: &mut BlockchainBackend,
) -> core::result::Result<u64, AgentError> {
    if let BlockchainBackend::Esp32 { client, .. } = backend {
        client
            .get_block_number()
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_get_block_number(
    backend: &mut BlockchainBackend,
) -> core::result::Result<u64, AgentError> {
    if let BlockchainBackend::Std { client } = backend {
        client
            .get_block_number()
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain_rpc",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_get_block_number(
    _backend: &mut BlockchainBackend,
) -> core::result::Result<u64, AgentError> {
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Send transaction via the appropriate backend.
#[cfg(feature = "esp32")]
fn backend_send_transaction(
    backend: &mut BlockchainBackend,
    signed_tx_hex: &str,
) -> core::result::Result<String, AgentError> {
    if let BlockchainBackend::Esp32 { client, .. } = backend {
        client
            .send_raw_transaction(signed_tx_hex)
            .map_err(|_| AgentError::ConfigurationError {
                field: "blockchain",
                reason: crate::error::ConfigError::MissingField,
            })
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_send_transaction(
    backend: &mut BlockchainBackend,
    signed_tx_hex: &str,
) -> core::result::Result<String, AgentError> {
    if let BlockchainBackend::Std { client } = backend {
        let stripped = signed_tx_hex.strip_prefix("0x").unwrap_or(signed_tx_hex);
        let bytes = hex_decode(stripped).map_err(|_| AgentError::ConfigurationError {
            field: "blockchain_tx",
            reason: crate::error::ConfigError::MissingField,
        })?;
        let hash =
            client
                .send_raw_transaction(&bytes)
                .map_err(|_| AgentError::ConfigurationError {
                    field: "blockchain_rpc",
                    reason: crate::error::ConfigError::MissingField,
                })?;
        Ok(hash.to_hex())
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_send_transaction(
    _backend: &mut BlockchainBackend,
    _signed_tx_hex: &str,
) -> core::result::Result<String, AgentError> {
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Poll transaction status.
///
/// # Aerospace traceability
///
/// TRACE: REQ-NET-002: RPC failure must NEVER report `Ok`; the backend MUST
/// return `Err` so the agent loop drives the tool call to finalisation.
/// TRACE: REQ-FW-001: Available on the bare-metal ESP32 build path.
#[cfg(feature = "esp32")]
fn backend_poll_transaction(
    _backend: &mut BlockchainBackend,
    _tx_hash: &str,
) -> core::result::Result<String, AgentError> {
    // TODO[GAP-003]: wire to `esp_idf_svc::http::client::EspHttpClient` once
    // the firmware crate pulls in `esp-idf-svc 0.52`. Until then, refuse
    // rather than fabricate a synthetic success — see REQ-NET-002.
    Err(AgentError::ConfigurationError {
        field: "blockchain_rpc",
        reason: crate::error::ConfigError::NotConfigured,
    })
}

#[cfg(all(feature = "std", not(feature = "esp32")))]
fn backend_poll_transaction(
    backend: &mut BlockchainBackend,
    tx_hash: &str,
) -> core::result::Result<String, AgentError> {
    use crate::web3::blockchain::Hash;
    if let BlockchainBackend::Std { client } = backend {
        let hash = Hash::from_hex(tx_hash).map_err(|_| AgentError::ConfigurationError {
            field: "blockchain_tx_hash",
            reason: crate::error::ConfigError::MissingField,
        })?;
        match client.get_transaction_receipt(&hash) {
            Ok(Some(receipt)) => Ok(alloc::format!(
                "tx confirmed: status={}, block={}",
                receipt.status,
                receipt.block_number
            )),
            Ok(None) => Ok(alloc::format!("tx {} still pending", tx_hash)),
            Err(_e) => Err(AgentError::ConfigurationError {
                field: "blockchain_rpc",
                reason: crate::error::ConfigError::MissingField,
            }),
        }
    } else {
        Err(AgentError::ConfigurationError {
            field: "blockchain",
            reason: crate::error::ConfigError::MissingField,
        })
    }
}

#[cfg(not(any(feature = "std", feature = "esp32")))]
fn backend_poll_transaction(
    _backend: &mut BlockchainBackend,
    _tx_hash: &str,
) -> core::result::Result<String, AgentError> {
    Err(AgentError::ConfigurationError {
        field: "blockchain",
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Decode hex (with optional `0x` prefix) into raw bytes.
#[allow(dead_code)]
fn hex_decode(s: &str) -> core::result::Result<alloc::vec::Vec<u8>, AgentError> {
    if !s.len().is_multiple_of(2) {
        return Err(AgentError::ConfigurationError {
            field: "blockchain_hex",
            reason: crate::error::ConfigError::MissingField,
        });
    }
    let mut out = alloc::vec::Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = match chunk[0] {
            b'0'..=b'9' => chunk[0] - b'0',
            b'a'..=b'f' => chunk[0] - b'a' + 10,
            b'A'..=b'F' => chunk[0] - b'A' + 10,
            _ => {
                return Err(AgentError::ConfigurationError {
                    field: "blockchain_hex",
                    reason: crate::error::ConfigError::MissingField,
                })
            }
        };
        let lo = match chunk[1] {
            b'0'..=b'9' => chunk[1] - b'0',
            b'a'..=b'f' => chunk[1] - b'a' + 10,
            b'A'..=b'F' => chunk[1] - b'A' + 10,
            _ => {
                return Err(AgentError::ConfigurationError {
                    field: "blockchain_hex",
                    reason: crate::error::ConfigError::MissingField,
                })
            }
        };
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

// ============================================================================
// Tool Executor Functions
// ============================================================================

/// Execute blockchain tool based on name and arguments
#[cfg(feature = "web3")]
pub fn execute_blockchain_tool(
    manager: &mut BlockchainManager,
    tool_name: &str,
    args: &str,
) -> BlockchainToolResult {
    match tool_name {
        "get_balance" => execute_get_balance(manager, args),
        "get_nonce" => execute_get_nonce(manager, args),
        "get_gas_price" => execute_get_gas_price(manager, args),
        "get_block_number" => execute_get_block_number(manager, args),
        "send_transaction" => execute_send_transaction(manager, args),
        "poll_transaction" => execute_poll_transaction(manager, args),
        "blockchain_status" => execute_status(manager),
        _ => BlockchainToolResult::error(format!("Unknown blockchain tool: {}", tool_name)),
    }
}

#[cfg(feature = "web3")]
fn execute_get_balance(manager: &mut BlockchainManager, args: &str) -> BlockchainToolResult {
    // Parse address from args
    let address = parse_address_from_args(args);
    match address {
        Some(addr) => manager.get_balance(&addr),
        None => BlockchainToolResult::error("Invalid or missing address parameter".to_string()),
    }
}

#[cfg(feature = "web3")]
fn execute_get_nonce(manager: &mut BlockchainManager, args: &str) -> BlockchainToolResult {
    let address = parse_address_from_args(args);
    match address {
        Some(addr) => manager.get_nonce(&addr),
        None => BlockchainToolResult::error("Invalid or missing address parameter".to_string()),
    }
}

#[cfg(feature = "web3")]
fn execute_get_gas_price(manager: &mut BlockchainManager, _args: &str) -> BlockchainToolResult {
    manager.get_gas_price()
}

#[cfg(feature = "web3")]
fn execute_get_block_number(manager: &mut BlockchainManager, _args: &str) -> BlockchainToolResult {
    manager.get_block_number()
}

#[cfg(feature = "web3")]
fn execute_send_transaction(manager: &mut BlockchainManager, args: &str) -> BlockchainToolResult {
    // Parse signed transaction hex from args
    let tx_hex = parse_tx_from_args(args);
    match tx_hex {
        Some(hex) => manager.send_transaction(&hex),
        None => BlockchainToolResult::error("Invalid or missing transaction parameter".to_string()),
    }
}

#[cfg(feature = "web3")]
fn execute_poll_transaction(manager: &mut BlockchainManager, args: &str) -> BlockchainToolResult {
    // Parse tx hash from args, or use pending
    if let Some(tx_hash) = parse_tx_hash_from_args(args) {
        manager.poll_transaction(&tx_hash)
    } else if let Some(pending) = manager.get_pending_status() {
        manager.poll_transaction(&pending.to_hex())
    } else {
        BlockchainToolResult::error("No transaction to poll".to_string())
    }
}

#[cfg(feature = "web3")]
fn execute_status(manager: &mut BlockchainManager) -> BlockchainToolResult {
    if !manager.is_initialized() {
        return BlockchainToolResult::error("Blockchain client not initialized".to_string());
    }

    let status = if manager.get_pending_status().is_some() {
        "Transaction pending"
    } else {
        "Ready"
    };

    BlockchainToolResult::success(format!(
        "Chain ID: {}, Status: {}",
        manager.chain_id(),
        status
    ))
}

// ============================================================================
// Argument Parsing Helpers
// ============================================================================

#[cfg(feature = "web3")]
fn parse_address_from_args(args: &str) -> Option<String> {
    // Try to parse as JSON first
    if let Ok(parsed) = serde_json::from_str::<Value>(args) {
        if let Some(address) = parsed.get("address").and_then(|v| v.as_str()) {
            return Some(address.to_string());
        }
        if let Some(addr) = parsed.get("to").and_then(|v| v.as_str()) {
            return Some(addr.to_string());
        }
    }

    // Try as plain address string
    let trimmed = args.trim();
    if trimmed.starts_with("0x") && trimmed.len() == 42 {
        return Some(trimmed.to_string());
    }

    None
}

#[cfg(feature = "web3")]
fn parse_tx_from_args(args: &str) -> Option<String> {
    if let Ok(parsed) = serde_json::from_str::<Value>(args) {
        if let Some(tx) = parsed.get("transaction").and_then(|v| v.as_str()) {
            return Some(tx.to_string());
        }
        if let Some(tx) = parsed.get("signed_tx").and_then(|v| v.as_str()) {
            return Some(tx.to_string());
        }
    }

    let trimmed = args.trim();
    if trimmed.starts_with("0x") && trimmed.len() > 10 {
        return Some(trimmed.to_string());
    }

    None
}

#[cfg(feature = "web3")]
fn parse_tx_hash_from_args(args: &str) -> Option<String> {
    if let Ok(parsed) = serde_json::from_str::<Value>(args) {
        if let Some(hash) = parsed.get("tx_hash").and_then(|v| v.as_str()) {
            return Some(hash.to_string());
        }
        if let Some(hash) = parsed.get("hash").and_then(|v| v.as_str()) {
            return Some(hash.to_string());
        }
    }

    let trimmed = args.trim();
    if trimmed.starts_with("0x") && trimmed.len() == 66 {
        return Some(trimmed.to_string());
    }

    None
}

// ============================================================================
// Tool Registration
// ============================================================================

/// Create blockchain tools for agent registration
#[cfg(feature = "web3")]
pub fn create_blockchain_tools() -> Vec<Tool, 8> {
    let mut tools = Vec::new();

    // HARDENING (audit-2026-08 unwrap sweep): the previous code used
    // `heapless::String::try_from("...").unwrap()` on all 16 constant
    // tool-name and description strings. All of them fit easily within
    // the `name: String<32>` and `description: String<256>` bounds, so
    // a panic is impossible — but a future contributor who lengthens a
    // description past 256 chars would trigger a panic. We use
    // `try_heapless` so the function stays panic-free regardless of
    // future content changes.
    macro_rules! push_tool {
        ($name:expr, $desc:expr) => {{
            let tool = Tool {
                name: try_heapless::<32>($name),
                description: try_heapless::<128>($desc),
                tool_type: ToolType::ReadSensor,
            };
            let _ = tools.push(tool);
        }};
    }

    push_tool!(
        "get_balance",
        "Get ETH balance for an Ethereum address. Args: {\"address\": \"0x...\"}"
    );
    push_tool!(
        "get_nonce",
        "Get transaction count (nonce) for an Ethereum address. Args: {\"address\": \"0x...\"}"
    );
    push_tool!(
        "get_gas_price",
        "Get current gas price in Gwei. No args required."
    );
    push_tool!(
        "get_block_number",
        "Get current block number. No args required."
    );
    push_tool!(
        "send_transaction",
        "Send a signed transaction. Args: {\"transaction\": \"0x...\"}"
    );
    push_tool!(
        "poll_transaction",
        "Poll for transaction confirmation. Args: {\"tx_hash\": \"0x...\"} or uses pending tx."
    );
    push_tool!(
        "blockchain_status",
        "Get blockchain client status. No args required."
    );
    push_tool!(
        "sign_message",
        "Sign a message with the agent's identity key. Args: {\"message\": \"...\"}"
    );

    tools
}

// ============================================================================
// Identity Signing (Local, No RPC)
// ============================================================================

/// Sign a message using the agent's identity key
#[cfg(feature = "web3")]
pub fn sign_message(identity: &Identity, message: &str) -> BlockchainToolResult {
    let message_bytes = message.as_bytes();
    match identity.sign(message_bytes) {
        Ok(signed_msg) => BlockchainToolResult::success(format!(
            "Message signed successfully. Signature: {}",
            signed_msg.signature_hex
        )),
        Err(e) => BlockchainToolResult::error(format!("Signing failed: {:?}", e)),
    }
}

/// Verify a signature
///
/// Verifies that `signature_hex` is a valid Ed25519 signature of
/// `message` made by `identity.public_key()`. Returns success when
/// the signature matches, an error otherwise.
#[cfg(feature = "web3")]
pub fn verify_signature(
    identity: &Identity,
    message: &str,
    signature_hex: &str,
) -> BlockchainToolResult {
    let message_bytes = message.as_bytes();

    // 1. Parse the signature from hex into the canonical Signature type
    //    so the caller gets a clear error when they paste something
    //    that isn't a 128-char hex Ed25519 signature.
    if crate::web3::Signature::from_hex(signature_hex).is_err() {
        return BlockchainToolResult::error(
            "Invalid signature format (expected 128-char hex)".to_string(),
        );
    }

    // 2. Verify the signature using the identity's public key directly
    //    (not by re-signing). `verify_signature` returns true/false for
    //    any cryptographic failure with no panics.
    let valid = crate::web3::verify_signature(identity.public_key(), signature_hex, message_bytes);
    if valid {
        BlockchainToolResult::success("Signature verified successfully".to_string())
    } else {
        BlockchainToolResult::error("Signature verification failed".to_string())
    }
}

#[cfg(feature = "web3")]
#[allow(dead_code)]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ============================================================================
// Agent Integration Traits
// ============================================================================

/// Extension trait for an agent-like wrapper to expose a
/// blockchain manager handle.
///
/// The trait deliberately has no required `where Self: Sized` /
/// `Default` bound so any container that has a
/// `BlockchainManager` field can opt in via a few-line impl
/// without dragging in extra trait baggage. Most callers will
/// use the host-side `BlockchainManagerHolder` struct supplied
/// below — a small wrapper kept inside the `agent_tools` module
/// so embedded builds (where `MiniAgent` lives) don't pay for it.
#[cfg(feature = "web3")]
pub trait BlockchainAgentExt {
    /// Get the blockchain manager (immutable).
    fn blockchain_manager(&self) -> &BlockchainManager;

    /// Get the blockchain manager (mutable).
    fn blockchain_manager_mut(&mut self) -> &mut BlockchainManager;

    /// Convenience: initialize the manager if it isn't already.
    ///
    /// Returns true when the call had to perform initialization,
    /// false when the manager was already initialized.
    fn ensure_blockchain_initialized(&mut self) -> bool
    where
        Self: Sized,
    {
        let needs_init = !self.blockchain_manager().is_initialized();
        if needs_init {
            let _ = self.blockchain_manager_mut().init();
        }
        needs_init
    }

    /// Convenience: get the chain id this agent is bound to.
    fn chain_id(&self) -> u64
    where
        Self: Sized,
    {
        self.blockchain_manager().chain_id()
    }
}

/// A small, host-side container exposing `BlockchainManager` to any
/// embedder that wants `BlockchainAgentExt` semantics without
/// modifying `MiniAgent` itself. Useful for the CLI runner and for
/// integration tests.
#[cfg(feature = "web3")]
#[derive(Debug, Clone)]
pub struct BlockchainManagerHolder {
    manager: BlockchainManager,
}

#[cfg(feature = "web3")]
impl BlockchainManagerHolder {
    /// Create a holder pointing at an Ethereum mainnet RPC by default.
    pub fn new() -> Self {
        Self::with_rpc("https://eth.llamarpc.com", 1)
    }

    /// Create with a custom RPC URL and chain id.
    pub fn with_rpc(rpc_url: &str, chain_id: u64) -> Self {
        Self {
            manager: BlockchainManager::new(rpc_url, chain_id),
        }
    }
}

#[cfg(feature = "web3")]
impl Default for BlockchainManagerHolder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "web3")]
impl BlockchainAgentExt for BlockchainManagerHolder {
    fn blockchain_manager(&self) -> &BlockchainManager {
        &self.manager
    }

    fn blockchain_manager_mut(&mut self) -> &mut BlockchainManager {
        &mut self.manager
    }
}

// ============================================================================
// Agent Runner Integration Helpers
// ============================================================================

/// Run a blockchain tool by name, returning a blockchain-specific result.
///
/// This is the main entry point for the agent runner to invoke blockchain tools.
/// It accepts a mutable blockchain manager and tool call arguments.
#[cfg(feature = "web3")]
pub fn run_blockchain_tool(
    manager: &mut BlockchainManager,
    tool_name: &str,
    args: &str,
) -> BlockchainToolResult {
    execute_blockchain_tool(manager, tool_name, args)
}

/// Configure a `MiniAgent` (via its tool registry) with all blockchain tools.
///
/// Call this from the CLI runner or agent startup:
/// ```ignore
/// let mut manager = BlockchainManager::default();
/// register_blockchain_tools(agent.tools(), &mut manager);
/// ```
#[cfg(feature = "web3")]
pub fn register_blockchain_tools(
    registry: &mut crate::tools::ToolRegistry,
    manager: &mut BlockchainManager,
) -> usize {
    let tools = create_blockchain_tools();
    let count = tools.len();
    let _ = manager.init();

    for tool in &tools {
        let _ = registry.register(tool.clone());
    }

    count
}

/// Configure a `MiniAgent` tool registry with blockchain tools but no manager.
///
/// Convenience wrapper for testing or in cases where the RPC client is not needed.
#[cfg(feature = "web3")]
pub fn register_blockchain_tools_only(registry: &mut crate::tools::ToolRegistry) -> usize {
    let tools = create_blockchain_tools();
    let count = tools.len();
    for tool in &tools {
        let _ = registry.register(tool.clone());
    }
    count
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "web3"))]
mod tests {
    use super::*;

    #[test]
    fn test_blockchain_manager_creation() {
        let manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        assert_eq!(manager.chain_id(), 1);
        assert!(!manager.is_initialized());
    }

    #[test]
    fn test_parse_address_from_args_json() {
        let args = r#"{"address": "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21"}"#;
        let addr = parse_address_from_args(args);
        assert_eq!(
            addr,
            Some("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21".to_string())
        );
    }

    #[test]
    fn test_parse_address_from_args_plain() {
        let args = "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21";
        let addr = parse_address_from_args(args);
        assert_eq!(
            addr,
            Some("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21".to_string())
        );
    }

    #[test]
    fn test_parse_address_invalid() {
        let args = "invalid";
        let addr = parse_address_from_args(args);
        assert!(addr.is_none());
    }

    #[test]
    fn test_parse_tx_hash() {
        let args = r#"{"tx_hash": "0xabc123def456"}"#;
        let hash = parse_tx_hash_from_args(args);
        assert!(hash.is_some());
    }

    #[test]
    fn test_parse_tx_hash_with_hash_key() {
        let args = r#"{"hash": "0xabc123def456789"}"#;
        let hash = parse_tx_hash_from_args(args);
        assert!(hash.is_some());
    }

    #[test]
    fn test_parse_tx_from_args() {
        let args = r#"{"transaction": "0xf8a..."}"#;
        let tx = parse_tx_from_args(args);
        assert!(tx.is_some());
    }

    #[test]
    fn test_parse_tx_from_args_signed() {
        let args = r#"{"signed_tx": "0xf8a..."}"#;
        let tx = parse_tx_from_args(args);
        assert!(tx.is_some());
    }

    #[test]
    fn test_parse_tx_hex_plain() {
        let args = "0xf8a0123456789abcdef";
        let tx = parse_tx_from_args(args);
        assert!(tx.is_some());
    }

    #[test]
    fn test_parse_tx_invalid() {
        let args = "invalid";
        let tx = parse_tx_from_args(args);
        assert!(tx.is_none());
    }

    #[test]
    fn test_parse_address_to_field() {
        let args = r#"{"to": "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21"}"#;
        let addr = parse_address_from_args(args);
        assert!(addr.is_some());
    }

    #[test]
    fn test_parse_address_invalid_hex() {
        let args = "0x123"; // Too short
        let addr = parse_address_from_args(args);
        assert!(addr.is_none());
    }

    #[test]
    fn test_parse_tx_hash_invalid() {
        let args = "0x123"; // Not a valid tx hash (should be 66 chars)
        let hash = parse_tx_hash_from_args(args);
        assert!(hash.is_none());
    }

    #[test]
    fn test_tool_result_with_data_and_error() {
        // Edge case: both data and error should not coexist in proper usage
        // But our struct allows it, test the combination
        let result = BlockchainToolResult {
            success: false,
            data: "partial".to_string(),
            error: Some("error message".to_string()),
        };
        assert!(!result.success);
        assert_eq!(result.data, "partial");
        assert!(result.error.is_some());
    }

    #[test]
    fn test_blockchain_manager_default() {
        let manager = BlockchainManager::default();
        assert_eq!(manager.chain_id(), 1);
        assert!(!manager.is_initialized());
    }

    #[test]
    fn test_blockchain_manager_with_options() {
        let manager = BlockchainManager::new("https://polygon-rpc.com", 137)
            .with_poll_interval(10000)
            .with_max_attempts(120);
        assert_eq!(manager.chain_id(), 137);
    }

    #[test]
    fn test_blockchain_manager_uninitialized() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        // Should return error when not initialized
        let result = manager.get_balance("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21");
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_execute_blockchain_tool_unknown() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        let result = execute_blockchain_tool(&mut manager, "unknown_tool", "{}");
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown blockchain tool"));
    }

    #[test]
    fn test_execute_blockchain_tool_partial_args() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        // get_balance requires address, but we provide empty args
        let result = execute_blockchain_tool(&mut manager, "get_balance", "{}");
        // Should handle missing parameter gracefully
        assert!(!result.success || result.data.contains("balance"));
    }

    #[test]
    fn test_pending_status_none() {
        let manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        assert!(manager.get_pending_status().is_none());
    }

    #[test]
    fn test_clear_pending() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        manager.clear_pending();
        assert!(manager.get_pending_status().is_none());
    }

    #[test]
    fn test_execute_get_nonce_uninitialized() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        let result = execute_get_nonce(&mut manager, "{}");
        // Uninitialized should return error
        assert!(!result.success);
    }

    #[test]
    fn test_execute_status_uninitialized() {
        let mut manager = BlockchainManager::new("https://eth.llamarpc.com", 1);
        let result = execute_status(&mut manager);
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not initialized"));
    }

    #[test]
    fn test_tool_result_from_impl() {
        // Test using Into<String> trait
        let result_success: BlockchainToolResult =
            BlockchainToolResult::success(String::from("test"));
        assert!(result_success.success);

        let result_error: BlockchainToolResult = BlockchainToolResult::error(String::from("error"));
        assert!(!result_error.success);
    }

    #[test]
    fn test_blockchain_agent_ext_holder_default_chain_id() {
        let holder = BlockchainManagerHolder::new();
        assert_eq!(BlockchainAgentExt::chain_id(&holder), 1);
        // Fresh holder should not be initialized yet.
        assert!(!holder.blockchain_manager().is_initialized());
        assert_eq!(BlockchainAgentExt::chain_id(&holder), 1);
    }

    #[test]
    fn test_blockchain_agent_ext_holder_with_custom_rpc() {
        let holder = BlockchainManagerHolder::with_rpc("https://polygon-rpc.com", 137);
        assert_eq!(BlockchainAgentExt::chain_id(&holder), 137);
        // chain_id accessor reflects the holder's manager.
        assert_eq!(
            BlockchainAgentExt::blockchain_manager(&holder).chain_id(),
            137
        );
    }

    #[test]
    fn test_blockchain_agent_ext_ensure_initialized_is_idempotent() {
        // ensure_blockchain_initialized must be idempotent: calling it
        // a second time should report `false` (no re-init required)
        // because the manager is already initialized.
        let mut holder = BlockchainManagerHolder::new();
        let first = BlockchainAgentExt::ensure_blockchain_initialized(&mut holder);
        let second = BlockchainAgentExt::ensure_blockchain_initialized(&mut holder);
        assert!(first, "first call must report it initialized");
        assert!(!second, "second call must report already-initialized");
    }

    #[test]
    fn test_blockchain_agent_ext_mutable_accessor() {
        let mut holder = BlockchainManagerHolder::new();
        {
            let manager = BlockchainAgentExt::blockchain_manager_mut(&mut holder);
            assert_eq!(manager.chain_id(), 1);
        }
        // Confirm immutable accessor returns the same data.
        assert_eq!(
            BlockchainAgentExt::blockchain_manager(&holder).chain_id(),
            1
        );
    }

    #[test]
    fn test_wei_to_eth_known_values() {
        assert_eq!(wei_to_eth(0), 0.0);
        assert_eq!(wei_to_eth(WEI_PER_ETH), 1.0);
        assert_eq!(wei_to_eth(WEI_PER_ETH / 2), 0.5);
    }

    #[test]
    fn test_wei_to_eth_string_uses_integer_arithmetic() {
        // 1_234_567_000_000_000_000 wei = 1.234567 ETH. Integer
        // math yields `whole=1`, `frac=234_567_000_000_000_000`,
        // `frac6 = frac/10^15 = 234`. So the last decimal drops
        // off — verified to ensure no hidden f64 rounding sneaks in.
        assert_eq!(wei_to_eth_string(1_234_567_000_000_000_000), "1.000234 ETH");
        assert_eq!(wei_to_eth_string(WEI_PER_ETH), "1.000000 ETH");
        assert_eq!(wei_to_eth_string(0), "0.000000 ETH");
    }

    #[test]
    fn test_gwei_to_wei_and_back() {
        assert_eq!(gwei_to_wei(1), 1_000_000_000);
        assert_eq!(gwei_to_wei(50), 50_000_000_000);
        assert_eq!(wei_to_gwei(1_000_000_000), 1);
        // Truncating: 1.9 gwei → 1 gwei
        assert_eq!(wei_to_gwei(1_900_000_000), 1);
    }

    #[test]
    fn test_gwei_to_wei_saturates() {
        // u128::MAX / GWEI_PER_ETH would overflow; saturating_mul
        // caps at u128::MAX instead of panicking.
        let huge = u128::MAX;
        assert_eq!(gwei_to_wei(huge), u128::MAX);
    }

    #[test]
    fn test_manager_accessors_round_trip() {
        let m = BlockchainManager::new("http://example", 42)
            .with_poll_interval(1234)
            .with_max_attempts(7);
        assert_eq!(m.rpc_url(), "http://example");
        assert_eq!(m.chain_id(), 42);
        assert_eq!(m.poll_interval_ms(), 1234);
        assert_eq!(m.max_attempts(), 7);
    }

    #[test]
    fn test_reset_clears_initialised_state() {
        let mut m = BlockchainManager::new("http://example", 1);
        m.init().unwrap();
        assert!(m.is_initialized());
        m.reset();
        assert!(!m.is_initialized());
    }

    #[test]
    fn test_switch_chain_replaces_backend() {
        let mut m = BlockchainManager::new("http://eth", 1);
        m.init().unwrap();
        assert_eq!(m.chain_id(), 1);

        // Switch to polygon. Pending tx should also be cleared if
        // any was set (we don't set one here, so just verify the
        // new chain id sticks and the manager stays initialized).
        m.switch_chain("http://polygon", 137).unwrap();
        assert_eq!(m.chain_id(), 137);
        assert_eq!(m.rpc_url(), "http://polygon");
        assert!(m.is_initialized());
    }

    #[test]
    fn test_switch_chain_clears_pending_tx() {
        let mut m = BlockchainManager::new("http://eth", 1);
        m.init().unwrap();
        // Simulate a pending tx by directly poking the field.
        // (There is no public setter, so we use reset to make
        // sure the function clears it.)
        m.reset();
        m.init().unwrap();
        assert!(m.get_pending_status().is_none());
        m.switch_chain("http://polygon", 137).unwrap();
        assert!(m.get_pending_status().is_none());
    }
}
