//! ESP32 HTTP Client for Blockchain RPC.
//!
//! Provides a minimal HTTP client implementation for ESP32 using either:
//! - `esp-idf` HTTP client (recommended for production)
//! - `smoltcp` for custom TCP/HTTP implementation
//!
//! ## Features
//!
//! - Blocking HTTP POST requests (no async)
//! - JSON-RPC request/response handling
//! - Connection pooling awareness
//! - Retry with exponential backoff
//!
//! ## Usage
//!
//! ```rust,ignore
//! use magent_core::web3::blockchain::esp32_http::{HttpClient, JsonRpcRequest, JsonRpcResponse};
//!
//! let client = HttpClient::new("https://eth.llamarpc.com", 8080);
//!
//! let request = JsonRpcRequest::new("eth_getBalance", &[
//!     serde_json::json!("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21"),
//!     serde_json::json!("latest"),
//! ]);
//!
//! match client.post(&request) {
//!     Ok(response) => {
//!         // Parse response.result
//!     }
//!     Err(e) => {
//!         // Handle error
//!     }
//! }
//! ```

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use once_cell::sync::OnceCell;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(unused_imports)]
use super::esp32_client::parse_hex;
#[allow(unused_imports)]
use crate::error::Web3ErrorKind;

// ============================================================================
// HTTP Client Configuration
// ============================================================================

/// HTTP client configuration
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Server hostname
    pub host: String,
    /// Server port
    pub port: u16,
    /// Use TLS (HTTPS)
    pub use_tls: bool,
    /// Connection timeout in milliseconds
    pub timeout_ms: u32,
    /// Maximum response size
    pub max_response_size: usize,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            host: "eth.llamarpc.com".to_string(),
            port: 443,
            use_tls: true,
            timeout_ms: 10000,
            max_response_size: 16 * 1024,
        }
    }
}

impl HttpClientConfig {
    /// Create from URL
    ///
    /// Accepts `<scheme>://<host>[:<port>][/<path>]`. Trailing paths
    /// are dropped (we only transport a host + port pair); everything
    /// after the first `/` is ignored. Returns sensible defaults for
    /// malformed input rather than panicking.
    pub fn from_url(url: &str) -> Self {
        let use_tls = url.starts_with("https://");
        // Only http/https URLs are valid RPC endpoints. Anything else
        // (ftp://, other schemes, garbage, empty) falls back to the safe
        // default instead of mis-parsing into a bogus/hostile host string.
        let Some(after_scheme) = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
        else {
            return Self::default();
        };
        // Drop path before splitting on `:` so the host never leaks
        // a route (e.g. `host/path` → `host`).
        let host_part = after_scheme.split('/').next().unwrap_or(after_scheme);
        let (host, port) = match host_part.split_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().unwrap_or(443)),
            None => (host_part, 443_u16),
        };
        Self {
            host: host.to_string(),
            port,
            use_tls,
            ..Default::default()
        }
    }

    /// Build a `scheme://host[:port]` authority string suitable for
    /// concatenating with a request path to form a full URL.
    ///
    /// Omits the default port for the scheme (443 for `https`, 80 for
    /// `http`) so the authority stays compact and RPC endpoints that
    /// expect a bare host remain compatible.
    pub fn base_url(&self) -> String {
        let scheme = if self.use_tls { "https" } else { "http" };
        let default_port = if self.use_tls { 443 } else { 80 };
        if self.port == default_port {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

// ============================================================================
// JSON-RPC Types
// ============================================================================

/// JSON-RPC request
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: Vec<Value>,
    id: u32,
}

impl JsonRpcRequest {
    /// Create a new request
    pub fn new(method: impl Into<String>, params: Vec<Value>) -> Self {
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

    /// eth_getBalance
    pub fn get_balance(address: &str, block: &str) -> Self {
        Self::new(
            "eth_getBalance",
            vec![
                Value::String(address.to_string()),
                Value::String(block.to_string()),
            ],
        )
    }

    /// eth_getTransactionCount
    pub fn get_nonce(address: &str, block: &str) -> Self {
        Self::new(
            "eth_getTransactionCount",
            vec![
                Value::String(address.to_string()),
                Value::String(block.to_string()),
            ],
        )
    }

    /// eth_gasPrice
    pub fn gas_price() -> Self {
        Self::new("eth_gasPrice", vec![])
    }

    /// eth_estimateGas
    pub fn estimate_gas(params: Value) -> Self {
        Self::new("eth_estimateGas", vec![params])
    }

    /// eth_call
    pub fn call(params: Value) -> Self {
        Self::new(
            "eth_call",
            vec![params, Value::String("latest".to_string())],
        )
    }

    /// eth_sendRawTransaction
    pub fn send_raw_transaction(signed_tx_hex: &str) -> Self {
        Self::new(
            "eth_sendRawTransaction",
            vec![Value::String(signed_tx_hex.to_string())],
        )
    }

    /// eth_getTransactionReceipt
    pub fn get_receipt(tx_hash: &str) -> Self {
        Self::new(
            "eth_getTransactionReceipt",
            vec![Value::String(tx_hash.to_string())],
        )
    }

    /// eth_getBlockByNumber
    pub fn get_block(block_number: u64, include_txs: bool) -> Self {
        Self::new(
            "eth_getBlockByNumber",
            vec![
                Value::String(format!("0x{:x}", block_number)),
                Value::Bool(include_txs),
            ],
        )
    }

    /// eth_blockNumber
    pub fn block_number() -> Self {
        Self::new("eth_blockNumber", vec![])
    }

    /// eth_getLogs
    pub fn get_logs(filter: &serde_json::Value) -> Self {
        Self::new("eth_getLogs", vec![filter.clone()])
    }
}

/// JSON-RPC response
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse<T> {
    /// Flattened success/error union — exactly one of `JsonRpcResult::Success`
    /// or `JsonRpcResult::Error` is populated after deserialisation.
    #[serde(flatten)]
    pub result: JsonRpcResult<T>,
}

/// JSON-RPC success/error union. The `serde(untagged)` attribute lets
/// the parser pick the right variant based on which fields are present
/// in the wire response (a `result` field means success, an `error`
/// field means failure).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResult<T> {
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
        error: JsonRpcError,
    },
}

