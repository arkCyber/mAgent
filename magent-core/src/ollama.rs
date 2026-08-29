//! Ollama LLM client for mAgent
//!
//! Provides integration with Ollama local LLM API for embedded AI agent.
//! Supports both std (testing) and no_std (embedded) environments.

use crate::error::{try_heapless, AgentError, Result};
use crate::MAX_BUFFER_SIZE;

/// Maximum number of tools in a response
#[allow(dead_code)]
const MAX_TOOLS_IN_RESPONSE: usize = 4;

/// Ollama API endpoint
const OLLAMA_API_HOST: &str = "http://localhost:11434";

/// Ollama request for chat completion
#[derive(Debug, Clone)]
pub struct OllamaRequest {
    /// Model name
    pub model: heapless::String<32>,
    /// Messages
    pub messages: heapless::Vec<OllamaMessage, 8>,
    /// Stream flag
    pub stream: bool,
    /// Tools for function calling
    pub tools: Option<heapless::Vec<ToolDefinition, 8>>,
}

/// Ollama message
#[derive(Debug, Clone)]
pub struct OllamaMessage {
    /// Role: system, user, assistant, tool
    pub role: heapless::String<16>,
    /// Content
    pub content: heapless::String<MAX_BUFFER_SIZE>,
    /// Tool calls (for assistant messages)
    pub tool_calls: Option<heapless::Vec<ToolCallSpec, 4>>,
}

/// Tool definition for Ollama
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool type
    pub tool_type: heapless::String<16>,
    /// Function definition
    pub function: FunctionDefinition,
}

/// Function definition
#[derive(Debug, Clone)]
pub struct FunctionDefinition {
    /// Function name
    pub name: heapless::String<32>,
    /// Function description
    pub description: heapless::String<128>,
    /// Parameters schema
    pub parameters: ParametersSchema,
}

/// Parameters schema
#[derive(Debug, Clone)]
pub struct ParametersSchema {
    /// Schema type
    pub schema_type: heapless::String<16>,
    /// Required parameters
    pub required: heapless::Vec<heapless::String<16>, 4>,
    /// Properties
    pub properties: heapless::Vec<ParameterProperty, 4>,
}

/// Parameter property
#[derive(Debug, Clone)]
pub struct ParameterProperty {
    /// Parameter name
    pub name: heapless::String<16>,
    /// Parameter type
    pub param_type: heapless::String<16>,
    /// Parameter description
    pub description: heapless::String<64>,
}

/// Tool call specification
#[derive(Debug, Clone)]
pub struct ToolCallSpec {
    /// Function name
    pub name: heapless::String<32>,
    /// Arguments (JSON string)
    pub arguments: heapless::String<256>,
}

/// Ollama response
#[derive(Debug, Clone)]
pub struct OllamaResponse {
    /// Response message
    pub message: OllamaMessage,
    /// Done flag
    pub done: bool,
    /// Total duration in nanoseconds
    pub total_duration: u64,
}

/// Ollama client for embedded systems
pub struct OllamaClient {
    /// Base URL for API
    base_url: heapless::String<64>,
    /// Model name
    model: heapless::String<32>,
    /// Request timeout in milliseconds
    timeout_ms: u32,
}

impl OllamaClient {
    /// Create a new Ollama client
    pub fn new(base_url: &str, model: &str, timeout_ms: u32) -> Result<Self> {
        Ok(Self {
            base_url: try_heapless::<64>(base_url),
            model: try_heapless::<32>(model),
            timeout_ms,
        })
    }

    /// Create with default settings (localhost:11434, llama3.2)
    pub fn with_defaults() -> Result<Self> {
        Self::new(OLLAMA_API_HOST, "llama3.2", 30000)
    }

    /// Build a chat request
    pub fn build_request(&self) -> OllamaRequest {
        OllamaRequest {
            model: self.model.clone(),
            messages: heapless::Vec::new(),
            stream: false,
            tools: None,
        }
    }

