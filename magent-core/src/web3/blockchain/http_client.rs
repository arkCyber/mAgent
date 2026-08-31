//! HTTP-based RPC Client for standard environments.
//!
//! This module provides a working HTTP implementation for blockchain RPC calls
//! when the `std` feature is enabled (desktop/server environments).

#[cfg(feature = "std")]
use alloc::string::{String, ToString};
#[cfg(feature = "std")]
use alloc::vec::Vec;

#[cfg(feature = "std")]
use serde::Deserialize;
#[cfg(feature = "std")]
use serde_json::Value;

#[cfg(feature = "std")]
use crate::error::Web3ErrorKind;
#[cfg(feature = "std")]
use crate::web3::blockchain::client::{ChainClient, ChainId};
#[cfg(feature = "std")]
use crate::web3::blockchain::ChainConfig;
#[cfg(feature = "std")]
use crate::web3::blockchain::{Address, Hash, Wei};

// Re-export the common types for use in this module
#[cfg(all(feature = "std", feature = "esp32"))]
use super::esp32_http::{JsonRpcRequest, JsonRpcResponse, JsonRpcResult};
#[cfg(all(feature = "std", not(feature = "esp32")))]
use super::http_types::{JsonRpcRequest, JsonRpcResponse, JsonRpcResult};

// ============================================================================
// HTTP RPC Client
// ============================================================================

/// HTTP-based JSON-RPC client for blockchain interactions.
/// Uses the standard library's HTTP capabilities.
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct HttpRpcClient {
    /// RPC endpoint URL
    rpc_url: String,
    /// Chain ID
    chain_id: ChainId,
    /// HTTP request timeout in seconds
    timeout_secs: u64,
    /// Maximum number of retry attempts for transient failures.
    /// 0 = no retries (just the initial attempt).
    max_retries: u32,
    /// Backoff base in milliseconds. Each retry waits
    /// `base_backoff_ms * 2^attempt` (capped at `max_backoff_ms`).
    base_backoff_ms: u64,
    /// Backoff ceiling in milliseconds.
    max_backoff_ms: u64,
}