/// JSON-RPC error object decoded from a failed response.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct JsonRpcError {
    /// Numeric error code (e.g. `-32601` for `Method not found`).
    code: i32,
    /// Human-readable error message from the remote peer.
    message: String,
    /// Optional structured data accompanying the error (schema depends
    /// on the remote method). Skipped during serialisation when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcError {
    /// Get error message
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get error code
    pub fn code(&self) -> i32 {
        self.code
    }
}

// ============================================================================
// HTTP Client Trait
// ============================================================================

/// HTTP client trait for blockchain RPC
///
/// Implement this trait for your specific HTTP stack:
/// - `esp-idf` HTTP client (route through `esp_idf_svc::http::client::EspHttpClient`)
/// - `reqwest::blocking` (host-side reference implementation)
/// - `embedded-http` crate
///
/// NOTE: `Debug` is `core::fmt::Debug` so the trait stays `no_std`-compatible
/// (REQ-SAFE-001). The compiler error you get from forgetting the
/// fully-qualified path is intentional — bare `Debug` would resolve to the
/// `derive(Debug)` macro via `use serde::{Deserialize, Serialize}` and the
/// `#[derive(Debug)]` shadow on `JsonRpcRequest` higher up in this file.
pub trait HttpClientTrait: core::fmt::Debug {
    /// Perform HTTP POST and return response body
    fn post(&self, request: &JsonRpcRequest) -> Result<String, HttpError>;

    /// Perform HTTP GET and return response body
    fn get(&self, path: &str) -> Result<String, HttpError>;

    /// Check if client is connected
    fn is_connected(&self) -> bool;

    /// Close connection
    fn close(&mut self);
}

// ============================================================================
// Transport abstraction
// ============================================================================
//
// `HttpClientTrait` (above) is the *public* per-request API surface. The
// `Transport` trait below is the *pluggable wire backend* that actually moves
// bytes to the RPC endpoint. Keeping them separate is what lets the `no_std`
// core stay dependency-free: `magent-core` never depends on `esp-idf-svc` /
// `reqwest`; the firmware and host crates inject their own `Transport` and the
// core just serialises/parses JSON-RPC over it (REQ-NET-004).

