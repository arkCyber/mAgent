//! BLE client for cloud communication.
//!
//! This is one of the concrete [`crate::communication::link::LinkAdapter`]
//! implementations. The HTTP-style request/response API below is kept
//! for backward compatibility with the original `BleClient` callers
//! (firmware / agent loop), but new code that needs to plug an external
//! data source into the agent should go through `LinkAdapter` + the
//! `IngressGateway` instead.

use crate::error::{AgentError, NetworkError, Result};
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum message size
const MAX_MESSAGE_SIZE: usize = 512;

/// BLE client for cloud communication
pub struct BleClient {
    connected: bool,
    /// Connection timeout, in milliseconds, used by `connect()` /
    /// send-and-await paths.
    pub timeout_ms: u32,
}

impl BleClient {
    /// Create a new BLE client
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            connected: false,
            timeout_ms,
        }
    }

    /// Create with default timeout
    pub fn with_defaults() -> Self {
        Self::new(30000) // 30 seconds
    }

    /// Connect to gateway
    pub async fn connect(&mut self) -> Result<()> {
        // In real implementation, this would establish BLE connection
        self.connected = true;
        Ok(())
    }

    /// Disconnect from gateway
    pub async fn disconnect(&mut self) -> Result<()> {
        self.connected = false;
        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Send request to cloud LLM API
    pub async fn send_request(&self, prompt: &str) -> Result<String<MAX_MESSAGE_SIZE>> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused,
            });
        }

        // Validate prompt length
        if prompt.len() > MAX_MESSAGE_SIZE {
            return Err(AgentError::InputValidationFailed {
                field: "prompt",
                reason: crate::error::ValidationError::TooLong,
            });
        }

        // In real implementation, this would:
        // 1. Serialize prompt to BLE message
        // 2. Send via BLE to gateway
        // 3. Gateway forwards to cloud LLM API (or local Ollama)
        // 4. Wait for response
        // 5. Deserialize response

        // For now, simulate response with more realistic content
        let response = if prompt.contains("temperature") {
            "The current temperature is 25.5°C"
        } else if prompt.contains("LED") {
            "LED has been turned on"
        } else if prompt.contains("flash") {
            "Configuration read from flash successfully"
        } else {
            "Task completed successfully"
        };

        Ok(heapless::String::try_from(response).unwrap())
    }

    /// Send tool result to cloud
    pub async fn send_tool_result(&self, _result: &ToolResult) -> Result<()> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused,
            });
        }

        // In real implementation, this would:
        // 1. Serialize tool result to BLE message
        // 2. Send via BLE to gateway
        // 3. Gateway forwards to LLM API

        Ok(())
    }

    /// Receive response from cloud
    pub async fn receive_response(&self) -> Result<LlmResponse> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused,
            });
        }

        // In real implementation, this would:
        // 1. Wait for BLE message from gateway
        // 2. Deserialize LLM response
        // 3. Parse tool calls if any

        // For now, simulate response
        Ok(LlmResponse {
            content: heapless::String::try_from("Response content").unwrap(),
            tool_calls: Vec::new(),
            finish_reason: heapless::String::try_from("stop").unwrap(),
        })
    }
}

/// LLM response from cloud API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Response content
    pub content: String<MAX_MESSAGE_SIZE>,
    /// Tool calls requested
    pub tool_calls: Vec<ToolCall, 4>,
    /// Finish reason
    pub finish_reason: String<16>,
}

/// Tool call from LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name
    pub name: String<32>,
    /// Tool arguments (JSON)
    pub arguments: String<128>,
}

/// Tool result to send back
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool name
    pub tool_name: String<32>,
    /// Result data
    pub data: String<256>,
    /// Success flag
    pub success: bool,
    /// Error message if failed
    pub error: Option<String<64>>,
}

/// Message type for BLE communication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageType {
    /// LLM request
    LlmRequest = 0,
    /// LLM response
    LlmResponse = 1,
    /// Tool call
    ToolCall = 2,
    /// Tool result
    ToolResult = 3,
    /// Heartbeat
    Heartbeat = 4,
    /// Error
    Error = 5,
}

/// BLE message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BleMessage {
    /// Message type
    pub message_type: MessageType,
    /// Message ID
    pub message_id: u32,
    /// Payload
    pub payload: String<MAX_MESSAGE_SIZE>,
}

impl BleMessage {
    /// Create a new message
    pub fn new(message_type: MessageType, message_id: u32, payload: &str) -> Result<Self> {
        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(AgentError::InputValidationFailed {
                field: "payload",
                reason: crate::error::ValidationError::TooLong,
            });
        }