#[cfg(feature = "std")]
impl HttpRpcClient {
    /// Create a new HTTP RPC client
    pub fn new(rpc_url: impl Into<String>, chain_id: ChainId) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            chain_id,
            timeout_secs: 30,
            max_retries: 3,
            base_backoff_ms: 100,
            max_backoff_ms: 2_000,
        }
    }

    /// Create from chain configuration
    pub fn from_chain(chain: ChainConfig) -> Self {
        let rpc = chain.rpc_url.unwrap_or_else(|| String::from(""));
        Self::new(rpc, chain.chain_id)
    }

    /// Set request timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Configure retry behaviour. `attempts` is the number of
    /// retries AFTER the initial attempt (so 0 = single attempt,
    /// 3 = up to 4 total tries).
    pub fn with_retry(mut self, attempts: u32) -> Self {
        self.max_retries = attempts;
        self
    }

    /// Configure exponential backoff bounds.
    pub fn with_backoff(mut self, base_ms: u64, max_ms: u64) -> Self {
        self.base_backoff_ms = base_ms;
        self.max_backoff_ms = max_ms;
        self
    }

    /// Read-only accessors (mostly for diagnostics).
    /// Per-request timeout in seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    /// Maximum number of retry attempts for transient RPC failures.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }
    /// Initial backoff delay in milliseconds, doubled up to
    /// [`Self::max_backoff_ms`] between retry attempts.
    pub fn base_backoff_ms(&self) -> u64 {
        self.base_backoff_ms
    }
    /// Upper bound on the per-retry backoff (in milliseconds).
    pub fn max_backoff_ms(&self) -> u64 {
        self.max_backoff_ms
    }

    /// Make a JSON-RPC call via HTTP POST
    pub fn call<R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<R, Web3ErrorKind> {
        let request = JsonRpcRequest::new(method, params);

        // Serialize request
        let body = serde_json::to_string(&request).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!("failed to serialize request: {}", e))
        })?;

        // Make HTTP POST with retry on transient errors.
        let response = self.http_post_with_retry(&body, method)?;

        // Parse response
        let rpc_response: JsonRpcResponse<R> = serde_json::from_str(&response).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!(
                "failed to parse response from {} (method={}): {}",
                self.rpc_url, method, e
            ))
        })?;

        match rpc_response.result {
            JsonRpcResult::Success { result, .. } => Ok(result),
            JsonRpcResult::Error { error, .. } => {
                // TRACE: REQ-NET-002 — surface all RPC errors verbatim.
                // The two `JsonRpcError` types (host `http_types::RpcError`
                // and ESP32 `esp32_http::JsonRpcError`) only differ in
                // whether `data` is present; copy the common fields via
                // their public accessors so the same renderer works on
                // either cfg branch.
                #[cfg(feature = "esp32")]
                let (code, message) = (error.code(), error.message().to_string());
                #[cfg(not(feature = "esp32"))]
                let (code, message) = (error.code, error.message.clone());
                let rpc_err = super::http_types::RpcError {
                    // `code` is `i32` on the esp32 path (`error.code()`) but
                    // already `i64` on host (`error.code`), so the `.into()`
                    // is cfg-dependent; silence the useless-conversion lint.
                    #[allow(clippy::useless_conversion)]
                    code: code.into(),
                    message,
                };
                Err(rpc_error_to_web3_error(&rpc_err, method, &self.rpc_url))
            }
        }
    }

    /// HTTP POST with retry on transport-level failures. We only
    /// retry on errors that *might* succeed next time (connection
    /// resets, timeouts, 5xx) — never on JSON-RPC application
    /// errors, which are returned directly by `call`.
    fn http_post_with_retry(&self, body: &str, method: &str) -> Result<String, Web3ErrorKind> {
        let mut last_err: Option<Web3ErrorKind> = None;
        for attempt in 0..=self.max_retries {
            match self.http_post(body) {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    let transport = matches_transport_error(&e);
                    if !transport || attempt == self.max_retries {
                        return Err(e);
                    }
                    last_err = Some(e);
                    let delay = self.backoff_delay(attempt);
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                }
            }
        }
        // Unreachable — the loop either returns Ok or returns the
        // last error on the final attempt. But keep a sensible
        // fallback for the type checker.
        Err(last_err.unwrap_or_else(|| {
            Web3ErrorKind::BlockchainError(format!(
                "RPC {} failed after retries against {}",
                method, self.rpc_url
            ))
        }))
    }

    /// `base * 2^attempt` capped at `max`. Attempt 0 is the
    /// shortest delay; attempt N+1 doubles.
    fn backoff_delay(&self, attempt: u32) -> u64 {
        let shift = attempt.min(20); // guard against overflow
        let base = self.base_backoff_ms.saturating_mul(1u64 << shift);
        base.min(self.max_backoff_ms)
    }

    /// Perform HTTP POST request.
    ///
    /// TRACE: REQ-NET-001. The std backend here is an opt-in feature
    /// `reqwest`; we keep the gate at `cfg(feature = "reqwest")` for
    /// fine-grained control but ALSO declare `reqwest` in
    /// `magent-core`'s `[features]` so the lint doesn't complain.
    #[cfg(feature = "reqwest")]
    fn http_post(&self, body: &str) -> Result<String, Web3ErrorKind> {
        // Build a blocking reqwest client
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build()
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("client build: {}", e)))?;

        // POST the JSON-RPC request
        let response = client
            .post(&self.rpc_url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("request failed: {}", e)))?;

        // Read response body
        response
            .text()
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("response read: {}", e)))
    }

    /// Fallback HTTP implementation that returns an error indicating
    /// the user should enable the `reqwest` feature.
    #[cfg(not(feature = "reqwest"))]
    fn http_post(&self, body: &str) -> Result<String, Web3ErrorKind> {
        let _ = body;
        Err(Web3ErrorKind::BlockchainError(format!(
            "reqwest feature not enabled - cannot POST to {}. Enable reqwest feature for real HTTP support.",
            self.rpc_url
        )))
    }
}