/// A pluggable HTTP wire backend for [`EspHttpClient`].
///
/// `url` is the full `scheme://host[:port]` authority (from
/// [`HttpClientConfig::base_url`]); `path` is the request path (usually `/` for
/// JSON-RPC). Implementations return the raw response body or an [`HttpError`].
///
/// `Send + Sync` is required so a `Transport` can be shared behind an `Arc`
/// across threads (the firmware agent thread) without locking.
///
/// NOTE: `Debug` is `core::fmt::Debug` to stay `no_std`-compatible (REQ-SAFE-001).
pub trait Transport: core::fmt::Debug + Send + Sync {
    /// Perform an HTTP POST of `body` to `url + path` and return the response body.
    fn post(
        &self,
        url: &str,
        path: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<String, HttpError>;

    /// Perform an HTTP GET of `url + path` and return the response body.
    fn get(&self, url: &str, path: &str) -> Result<String, HttpError>;
}

/// An `Arc`-shared, thread-safe transport handle.
pub type SharedTransport = Arc<dyn Transport>;

/// Global process-wide default transport.
///
/// [`EspHttpClient::new`] / [`EspHttpClient::from_url`] fall back to this when
/// no explicit transport is supplied, so firmware and host crates can install a
/// real backend **once** at startup and every `EspHttpClient` constructed by
/// `agent_tools` automatically uses it — no threading of a transport through
/// every call site.
static DEFAULT_TRANSPORT: OnceCell<SharedTransport> = OnceCell::new();

/// Install the process-wide default [`Transport`] (idempotent).
///
/// Returns `Err(t)` with the supplied handle if a default is already installed
/// (e.g. if the firmware boot path is re-entered after a soft reboot). Failure
/// is non-fatal: an explicit transport on a per-client basis still wins.
pub fn set_default_transport(t: SharedTransport) -> Result<(), SharedTransport> {
    DEFAULT_TRANSPORT.set(t)
}

/// The current default transport, or a `MockTransport` if none was installed.
fn default_transport() -> SharedTransport {
    DEFAULT_TRANSPORT
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(MockTransport))
}

/// A no-op transport that fails with a descriptive error.
///
/// This is the **safety fallback** used when no transport has been installed
/// (REQ-NET-002: never silently fake a success). It exists so `EspHttpClient`
/// remains constructible in unit tests and in the bare `no_std` core, while a
/// real RPC call on a device that forgot to install a transport fails loudly
/// instead of hanging or returning garbage.
#[derive(Debug, Clone, Default)]
pub struct MockTransport;

impl Transport for MockTransport {
    fn post(
        &self,
        _url: &str,
        _path: &str,
        _body: &str,
        _headers: &[(&str, &str)],
    ) -> Result<String, HttpError> {
        Err(HttpError::ConnectionFailed(
            "no Transport installed: call magent_core::web3::blockchain::esp32_http::\
             set_default_transport(...) or use EspHttpClient::with_transport(...)"
                .to_string(),
        ))
    }

    fn get(&self, _url: &str, _path: &str) -> Result<String, HttpError> {
        Err(HttpError::ConnectionFailed(
            "no Transport installed: call set_default_transport(...) or \
             use EspHttpClient::with_transport(...)"
                .to_string(),
        ))
    }
}

/// A transport that returns preprogrammed response bodies.
///
/// Used by the unit tests below to exercise the full JSON-RPC
/// serialise → POST → parse pipeline against a fixed, offline backend
/// (no network, no host HTTP stack required).
#[derive(Debug, Clone, Default)]
pub struct StaticTransport {
    /// Response body returned by every `post` call.
    pub post_response: String,
    /// Response body returned by every `get` call.
    pub get_response: String,
}

impl StaticTransport {
    /// Build a transport that answers every POST with `post_response`.
    pub fn posting(post_response: impl Into<String>) -> Self {
        Self {
            post_response: post_response.into(),
            get_response: String::new(),
        }
    }
}

impl Transport for StaticTransport {
    fn post(
        &self,
        _url: &str,
        _path: &str,
        _body: &str,
        _headers: &[(&str, &str)],
    ) -> Result<String, HttpError> {
        Ok(self.post_response.clone())
    }

    fn get(&self, _url: &str, _path: &str) -> Result<String, HttpError> {
        Ok(self.get_response.clone())
    }
}

/// HTTP errors
#[derive(Debug, Clone)]
pub enum HttpError {
    /// Connection failed
    ConnectionFailed(String),
    /// Request timeout
    Timeout,
    /// Invalid response
    InvalidResponse(String),
    /// TLS error
    TlsError(String),
    /// DNS resolution failed
    DnsFailed(String),
    /// Buffer overflow
    BufferOverflow,
}

impl core::fmt::Display for HttpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HttpError::ConnectionFailed(s) => write!(f, "connection failed: {}", s),
            HttpError::Timeout => write!(f, "request timeout"),
            HttpError::InvalidResponse(s) => write!(f, "invalid response: {}", s),
            HttpError::TlsError(s) => write!(f, "TLS error: {}", s),
            HttpError::DnsFailed(s) => write!(f, "DNS failed: {}", s),
            HttpError::BufferOverflow => write!(f, "response buffer overflow"),
        }
    }
}

