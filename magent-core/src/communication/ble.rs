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

/// Maximum number of frames buffered in each direction before the client
/// reports `BufferOverflow`. Small and fixed on purpose (aerospace-grade
/// bounds): the BLE driver drains `outbound` long before it fills.
const BLE_QUEUE_DEPTH: usize = 4;

/// BLE client for cloud communication.
///
/// ## Transport contract
///
/// This client contains **no radio and no fabricated responses**. A BLE
/// gateway integration drives it from the outside:
///
/// 1. The agent calls [`BleClient::send_request`] /
///    [`BleClient::send_tool_result`], which serialise a [`BleMessage`] onto
///    the `outbound` queue.
/// 2. The driver drains those frames with [`BleClient::take_outbound`] and
///    writes them to the radio.
/// 3. When the gateway replies, the driver decodes the frame and hands it
///    back with [`BleClient::enqueue_response`].
/// 4. [`BleClient::send_request`] returns that reply — or, if nothing
///    arrived, a real [`NetworkError::Timeout`]. It never invents content.
///
/// TRACE: REQ-NET-002 — a missing wire is reported as an error, never as a
/// synthetic success.
pub struct BleClient {
    connected: bool,
    /// Connection timeout, in milliseconds. Retained for callers that
    /// wrap their own wait around [`BleClient::send_request`].
    pub timeout_ms: u32,
    /// Frames queued for the radio driver to transmit (FIFO).
    outbound: Vec<BleMessage, BLE_QUEUE_DEPTH>,
    /// LLM responses delivered by the gateway, awaiting the agent loop.
    inbound: Vec<LlmResponse, BLE_QUEUE_DEPTH>,
    /// Monotonic message-id source for request/response correlation.
    next_message_id: u32,
}