// ============================================================================
// Error classification & retry helper
// ============================================================================

/// JSON-RPC standard error codes (subset that we recognise).
///
/// Reference: [EIP-1474](https://eips.ethereum.org/EIPS/eip-1474)
/// and `go-ethereum`'s `errors.go`. We name the most common ones
/// because clients frequently log only the numeric code, which
/// makes triage harder than it needs to be.
#[cfg(feature = "std")]
fn rpc_error_name(code: i64) -> &'static str {
    match code {
        -32700 => "Parse error",
        -32600 => "Invalid request",
        -32601 => "Method not found",
        -32602 => "Invalid params",
        -32603 => "Internal error",
        -32000 => "Server error",
        -32001 => "Resource not found",
        -32002 => "Resource unavailable",
        -32003 => "Transaction rejected",
        -32004 => "Method not supported",
        -32005 => "Limit exceeded",
        -32006 => "JSON-RPC version not supported",
        -32010 => "Execution timeout",
        _ => "Unknown error",
    }
}

/// Render a [`super::http_types::RpcError`] into a
/// [`Web3ErrorKind::BlockchainError`] that includes the JSON-RPC
/// standard name, the method, and the endpoint URL.
#[cfg(feature = "std")]
fn rpc_error_to_web3_error(
    err: &super::http_types::RpcError,
    method: &str,
    rpc_url: &str,
) -> Web3ErrorKind {
    Web3ErrorKind::BlockchainError(format!(
        "RPC {} failed for method={} url={}: code={} ({}): {}",
        rpc_error_name(err.code),
        method,
        rpc_url,
        err.code,
        err.message,
        // `message` already included above; duplicate is fine, it
        // makes the diagnostic self-contained when the standard
        // name is empty.
        err.message,
    ))
}

/// True if this error looks like a transport-level failure
/// (timeout, connection reset, DNS, etc.) that a retry might fix.
/// Anything else (serialization, JSON-RPC app error, …) is
/// returned directly.
#[cfg(feature = "std")]
fn matches_transport_error(err: &Web3ErrorKind) -> bool {
    let msg = match err {
        Web3ErrorKind::BlockchainError(m) => m.as_str(),
        _ => return false,
    };
    let lower = msg.to_lowercase();
    let needles = [
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "broken pipe",
        "dns",
        "temporary failure",
        "try again",
        "request failed",
        "client build",
        "response read",
    ];
    needles.iter().any(|n| lower.contains(n))
}

#[cfg(feature = "std")]
impl ChainClient for HttpRpcClient {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    fn get_nonce(&self, address: &Address) -> Result<u64, Web3ErrorKind> {
        let params = vec![
            serde_json::json!(address.to_hex()),
            serde_json::json!("latest"),
        ];
        let result: String = self.call("eth_getTransactionCount", params)?;
        parse_hex_u64(&result)
    }

    fn get_gas_price(&self) -> Result<Wei, Web3ErrorKind> {
        let result: String = self.call("eth_gasPrice", vec![])?;
        parse_hex_wei(&result)
    }

    fn estimate_gas(&self, to: &Address, data: &[u8], value: Wei) -> Result<u64, Web3ErrorKind> {
        let call_object = serde_json::json!({
            "to": to.to_hex(),
            "data": format!("0x{}", hex_encode(data)),
            "value": format!("0x{:x}", value.as_wei())
        });
        let result: String = self.call("eth_estimateGas", vec![call_object])?;
        parse_hex_u64(&result)
    }

    fn get_balance(&self, address: &Address) -> Result<Wei, Web3ErrorKind> {
        let params = vec![
            serde_json::json!(address.to_hex()),
            serde_json::json!("latest"),
        ];
        let result: String = self.call("eth_getBalance", params)?;
        parse_hex_wei(&result)
    }