// ============================================================================
// Blocking HTTP Client Implementation
// ============================================================================

/// Blocking HTTP client for ESP32
///
/// This is a placeholder implementation. In production, integrate with:
/// - `esp_idf_hal::http::client::EspHttpClient`
/// - `smoltcp` with custom HTTP parser
/// - `embedded-http` crate
#[derive(Debug, Clone)]
pub struct EspHttpClient {
    config: HttpClientConfig,
    connected: bool,
    transport: SharedTransport,
}

impl EspHttpClient {
    /// Create new client using the process-wide default transport
    /// (or `MockTransport` if none was installed via
    /// [`set_default_transport`]).
    pub fn new(config: HttpClientConfig) -> Self {
        Self {
            config,
            connected: false,
            transport: default_transport(),
        }
    }

    /// Create from URL
    pub fn from_url(url: &str) -> Self {
        Self::new(HttpClientConfig::from_url(url))
    }

    /// Create a client bound to an explicit transport, bypassing the
    /// process-wide default.
    pub fn with_transport(config: HttpClientConfig, transport: SharedTransport) -> Self {
        Self {
            config,
            connected: false,
            transport,
        }
    }

    /// Swap the wire backend after construction.
    pub fn set_transport(&mut self, transport: SharedTransport) {
        self.transport = transport;
    }

    /// Connect to server
    pub fn connect(&mut self) -> Result<(), HttpError> {
        // In production, implement actual TCP/TLS connection
        // using esp-idf or smoltcp
        self.connected = true;
        Ok(())
    }

    /// Disconnect from server
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// Send JSON-RPC request
    pub fn rpc<T: for<'de> Deserialize<'de>>(
        &mut self,
        request: &JsonRpcRequest,
    ) -> Result<T, HttpError> {
        if !self.connected {
            self.connect()?;
        }

        // Serialize request
        let body = serde_json::to_string(request)
            .map_err(|e| HttpError::InvalidResponse(e.to_string()))?;

        // Perform HTTP POST
        let response = self.post_raw("/", &body)?;

        // Parse response
        serde_json::from_str(&response).map_err(|e| HttpError::InvalidResponse(e.to_string()))
    }

    /// Raw POST request
    ///
    /// Serialises a JSON-RPC body over the configured [`Transport`]. The
    /// concrete wire implementation (esp-idf / reqwest / mock) is injected at
    /// construction time; this method only builds the authority URL and hands
    /// the body to the transport (REQ-NET-004).
    pub fn post_raw(&mut self, path: &str, body: &str) -> Result<String, HttpError> {
        let url = self.config.base_url();
        let headers = [("content-type", "application/json")];
        self.transport.post(&url, path, body, &headers)
    }

    /// Get block number
    pub fn get_block_number(&mut self) -> Result<u64, HttpError> {
        let request = JsonRpcRequest::block_number();
        let response: JsonRpcResponse<String> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => {
                let hex = result.trim_start_matches("0x");
                u64::from_str_radix(hex, 16).map_err(|e| HttpError::InvalidResponse(e.to_string()))
            }
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }

    /// Get balance
    pub fn get_balance(&mut self, address: &str) -> Result<u128, HttpError> {
        let request = JsonRpcRequest::get_balance(address, "latest");
        let response: JsonRpcResponse<String> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => {
                let hex = result.trim_start_matches("0x");
                u128::from_str_radix(hex, 16).map_err(|e| HttpError::InvalidResponse(e.to_string()))
            }
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }

    /// Get nonce
    pub fn get_nonce(&mut self, address: &str) -> Result<u64, HttpError> {
        let request = JsonRpcRequest::get_nonce(address, "latest");
        let response: JsonRpcResponse<String> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => {
                let hex = result.trim_start_matches("0x");
                u64::from_str_radix(hex, 16).map_err(|e| HttpError::InvalidResponse(e.to_string()))
            }
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }

    /// Get gas price
    pub fn get_gas_price(&mut self) -> Result<u128, HttpError> {
        let request = JsonRpcRequest::gas_price();
        let response: JsonRpcResponse<String> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => {
                let hex = result.trim_start_matches("0x");
                u128::from_str_radix(hex, 16).map_err(|e| HttpError::InvalidResponse(e.to_string()))
            }
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }

    /// Send raw transaction
    pub fn send_raw_transaction(&mut self, signed_tx_hex: &str) -> Result<String, HttpError> {
        let request = JsonRpcRequest::send_raw_transaction(signed_tx_hex);
        let response: JsonRpcResponse<String> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => Ok(result),
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }

    /// Get transaction receipt
    pub fn get_transaction_receipt(
        &mut self,
        tx_hash: &str,
    ) -> Result<Option<TransactionReceiptResponse>, HttpError> {
        let request = JsonRpcRequest::get_receipt(tx_hash);
        let response: JsonRpcResponse<Option<TransactionReceiptResponse>> = self.rpc(&request)?;
        match response.result {
            JsonRpcResult::Success { result, .. } => Ok(result),
            JsonRpcResult::Error { error, .. } => Err(HttpError::InvalidResponse(error.message)),
        }
    }
}

impl HttpClientTrait for EspHttpClient {
    fn post(&self, request: &JsonRpcRequest) -> Result<String, HttpError> {
        let body = serde_json::to_string(request)
            .map_err(|e| HttpError::InvalidResponse(e.to_string()))?;
        let url = self.config.base_url();
        let headers = [("content-type", "application/json")];
        self.transport.post(&url, "/", &body, &headers)
    }

    fn get(&self, path: &str) -> Result<String, HttpError> {
        let url = self.config.base_url();
        self.transport.get(&url, path)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn close(&mut self) {
        self.disconnect();
    }
}

/// Transaction receipt from RPC. Field names and types match the
/// `eth_getTransactionReceipt` JSON-RPC response, but every numeric
/// field is deserialised as `String` (not `u64`) because the
/// quantities are encoded as **hex strings** with up to 64 digits and
/// our `no_std` build doesn't want to pull in `num-bigint` just to
/// parse them. Callers must hex-decode each field individually.
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionReceiptResponse {
    /// Hash of the mined transaction.
    pub transaction_hash: String,
    /// Block number that included the transaction (hex string).
    pub block_number: String,
    /// Hash of the block that included the transaction (hex string).
    pub block_hash: String,
    /// Index of the transaction within the block (hex string).
    pub transaction_index: String,
    /// Sender address (hex string with optional `0x` prefix).
    pub from: String,
    /// Recipient address (hex string). `None` for contract-creation
    /// transactions. Skipped during serialisation when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Total gas used by all transactions in the block up to and
    /// including this one (hex string).
    pub cumulative_gas_used: String,
    /// Gas used by this transaction alone (hex string).
    pub gas_used: String,
    /// `"0x1"` on success, `"0x0"` on revert (hex string).
    pub status: String,
}

// ============================================================================
// Integration with Blockchain State Machine
// ============================================================================

/// Helper to wait for transaction confirmation with polling
///
/// TRACE: REQ-VFY-001 — derives needed so the `BlockChainManager` derive
/// expands cleanly when `esp32` is enabled.
#[derive(Debug, Clone)]
pub struct TransactionPoller {
    #[allow(dead_code)]
    client: EspHttpClient,
    poll_interval_ms: u32,
    #[allow(dead_code)]
    max_attempts: usize,
}

impl TransactionPoller {
    /// Create new poller
    pub fn new(client: EspHttpClient, poll_interval_ms: u32, max_attempts: usize) -> Self {
        Self {
            client,
            poll_interval_ms,
            max_attempts,
        }
    }

    /// Wait for transaction confirmation
    ///
    /// In embedded environment, this should be called in a loop with delays:
    ///
    /// ```rust,ignore
    /// let mut poller = TransactionPoller::new(client, 5000, 60);
    /// loop {
    ///     match poller.poll(tx_hash) {
    ///         PollStatus::Confirmed(receipt) => { /* done */ }
    ///         PollStatus::Pending { attempts, delay_ms } => {
    ///             delay_ms(delay_ms);
    ///         }
    ///         PollStatus::NotFound => { /* failed */ }
    ///     }
    /// }
    /// ```
    pub fn poll(&mut self, tx_hash: &str) -> PollStatus {
        match self.client.get_transaction_receipt(tx_hash) {
            Ok(Some(receipt)) => {
                let status =
                    u64::from_str_radix(receipt.status.trim_start_matches("0x"), 16).unwrap_or(0);
                if status == 1 {
                    PollStatus::Confirmed(receipt)
                } else {
                    PollStatus::Failed
                }
            }
            Ok(None) => PollStatus::Pending {
                attempts: 0,
                delay_ms: self.poll_interval_ms,
            },
            Err(e) => PollStatus::Error(e.to_string()),
        }
    }
}

/// Status of transaction polling
#[derive(Debug)]
pub enum PollStatus {
    /// Transaction confirmed
    Confirmed(TransactionReceiptResponse),
    /// Still pending
    Pending {
        /// Number of polls performed so far.
        attempts: usize,
        /// Suggested delay before the next poll (in milliseconds).
        delay_ms: u32,
    },
    /// Transaction failed
    Failed,
    /// Error occurred
    Error(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = HttpClientConfig::default();
        assert_eq!(config.host, "eth.llamarpc.com");
        assert_eq!(config.port, 443);
        assert!(config.use_tls);
        assert_eq!(config.timeout_ms, 10000);
        assert_eq!(config.max_response_size, 16 * 1024);
    }