impl BleClient {
    /// Create a new BLE client
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            connected: false,
            timeout_ms,
            outbound: Vec::new(),
            inbound: Vec::new(),
            next_message_id: 0,
        }
    }

    /// Create with default timeout
    pub fn with_defaults() -> Self {
        Self::new(30000) // 30 seconds
    }

    /// Connect to gateway.
    ///
    /// The link itself is established by the platform BLE driver; this only
    /// arms the client's request/response state machine.
    pub async fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    /// Disconnect from gateway, discarding any queued frames so a later
    /// reconnect can't deliver stale replies.
    pub async fn disconnect(&mut self) -> Result<()> {
        self.connected = false;
        self.outbound.clear();
        self.inbound.clear();
        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Queue an LLM request for the gateway and await its reply.
    ///
    /// The prompt is handed to the radio driver via the `outbound` queue;
    /// the return value comes from a frame the driver delivered through
    /// [`BleClient::enqueue_response`]. If nothing arrives, this returns
    /// [`NetworkError::Timeout`] — it never fabricates an answer.
    pub async fn send_request(&mut self, prompt: &str) -> Result<String<MAX_MESSAGE_SIZE>> {
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

        let request = BleMessage::new(MessageType::LlmRequest, self.next_id(), prompt)?;
        self.outbound
            .push(request)
            .map_err(|_| AgentError::BufferOverflow {
                capacity: BLE_QUEUE_DEPTH,
                attempted: BLE_QUEUE_DEPTH + 1,
            })?;

        let response = self.receive_response().await?;
        Ok(response.content)
    }

    /// Queue a tool result for transmission to the cloud gateway.
    pub async fn send_tool_result(&mut self, result: &ToolResult) -> Result<()> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused,
            });
        }

        // JSON is the on-wire payload the gateway expects; the bounded
        // `BleMessage` constructor rejects anything over `MAX_MESSAGE_SIZE`.
        let payload =
            serde_json::to_string(result).map_err(|_| AgentError::InputValidationFailed {
                field: "tool_result",
                reason: crate::error::ValidationError::InvalidFormat,
            })?;
        let message = BleMessage::new(MessageType::ToolResult, self.next_id(), &payload)?;
        self.outbound
            .push(message)
            .map_err(|_| AgentError::BufferOverflow {
                capacity: BLE_QUEUE_DEPTH,
                attempted: BLE_QUEUE_DEPTH + 1,
            })?;
        Ok(())
    }

    /// Receive the next response delivered by the gateway.
    ///
    /// Returns [`NetworkError::Timeout`] when the driver has not queued a
    /// frame yet; a missing wire must never look like a successful answer.
    pub async fn receive_response(&mut self) -> Result<LlmResponse> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused,
            });
        }

        if self.inbound.is_empty() {
            return Err(AgentError::NetworkConnectionFailed {
                reason: NetworkError::Timeout,
            });
        }

        Ok(self.inbound.remove(0))
    }

    /// Driver hook: queue an LLM response decoded from a gateway frame.
    pub fn enqueue_response(&mut self, response: LlmResponse) -> Result<()> {
        self.inbound
            .push(response)
            .map_err(|_| AgentError::BufferOverflow {
                capacity: BLE_QUEUE_DEPTH,
                attempted: BLE_QUEUE_DEPTH + 1,
            })
    }

    /// Driver hook: take the next frame the radio should transmit (FIFO).
    pub fn take_outbound(&mut self) -> Option<BleMessage> {
        if self.outbound.is_empty() {
            None
        } else {
            Some(self.outbound.remove(0))
        }
    }

    /// Number of frames awaiting transmission.
    pub fn pending_outbound(&self) -> usize {
        self.outbound.len()
    }

    /// Next monotonic message id (wraps after 2^32 frames).
    fn next_id(&mut self) -> u32 {
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        id
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
        let mut c = BleClient::new(1000);
        let err = block_on(c.send_request("temperature")).unwrap_err();
        assert!(matches!(
            err,
            AgentError::NetworkConnectionFailed {
                reason: NetworkError::ConnectionRefused
            }
        ));
    }

    #[test]
    fn send_request_requires_connection() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.send_request("temperature")).is_err());
    }

    /// The reply is whatever the driver queued — the client must not
    /// fabricate one. Drive the full round-trip: request goes out on the
    /// wire queue, the (simulated) radio answers, the client returns it.
    #[test]
    fn send_request_returns_driver_supplied_response() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        assert!(block_on(c.receive_response()).is_err());

        assert!(c
            .enqueue_response(LlmResponse {
                content: heapless::String::try_from("gateway says hi").unwrap(),
                tool_calls: Vec::new(),
                finish_reason: heapless::String::try_from("stop").unwrap(),
            })
            .is_ok());

        let reply = block_on(c.send_request("hello")).unwrap();
        assert_eq!(reply.as_str(), "gateway says hi");
    }

    /// A request is serialised onto the outbound queue for the radio driver.
    #[test]
    fn send_request_queues_outbound_frame() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        assert_eq!(c.pending_outbound(), 0);

        // No reply queued → the call times out, but the request must still
        // have been handed to the driver.
        let err = block_on(c.send_request("what is the temperature")).unwrap_err();
        assert!(matches!(
            err,
            AgentError::NetworkConnectionFailed {
                reason: NetworkError::Timeout
            }
        ));
        assert_eq!(c.pending_outbound(), 1);

        let frame = c.take_outbound().expect("frame queued for the radio");
        assert_eq!(frame.message_type, MessageType::LlmRequest);
        assert_eq!(frame.payload.as_str(), "what is the temperature");
        assert!(c.take_outbound().is_none());
    }

    /// An empty inbound queue is a timeout, never a synthetic answer
    /// (REQ-NET-002).
    #[test]
    fn receive_response_is_empty_without_driver_frame() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        let err = block_on(c.receive_response()).unwrap_err();
        assert!(matches!(
            err,
            AgentError::NetworkConnectionFailed {
                reason: NetworkError::Timeout
            }
        ));
    }

    #[test]
    fn disconnect_discards_queued_frames() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        let _ = block_on(c.send_request("hello"));
        assert_eq!(c.pending_outbound(), 1);

        assert!(block_on(c.disconnect()).is_ok());
        assert_eq!(c.pending_outbound(), 0);
        assert!(c.take_outbound().is_none());
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
        let mut c = BleClient::new(1000);
        assert!(block_on(c.send_tool_result(&result)).is_err());

        let mut c2 = BleClient::new(1000);
        assert!(block_on(c2.connect()).is_ok());
        assert!(block_on(c2.send_tool_result(&result)).is_ok());
        // The tool result is serialised into an outbound frame.
        let frame = c2.take_outbound().expect("tool result queued");
        assert_eq!(frame.message_type, MessageType::ToolResult);
        assert!(frame.payload.as_str().contains("noop"));
    }

    #[test]
    fn enqueue_response_round_trip() {
        let mut c = BleClient::new(1000);
        assert!(block_on(c.connect()).is_ok());
        assert!(c
            .enqueue_response(LlmResponse {
                content: heapless::String::try_from("Response content").unwrap(),
                tool_calls: Vec::new(),
                finish_reason: heapless::String::try_from("stop").unwrap(),
            })
            .is_ok());
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