    /// Add a message to the request
    pub fn add_message(
        &mut self,
        request: &mut OllamaRequest,
        role: &str,
        content: &str,
    ) -> Result<()> {
        let msg = OllamaMessage {
            role: try_heapless::<16>(role),
            content: try_heapless::<2048>(content),
            tool_calls: None,
        };

        request
            .messages
            .push(msg)
            .map_err(|_| AgentError::BufferOverflow {
                capacity: 8,
                attempted: request.messages.len() + 1,
            })?;

        Ok(())
    }

    /// Add system message
    pub fn add_system_message(&mut self, request: &mut OllamaRequest, content: &str) -> Result<()> {
        self.add_message(request, "system", content)
    }

    /// Add tool definitions
    ///
    /// HARDENING (audit-2026-08 H7): every `heapless::String::try_from(..).unwrap()`
    /// on a caller-provided `&str` was replaced with `try_heapless` — a
    /// wrapper that truncates at a UTF-8 boundary instead of panicking
    /// when the input exceeds the buffer. A long tool name or parameter
    /// description (e.g. a typo with thousands of bytes) now yields a
    /// silent truncation rather than a worker-thread panic.
    // TRACE: the `tools` tuple is intentionally a `(&str, &str, &[..])` nesting to
    // stay `no_std`/heapless; `type_complexity` is a readability lint, and the
    // alias would leak a private tuple name into this `pub` signature.
    #[allow(clippy::type_complexity)]
    pub fn add_tools(
        &mut self,
        request: &mut OllamaRequest,
        tools: &[(&str, &str, &[(&str, &str, &str)])],
    ) -> Result<()> {
        let mut tool_defs: heapless::Vec<ToolDefinition, 8> = heapless::Vec::new();

        for (name, description, params) in tools {
            let mut required: heapless::Vec<heapless::String<16>, 4> = heapless::Vec::new();
            let mut properties: heapless::Vec<ParameterProperty, 4> = heapless::Vec::new();

            for (param_name, param_type, param_desc) in *params {
                let _ = required.push(try_heapless::<16>(param_name));

                let prop = ParameterProperty {
                    name: try_heapless::<16>(param_name),
                    param_type: try_heapless::<16>(param_type),
                    description: try_heapless::<64>(param_desc),
                };
                let _ = properties.push(prop);
            }

            // HARDENING (audit-2026-08 unwrap sweep): replace compile-time
            // constant string `try_from(...).unwrap()` with `try_heapless`
            // so a future schema rename (e.g. "function" → "tool") can't
            // accidentally introduce a panic.
            let func_def = FunctionDefinition {
                name: try_heapless::<32>(name),
                description: try_heapless::<128>(description),
                parameters: ParametersSchema {
                    schema_type: try_heapless::<16>("object"),
                    required,
                    properties,
                },
            };

            let tool_def = ToolDefinition {
                tool_type: try_heapless::<16>("function"),
                function: func_def,
            };

            tool_defs
                .push(tool_def)
                .map_err(|_| AgentError::BufferOverflow {
                    capacity: 8,
                    attempted: tool_defs.len() + 1,
                })?;
        }

        request.tools = Some(tool_defs);
        Ok(())
    }