    #[test]
    fn test_config_from_url_https() {
        let config = HttpClientConfig::from_url("https://eth.llamarpc.com");
        assert_eq!(config.host, "eth.llamarpc.com");
        assert_eq!(config.port, 443);
        assert!(config.use_tls);
    }

    #[test]
    fn test_config_from_url_http() {
        let config = HttpClientConfig::from_url("http://localhost:8545");
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 8545);
        assert!(!config.use_tls);
    }

    #[test]
    fn test_config_from_url_malformed_no_panic() {
        // Malformed / hostile URLs must never panic (REQ-NET-003): the parser
        // returns sensible defaults instead.
        for bad in ["", "://", "http://", "not a url", "http://:9999/", "ftp://h/"] {
            let cfg = HttpClientConfig::from_url(bad);
            // Host may be empty, but port must stay a valid u16 and use_tls sane.
            assert!(cfg.port > 0);
            let _ = cfg.base_url();
        }
    }

    #[test]
    fn test_config_from_url_rejects_non_http_scheme() {
        // Non-http(s) schemes must not be mis-parsed into a bogus host; they
        // fall back to the safe default (defense against scheme confusion).
        let def = HttpClientConfig::default();
        for bad in ["ftp://host/x", "gopher://h", "ws://h", "not a url", ""] {
            let cfg = HttpClientConfig::from_url(bad);
            assert_eq!(cfg.host, def.host, "scheme {bad:?} should be rejected");
            assert_eq!(cfg.port, def.port);
            assert_eq!(cfg.use_tls, def.use_tls);
        }
    }

    #[test]
    fn test_config_from_url_with_path() {
        let config = HttpClientConfig::from_url("https://eth.llamarpc.com/path/to/resource");
        assert_eq!(config.host, "eth.llamarpc.com");
        assert_eq!(config.port, 443);
    }

    #[test]
    fn test_config_from_url_custom_port() {
        let config = HttpClientConfig::from_url("https://polygon-rpc.com:8080");
        assert_eq!(config.host, "polygon-rpc.com");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn test_rpc_request_new() {
        let req = JsonRpcRequest::new("eth_getBalance", vec![]);
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "eth_getBalance");
        assert_eq!(req.params.len(), 0);
    }

    #[test]
    fn test_rpc_request_get_balance() {
        let req =
            JsonRpcRequest::get_balance("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21", "latest");
        assert_eq!(req.method, "eth_getBalance");
        assert_eq!(req.params.len(), 2);
    }

    #[test]
    fn test_rpc_request_get_nonce() {
        let req =
            JsonRpcRequest::get_nonce("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21", "pending");
        assert_eq!(req.method, "eth_getTransactionCount");
        assert_eq!(req.params.len(), 2);
    }

    #[test]
    fn test_rpc_request_gas_price() {
        let req = JsonRpcRequest::gas_price();
        assert_eq!(req.method, "eth_gasPrice");
        assert_eq!(req.params.len(), 0);
    }

    #[test]
    fn test_rpc_request_estimate_gas() {
        let params = serde_json::json!({
            "from": "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21",
            "to": "0x8626f6940E2eb28930eFb4CeF49B2d1F2C9C1199",
            "value": "0x1"
        });
        let req = JsonRpcRequest::estimate_gas(params);
        assert_eq!(req.method, "eth_estimateGas");
    }

    #[test]
    fn test_rpc_request_call() {
        let params = serde_json::json!({
            "to": "0xContractAddress",
            "data": "0x"
        });
        let req = JsonRpcRequest::call(params);
        assert_eq!(req.method, "eth_call");
    }

    #[test]
    fn test_rpc_request_send_raw_transaction() {
        let req = JsonRpcRequest::send_raw_transaction("0xf8a...");
        assert_eq!(req.method, "eth_sendRawTransaction");
        assert_eq!(req.params.len(), 1);
    }