        Ok(Self {
            message_type,
            message_id,
            payload: heapless::String::try_from(payload)
                .unwrap_or_else(|_| heapless::String::new()),
        })
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8, 512>> {
        // PATCHED (MicroAgent): see `AgentConfig::to_bytes` for
        // the heapless 0.7→0.9 boundary rationale.
        let buf =
            postcard::to_vec::<Self, 512>(self).map_err(|_| AgentError::ConfigurationError {
                field: "serialization",
                reason: crate::error::ConfigError::TypeMismatch,
            })?;
        let mut out = Vec::<u8, 512>::new();
        let take = buf.len().min(512);
        for &b in &buf.as_slice()[..take] {
            let _ = out.push(b);
        }
        Ok(out)
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|_| AgentError::ConfigurationError {
            field: "deserialization",
            reason: crate::error::ConfigError::TypeMismatch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn client_new_and_with_defaults() {
        let c = BleClient::new(5000);
        assert_eq!(c.timeout_ms, 5000);
        assert!(!c.is_connected());
        assert_eq!(BleClient::with_defaults().timeout_ms, 30000);
    }

    #[test]
    fn connect_and_disconnect_flip_state() {
        let mut c = BleClient::new(1000);
        assert!(!c.is_connected());
        assert!(block_on(c.connect()).is_ok());
        assert!(c.is_connected());
        assert!(block_on(c.disconnect()).is_ok());
        assert!(!c.is_connected());
    }

    #[test]
    fn send_request_fails_when_disconnected() {
        let c = BleClient::new(1000);
        let err = block_on(c.send_request("temperature")).unwrap_err();
        assert!(matches!(
            err,
            AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused
            }
        ));
    }

    #[test]
    fn send_request_returns_keyword_responses() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        assert_eq!(
            block_on(c.send_request("what is temperature"))
                .unwrap()
                .as_str(),
            "The current temperature is 25.5°C"
        );
        assert_eq!(
            block_on(c.send_request("turn LED on")).unwrap().as_str(),
            "LED has been turned on"
        );
        assert_eq!(
            block_on(c.send_request("read flash")).unwrap().as_str(),
            "Configuration read from flash successfully"
        );
        assert_eq!(
            block_on(c.send_request("hello")).unwrap().as_str(),
            "Task completed successfully"
        );
    }

    #[test]
    fn send_request_rejects_overlong_prompt() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        let long = "x".repeat(MAX_MESSAGE_SIZE + 1);
        let err = block_on(c.send_request(&long)).unwrap_err();
        assert!(matches!(
            err,
            AgentError::InputValidationFailed {
                field: "prompt",
                reason: crate::error::ValidationError::TooLong
            }
        ));
    }

    #[test]
    fn send_tool_result_requires_connection() {
        let result = ToolResult {
            tool_name: heapless::String::try_from("noop").unwrap(),
            data: heapless::String::try_from("ok").unwrap(),
            success: true,
            error: None,
        };
        let c = BleClient::new(1000);
        assert!(block_on(c.send_tool_result(&result)).is_err());

        let mut c2 = BleClient::new(1000);
        assert!(block_on(c2.connect()).is_ok());
        assert!(block_on(c2.send_tool_result(&result)).is_ok());
    }

    #[test]
    fn receive_response_returns_llm_response() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        let resp = block_on(c.receive_response()).unwrap();
        assert_eq!(resp.content.as_str(), "Response content");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.finish_reason.as_str(), "stop");
    }

    #[test]
    fn ble_message_new_validates_payload_length() {
        assert!(BleMessage::new(MessageType::LlmRequest, 1, "hi").is_ok());
        let long = "y".repeat(MAX_MESSAGE_SIZE + 1);
        let err = BleMessage::new(MessageType::LlmRequest, 1, &long).unwrap_err();
        assert!(matches!(
            err,
            AgentError::InputValidationFailed {
                field: "payload",
                ..
            }
        ));
    }

    #[test]
    fn ble_message_bytes_round_trip() {
        let msg = BleMessage::new(MessageType::ToolResult, 42, "payload").unwrap();
        let bytes = msg.to_bytes().unwrap();
        let back = BleMessage::from_bytes(&bytes).unwrap();
        assert_eq!(back.message_type, MessageType::ToolResult);
        assert_eq!(back.message_id, 42);
        assert_eq!(back.payload.as_str(), "payload");
    }

    #[test]
    fn ble_message_from_bytes_rejects_garbage() {
        assert!(BleMessage::from_bytes(&[0xff, 0x00, 0x01]).is_err());
    }

    #[test]
    fn message_type_is_stable() {
        assert_eq!(MessageType::LlmRequest as u8, 0);
        assert_eq!(MessageType::Error as u8, 5);
    }
}