    /// Serialize request to JSON (simplified for embedded)
    pub fn serialize_request(&self, request: &OllamaRequest) -> heapless::String<2048> {
        let mut json = heapless::String::new();

        // Model and stream
        let _ = json.push_str(r#"{"model":""#);
        let _ = json.push_str(&request.model);
        let _ = json.push_str(r#"","stream":"#);
        if request.stream {
            let _ = json.push_str("true");
        } else {
            let _ = json.push_str("false");
        }
        let _ = json.push_str(r#","messages":["#);

        // Messages
        let mut first = true;
        for msg in &request.messages {
            if !first {
                let _ = json.push_str(",");
            }
            first = false;

            let _ = json.push_str(r#"{"role":""#);
            let _ = json.push_str(&msg.role);
            let _ = json.push_str(r#"","content":""#);
            let _ = json.push_str(&msg.content);
            let _ = json.push_str(r#""}"#);
        }

        let _ = json.push_str("]}");

        json
    }

    /// Parse JSON response (simplified for embedded)
    pub fn parse_response(&self, json: &str) -> Result<OllamaResponse> {
        // Simplified JSON parsing - in production, use a proper JSON parser
        // Look for "content" field
        let content_start = match json.find("\"content\":\"") {
            Some(pos) => pos + 10,
            None => {
                return Err(AgentError::ConfigurationError {
                    field: "response",
                    reason: crate::error::ConfigError::TypeMismatch,
                });
            }
        };

        let content_end = match json[content_start..].find("\",\"") {
            Some(pos) => content_start + pos,
            None => json.len() - 2,
        };

        let content = &json[content_start..content_end];

        // Extract tool_calls if present
        let mut tool_calls: heapless::Vec<ToolCallSpec, 4> = heapless::Vec::new();

        if json.contains("\"tool_calls\"") {
            // Parse tool calls (simplified)
            let mut search_pos = 0;
            while let Some(func_start) = json[search_pos..].find("\"function\":{\"name\":\"") {
                let actual_pos = search_pos + func_start + 17;
                if let Some(func_end) = json[actual_pos..].find('"') {
                    let func_name = &json[actual_pos..actual_pos + func_end];

                    let tool_call = ToolCallSpec {
                        // HARDENING (audit-2026-08 unwrap sweep):
                        // `func_name` is extracted from the LLM's
                        // JSON response, which the model can make
                        // arbitrarily long. The previous
                        // `String::try_from(func_name).unwrap()`
                        // would panic the agent on a model that
                        // produces a >32-byte function name.
                        // `try_heapless` truncates with a warning.
                        name: try_heapless::<32>(func_name),
                        arguments: try_heapless::<256>("{}"),
                    };
                    let _ = tool_calls.push(tool_call);
                }
                search_pos = actual_pos;
            }
        }

        let message = OllamaMessage {
            role: try_heapless::<16>("assistant"),
            // HARDENING (audit-2026-08 unwrap sweep): `content` is
            // the LLM's full response text, which models are free
            // to make arbitrarily long. `try_heapless` truncates
            // at the buffer's UTF-8 boundary instead of panicking.
            content: try_heapless::<2048>(content),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        };

        Ok(OllamaResponse {
            message,
            done: json.contains("\"done\":true"),
            total_duration: 0,
        })
    }

    /// Get base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get model name
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get timeout
    pub fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }
}

/// System prompt for the agent
pub const SYSTEM_PROMPT: &str = r#"You are mAgent, an aerospace-grade embedded AI agent running on nRF52840.

You have access to the following tools:
- read_sensor(sensor): Read sensor data (temperature, accelerometer, humidity, pressure)
- write_gpio(pin, state): Control GPIO pins (state: high/low)
- flash_read(address): Read data from flash storage
- flash_write(address, data): Write data to flash storage
- ble_send(data): Send data via BLE

You must:
1. Think step by step
2. Use tools when needed
3. Be concise and efficient (memory is limited)
4. Prioritize safety and reliability

Respond with a JSON object containing your response and any tool calls."#;

/// Tool definition tuple type
pub type ToolDef = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str, &'static str)],
);

/// Tool definitions for the agent
pub const TOOL_DEFINITIONS: [ToolDef; 5] = [
    (
        "read_sensor",
        "Read sensor data",
        &[(
            "sensor",
            "string",
            "Sensor type: temperature, accelerometer, humidity, pressure",
        )],
    ),
    (
        "write_gpio",
        "Control GPIO pin",
        &[
            ("pin", "integer", "GPIO pin number"),
            ("state", "string", "Pin state: high or low"),
        ],
    ),
    (
        "flash_read",
        "Read from flash storage",
        &[("address", "integer", "Flash address to read from")],
    ),
    (
        "flash_write",
        "Write to flash storage",
        &[
            ("address", "integer", "Flash address to write to"),
            ("data", "string", "Data to write"),
        ],
    ),
    (
        "ble_send",
        "Send data via BLE",
        &[("data", "string", "Data to send")],
    ),
];

/// Get tool definitions
pub const fn get_tool_definitions() -> [ToolDef; 5] {
    TOOL_DEFINITIONS
}