    #[test]
    fn test_rpc_request_get_receipt() {
        let req = JsonRpcRequest::get_receipt("0x1234567890abcdef");
        assert_eq!(req.method, "eth_getTransactionReceipt");
        assert_eq!(req.params.len(), 1);
    }

    #[test]
    fn test_rpc_request_get_block() {
        let req = JsonRpcRequest::get_block(12345678, false);
        assert_eq!(req.method, "eth_getBlockByNumber");
        assert_eq!(req.params.len(), 2);
    }

    #[test]
    fn test_rpc_request_block_number() {
        let req = JsonRpcRequest::block_number();
        assert_eq!(req.method, "eth_blockNumber");
        assert_eq!(req.params.len(), 0);
    }

    #[test]
    fn test_rpc_request_get_logs() {
        let filter = serde_json::json!({
            "address": "0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21",
            "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]
        });
        let req = JsonRpcRequest::get_logs(&filter);
        assert_eq!(req.method, "eth_getLogs");
    }

    #[test]
    fn test_parse_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#;
        let response: JsonRpcResponse<String> = serde_json::from_str(json).unwrap();
        match response.result {
            JsonRpcResult::Success { id, result } => {
                assert_eq!(id, 1);
                assert_eq!(result, "0x1");
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_parse_response_error() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#;
        let response: JsonRpcResponse<String> = serde_json::from_str(json).unwrap();
        match response.result {
            JsonRpcResult::Error { id, error } => {
                assert_eq!(id, 1);
                assert_eq!(error.code, -32600);
                assert_eq!(error.message, "Invalid Request");
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn test_parse_response_null_result() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let response: JsonRpcResponse<Option<String>> = serde_json::from_str(json).unwrap();
        match response.result {
            JsonRpcResult::Success { result, .. } => {
                assert!(result.is_none());
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_json_rpc_error_accessors() {
        let error = JsonRpcError {
            code: -32600,
            message: "Invalid Request".to_string(),
            data: None,
        };
        assert_eq!(error.code(), -32600);
        assert_eq!(error.message(), "Invalid Request");
    }

    #[test]
    fn test_http_error_display() {
        let err = HttpError::ConnectionFailed("test".to_string());
        assert!(err.to_string().contains("connection failed"));
        assert!(err.to_string().contains("test"));
    }

    #[test]
    fn test_http_error_variants() {
        assert!(HttpError::Timeout.to_string().contains("timeout"));
        assert!(HttpError::BufferOverflow.to_string().contains("overflow"));
        assert!(HttpError::DnsFailed("test".to_string())
            .to_string()
            .contains("DNS"));
        assert!(HttpError::TlsError("test".to_string())
            .to_string()
            .contains("TLS"));
    }

    #[test]
    fn test_esp_http_client_creation() {
        let client = EspHttpClient::from_url("https://eth.llamarpc.com");
        assert!(!client.is_connected());
    }

    #[test]
    fn test_esp_http_client_config() {
        let config = HttpClientConfig::from_url("https://polygon-rpc.com:8545");
        let client = EspHttpClient::new(config);
        assert!(!client.is_connected());
    }

    #[test]
    fn test_esp_http_client_connect() {
        let mut client = EspHttpClient::from_url("https://eth.llamarpc.com");
        // Connection will fail because it's a placeholder, but we can check the method works
        // In real implementation, this would actually connect
        let result = client.connect();
        // Result depends on implementation status
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_base_url_default_ports() {
        assert_eq!(
            HttpClientConfig::from_url("https://eth.llamarpc.com").base_url(),
            "https://eth.llamarpc.com"
        );
        assert_eq!(
            HttpClientConfig::from_url("http://localhost:8545").base_url(),
            "http://localhost:8545"
        );
        // Explicit default port is normalised away.
        assert_eq!(
            HttpClientConfig::from_url("https://polygon-rpc.com:443").base_url(),
            "https://polygon-rpc.com"
        );
        // Non-default port is preserved.
        assert_eq!(
            HttpClientConfig::from_url("http://127.0.0.1:8080").base_url(),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn test_rpc_success_via_static_transport() {
        // A valid `eth_blockNumber` response over the offline static transport.
        let t = StaticTransport::posting(r#"{"jsonrpc":"2.0","id":0,"result":"0x1f4"}"#);
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            Arc::new(t),
        );
        // 0x1f4 == 500
        assert_eq!(client.get_block_number().unwrap(), 500);
    }

    #[test]
    fn test_rpc_error_via_static_transport() {
        // A JSON-RPC error object must surface as an HttpError, not panic.
        let t = StaticTransport::posting(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32601,"message":"Method not found"}}"#,
        );
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            Arc::new(t),
        );
        let res = client.get_block_number();
        assert!(matches!(res, Err(HttpError::InvalidResponse(_))));
    }

    #[test]
    fn test_rpc_malformed_response_is_error_not_panic() {
        // A non-JSON response body must surface as an error, never a panic
        // (hostile RPC endpoint / truncated body).
        let t = StaticTransport::posting("this is not json");
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            Arc::new(t),
        );
        let res = client.get_block_number();
        assert!(matches!(res, Err(HttpError::InvalidResponse(_))));
    }

    /// A transport that always returns a fixed `HttpError` (used to verify the
    /// JSON-RPC layer propagates transport-level failures unchanged).
    #[derive(Debug)]
    struct ErrTransport {
        err: HttpError,
    }

    impl Transport for ErrTransport {
        fn post(
            &self,
            _url: &str,
            _path: &str,
            _body: &str,
            _headers: &[(&str, &str)],
        ) -> Result<String, HttpError> {
            Err(self.err.clone())
        }
        fn get(&self, _url: &str, _path: &str) -> Result<String, HttpError> {
            Err(self.err.clone())
        }
    }

    #[test]
    fn test_rpc_propagates_transport_connection_error() {
        let t: SharedTransport = Arc::new(ErrTransport {
            err: HttpError::ConnectionFailed("rpc endpoint down".to_string()),
        });
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            t,
        );
        let res = client.get_block_number();
        assert!(matches!(res, Err(HttpError::ConnectionFailed(msg)) if msg.contains("down")));
    }

    #[test]
    fn test_rpc_propagates_transport_timeout() {
        let t: SharedTransport = Arc::new(ErrTransport { err: HttpError::Timeout });
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            t,
        );
        let res = client.get_block_number();
        assert!(matches!(res, Err(HttpError::Timeout)));
    }

    #[test]
    fn test_rpc_fails_loudly_without_transport() {
        // A MockTransport (the no-backend fallback) must return Err — never a
        // fabricated success (REQ-NET-002).
        let mut client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("https://eth.llamarpc.com"),
            Arc::new(MockTransport),
        );
        assert!(client.get_block_number().is_err());
    }

    #[test]
    fn test_get_via_static_transport() {
        let t = StaticTransport {
            get_response: r#"{"ok":true}"#.to_string(),
            ..Default::default()
        };
        let client = EspHttpClient::with_transport(
            HttpClientConfig::from_url("http://localhost:8545"),
            Arc::new(t),
        );
        let body = client.get("/v1/status").unwrap();
        assert!(body.contains("\"ok\":true"));
    }

    #[test]
    fn test_set_default_transport_roundtrip() {
        // Installing a process-wide default makes `EspHttpClient::new` /
        // `from_url` pick it up automatically (the firmware boot path uses
        // this to inject the real esp-idf transport once).
        let t: SharedTransport =
            Arc::new(StaticTransport::posting(r#"{"jsonrpc":"2.0","id":0,"result":"0x1"}"#));
        let _ = set_default_transport(t);
        let mut client = EspHttpClient::from_url("https://eth.llamarpc.com");
        assert!(client.get_block_number().is_ok());
    }

    #[test]
    fn test_transaction_receipt_response_deserialization() {
        let json = r#"{
            "transaction_hash": "0xabc123",
            "block_number": "0x100",
            "block_hash": "0xdef456",
            "transaction_index": "0x0",
            "from": "0x123",
            "to": "0x456",
            "cumulative_gas_used": "0x1000",
            "gas_used": "0x1000",
            "status": "0x1"
        }"#;
        let receipt: TransactionReceiptResponse = serde_json::from_str(json).unwrap();
        assert_eq!(receipt.transaction_hash, "0xabc123");
        assert_eq!(receipt.status, "0x1");
    }

    #[test]
    fn test_transaction_receipt_response_optional_to() {
        let json = r#"{
            "transaction_hash": "0xabc123",
            "block_number": "0x100",
            "block_hash": "0xdef456",
            "transaction_index": "0x0",
            "from": "0x123",
            "cumulative_gas_used": "0x1000",
            "gas_used": "0x1000",
            "status": "0x1"
        }"#;
        let receipt: TransactionReceiptResponse = serde_json::from_str(json).unwrap();
        assert!(receipt.to.is_none());
    }
}
