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
    /// Response body
    pub result: JsonRpcResult<T>,
    /// Request id
    pub id: Value,
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