    fn get_block_number(&self) -> Result<u64, Web3ErrorKind> {
        let result: String = self.call("eth_blockNumber", vec![])?;
        parse_hex_u64(&result)
    }

    fn send_raw_transaction(&self, signed_tx: &[u8]) -> Result<Hash, Web3ErrorKind> {
        let tx_hex = format!("0x{}", hex_encode(signed_tx));
        let result: String =
            self.call("eth_sendRawTransaction", vec![serde_json::json!(tx_hex)])?;
        Hash::from_hex(&result)
    }

    fn get_transaction_receipt(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<crate::web3::blockchain::TransactionReceipt>, Web3ErrorKind> {
        let params = vec![serde_json::json!(tx_hash.to_hex())];
        let result: Option<crate::web3::blockchain::TransactionReceipt> =
            self.call("eth_getTransactionReceipt", params)?;
        Ok(result)
    }

    fn call(&self, to: &Address, data: &[u8]) -> Result<Vec<u8>, Web3ErrorKind> {
        let call_object = serde_json::json!({
            "to": to.to_hex(),
            "data": format!("0x{}", hex_encode(data)),
        });
        let result: String = self.call("eth_call", vec![call_object])?;
        parse_hex_bytes(&result)
    }

    fn get_code(&self, address: &Address) -> Result<Vec<u8>, Web3ErrorKind> {
        let params = vec![
            serde_json::json!(address.to_hex()),
            serde_json::json!("latest"),
        ];
        let result: String = self.call("eth_getCode", params)?;
        parse_hex_bytes(&result)
    }

    fn get_block(
        &self,
        block_number: u64,
    ) -> Result<Option<crate::web3::blockchain::Block>, Web3ErrorKind> {
        let params = vec![
            serde_json::json!(format!("0x{:x}", block_number)),
            serde_json::Value::from(false),
        ];
        let result: Option<crate::web3::blockchain::Block> =
            self.call("eth_getBlockByNumber", params)?;
        Ok(result)
    }

    fn get_logs(
        &self,
        filter: &crate::web3::blockchain::EventFilter,
    ) -> Result<Vec<crate::web3::blockchain::EventLog>, Web3ErrorKind> {
        // Convert topics [Option<Hash>; 4] to a JSON array of strings (or null).
        let topics_json: Vec<serde_json::Value> = filter
            .topics
            .iter()
            .map(|t| match t {
                Some(h) => serde_json::Value::String(h.to_hex()),
                None => serde_json::Value::Null,
            })
            .collect();

        let filter_obj = serde_json::json!({
            "address": filter.address.map(|a| a.to_hex()),
            "topics": topics_json,
            "fromBlock": format!("0x{:x}", filter.from_block),
            "toBlock": format!("0x{:x}", filter.to_block),
        });
        let result: Vec<crate::web3::blockchain::EventLog> =
            self.call("eth_getLogs", vec![filter_obj])?;
        Ok(result)
    }

    fn health_check(&self) -> Result<(), Web3ErrorKind> {
        let _: String = self.call("eth_blockNumber", vec![])?;
        Ok(())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

#[cfg(feature = "std")]
fn parse_hex_u64(hex: &str) -> Result<u64, Web3ErrorKind> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    u64::from_str_radix(hex, 16)
        .map_err(|e| Web3ErrorKind::BlockchainError(format!("failed to parse u64: {}", e)))
}

#[cfg(feature = "std")]
fn parse_hex_wei(hex: &str) -> Result<Wei, Web3ErrorKind> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    let wei = u128::from_str_radix(hex, 16)
        .map_err(|e| Web3ErrorKind::BlockchainError(format!("failed to parse wei: {}", e)))?;
    Ok(Wei(wei))
}

#[cfg(feature = "std")]
fn parse_hex_bytes(hex: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if !hex.len().is_multiple_of(2) {
        return Err(Web3ErrorKind::BlockchainError("odd hex length".to_string()));
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
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

#[cfg(feature = "std")]
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
// Tests
// ============================================================================

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn test_http_client_creation() {
        let client = HttpRpcClient::new("https://eth.llamarpc.com", 1);
        assert_eq!(client.chain_id(), 1);
    }

    #[test]
    fn test_http_client_with_timeout() {
        let client = HttpRpcClient::new("https://eth.llamarpc.com", 1).with_timeout(60);
        assert_eq!(client.rpc_url(), "https://eth.llamarpc.com");
    }

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64("0x100").unwrap(), 256);
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0);
        assert_eq!(parse_hex_u64("0xF").unwrap(), 15);
    }

    #[test]
    fn test_parse_hex_u64_invalid() {
        assert!(parse_hex_u64("invalid").is_err());
        assert!(parse_hex_u64("0xGG").is_err());
    }

    #[test]
    fn test_parse_hex_wei() {
        let wei = parse_hex_wei("0xde0b6b3a7640000").unwrap(); // 1 ETH
        assert_eq!(wei.as_wei(), 1_000_000_000_000_000_000);
    }

    #[test]
    fn test_parse_hex_bytes() {
        let bytes = parse_hex_bytes("0x48656c6c6f").unwrap();
        assert_eq!(bytes, b"Hello");
    }

    #[test]
    fn test_parse_hex_bytes_empty() {
        let bytes = parse_hex_bytes("0x").unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn test_parse_hex_bytes_odd_length() {
        assert!(parse_hex_bytes("0x123").is_err());
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(b"Hello"), "48656c6c6f");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(b"\x00\xff"), "00ff");
    }

    #[test]
    fn test_client_chain_id_and_url_accessors() {
        let client = HttpRpcClient::new("https://polygon-rpc.com", 137);
        assert_eq!(client.chain_id(), 137);
        assert_eq!(client.rpc_url(), "https://polygon-rpc.com");
    }

    #[test]
    fn test_client_from_chain_config() {
        let chain = ChainConfig::new(1, "Ethereum").with_rpc_url("https://eth.llamarpc.com");
        let client = HttpRpcClient::from_chain(chain);
        assert_eq!(client.chain_id(), 1);
        assert_eq!(client.rpc_url(), "https://eth.llamarpc.com");
    }

    // PATCHED (MicroAgent): this test only makes sense when `reqwest` is OFF
    // (it asserts the no-reqwest error path). Now that `std` enables `reqwest`
    // by default, gate it on the feature being disabled.
    #[test]
    #[cfg(not(feature = "reqwest"))]
    fn test_client_post_without_reqwest_returns_error() {
        // Without the `reqwest` feature, `http_post` returns a clear error
        // explaining what to enable. This avoids silently doing nothing.
        let client = HttpRpcClient::new("https://eth.llamarpc.com", 1);
        let result = client.http_post("{}");
        assert!(result.is_err());
    }

    #[test]
    fn test_client_call_returns_error_for_unreachable_endpoint() {
        // Calling RPC without reqwest enabled must surface the missing
        // feature, not panic.
        let client = HttpRpcClient::new("https://eth.llamarpc.com", 1);
        let result: core::result::Result<u64, _> = client
            .get_nonce(&Address::from_hex("0x0000000000000000000000000000000000000000").unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_event_log_serde_round_trip() {
        let log = crate::web3::blockchain::EventLog::new(
            Address::from_hex("0x0000000000000000000000000000000000000001").unwrap(),
        );
        let json = serde_json::to_string(&log).unwrap();
        let _decoded: crate::web3::blockchain::EventLog = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_rpc_error_name_canonical_codes() {
        // Spot-check that we map the standard JSON-RPC codes to
        // their conventional names.
        assert_eq!(rpc_error_name(-32700), "Parse error");
        assert_eq!(rpc_error_name(-32600), "Invalid request");
        assert_eq!(rpc_error_name(-32601), "Method not found");
        assert_eq!(rpc_error_name(-32602), "Invalid params");
        assert_eq!(rpc_error_name(-32603), "Internal error");
        assert_eq!(rpc_error_name(-32000), "Server error");
        assert_eq!(rpc_error_name(-32003), "Transaction rejected");
        assert_eq!(rpc_error_name(9999), "Unknown error");
    }

    #[test]
    fn test_rpc_error_to_web3_error_includes_method_and_url() {
        use crate::web3::blockchain::http_types::RpcError;
        let err = RpcError {
            code: -32003,
            message: "insufficient funds".into(),
        };
        let converted = rpc_error_to_web3_error(&err, "eth_sendRawTransaction", "http://x");
        match converted {
            Web3ErrorKind::BlockchainError(msg) => {
                assert!(msg.contains("Transaction rejected"));
                assert!(msg.contains("eth_sendRawTransaction"));
                assert!(msg.contains("http://x"));
                assert!(msg.contains("insufficient funds"));
            }
            _ => panic!("expected BlockchainError variant"),
        }
    }

    #[test]
    fn test_matches_transport_error_known_patterns() {
        // Each pattern below is a "transport" error that retry
        // should attempt again.
        for needle in [
            "request failed: connection refused",
            "client build: dns error",
            "response read: broken pipe",
            "request failed: timeout",
            "temporary failure in name resolution",
        ] {
            let e = Web3ErrorKind::BlockchainError(needle.into());
            assert!(
                matches_transport_error(&e),
                "expected transport retry for {needle}"
            );
        }
    }

    #[test]
    fn test_matches_transport_error_rejects_app_errors() {
        let e = Web3ErrorKind::BlockchainError("execution reverted: out of gas".into());
        assert!(!matches_transport_error(&e));
    }

    #[test]
    fn test_matches_transport_error_returns_false_for_other_variants() {
        // Non-BlockchainError variants (e.g. InvalidPublicKey) are
        // never transport errors.
        let e = Web3ErrorKind::InvalidPublicKey { actual_len: 0 };
        assert!(!matches_transport_error(&e));
    }

    #[test]
    fn test_backoff_delay_doubles_until_cap() {
        let client = HttpRpcClient::new("http://x", 1).with_backoff(100, 1_000);
        assert_eq!(client.backoff_delay(0), 100);
        assert_eq!(client.backoff_delay(1), 200);
        assert_eq!(client.backoff_delay(2), 400);
        assert_eq!(client.backoff_delay(3), 800);
        // Cap kicks in.
        assert_eq!(client.backoff_delay(4), 1_000);
        assert_eq!(client.backoff_delay(20), 1_000);
    }

    #[test]
    fn test_with_retry_configures_max_attempts() {
        let client = HttpRpcClient::new("http://x", 1).with_retry(7);
        assert_eq!(client.max_retries(), 7);
        assert_eq!(client.base_backoff_ms(), 100);
        assert_eq!(client.max_backoff_ms(), 2_000);
    }

    #[test]
    fn test_default_retry_is_three() {
        let client = HttpRpcClient::new("http://x", 1);
        assert_eq!(client.max_retries(), 3);
        assert_eq!(client.timeout_secs(), 30);
    }

    #[test]
    fn test_with_timeout_and_backoff() {
        let client = HttpRpcClient::new("http://x", 1)
            .with_timeout(15)
            .with_backoff(50, 500);
        assert_eq!(client.timeout_secs(), 15);
        assert_eq!(client.base_backoff_ms(), 50);
        assert_eq!(client.max_backoff_ms(), 500);
    }

    #[test]
    fn test_from_chain_uses_rpc_url_and_defaults() {
        use crate::web3::blockchain::ChainConfig;
        let mut c = ChainConfig::new(1, "Ethereum");
        c.rpc_url = Some("https://rpc.example.com".into());
        let client = HttpRpcClient::from_chain(c);
        assert_eq!(client.chain_id(), 1);
        // Missing URL must not panic.
        let c2 = ChainConfig::new(2, "Polygon");
        let client2 = HttpRpcClient::from_chain(c2);
        assert_eq!(client2.chain_id(), 2);
    }
}
