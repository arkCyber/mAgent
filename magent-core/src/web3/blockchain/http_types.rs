//! JSON-RPC types for the HTTP client.
//!
//! Defines the request / response envelopes shared by all chain-client
//! implementations.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version
    pub jsonrpc: String,
    /// Method name
    pub method: String,
    /// Method params
    pub params: Vec<Value>,
    /// Request id (kept as a raw value so callers can use string, int, or null)
    pub id: Value,
}

impl JsonRpcRequest {
    /// Build a new JSON-RPC request.
    pub fn new(method: &str, params: Vec<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: Value::from(1u64),
        }
    }
}

/// JSON-RPC response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse<T> {
    /// JSON-RPC version
    pub jsonrpc: String,
    /// Response body — `result` and `error` live at the top level of the
    /// wire object (siblings of `jsonrpc`/`id`), so we flatten the union so
    /// serde parses them from there rather than expecting a nested
    /// `{ result, id }` object. Without `flatten`, a success response
    /// (`{"result":"0x…","id":1}`) fails to parse as the untagged enum and
    /// every RPC call errors.
    #[serde(flatten)]
    pub result: JsonRpcResult<T>,
}

/// JSON-RPC response payload (success or error).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResult<T> {
    /// Successful response.
    Success {
        /// Result body
        result: T,
        /// Request id (mirrored)
        id: Value,
    },
    /// Error response.
    Error {
        /// Error details
        error: RpcError,
        /// Request id (mirrored)
        id: Value,
    },
}

/// JSON-RPC error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Numeric error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_success_response_with_flat_result() {
        // Regression: the host `JsonRpcResponse` used to hold `result` as a
        // non-flattened untagged enum, so a normal success payload
        // (`{"result":"0x…","id":1}` — where `result` is a plain string)
        // failed to deserialise and every live RPC call errored with
        // "did not match any variant of untagged enum JsonRpcResult".
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"0x1b"}"#;
        let r: JsonRpcResponse<String> = serde_json::from_str(json).unwrap();
        match r.result {
            JsonRpcResult::Success { result, .. } => assert_eq!(result, "0x1b"),
            JsonRpcResult::Error { .. } => panic!("expected success"),
        }
    }

    #[test]
    fn parses_error_response_with_flat_result() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}"#;
        let r: JsonRpcResponse<String> = serde_json::from_str(json).unwrap();
        match r.result {
            JsonRpcResult::Success { .. } => panic!("expected error"),
            JsonRpcResult::Error { error, .. } => {
                assert_eq!(error.code, -32000);
                assert_eq!(error.message, "boom");
            }
        }
    }

    #[test]
    fn round_trips_success_response() {
        // Serialisation must also keep `result` at the top level (flattened).
        let r = JsonRpcResponse::<String> {
            jsonrpc: "2.0".to_string(),
            result: JsonRpcResult::Success {
                id: serde_json::json!(1),
                result: "0x0".to_string(),
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"result\":\"0x0\""));
        assert!(s.contains("\"id\":1"));
    }
}
