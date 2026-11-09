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

    /// Append `s` to `out`, JSON-escaping `"`, `\`, control characters.
    ///
    /// The serializer used to concatenate content verbatim, so any message
    /// containing a quote (extremely common in model replies) or a newline
    /// produced invalid JSON and the request was rejected by the backend.
    fn push_json_escaped(out: &mut heapless::String<2048>, s: &str) {
        use core::fmt::Write as _;
        for c in s.chars() {
            match c {
                '"' => {
                    let _ = out.push_str("\\\"");
                }
                '\\' => {
                    let _ = out.push_str("\\\\");
                }
                '\n' => {
                    let _ = out.push_str("\\n");
                }
                '\r' => {
                    let _ = out.push_str("\\r");
                }
                '\t' => {
                    let _ = out.push_str("\\t");
                }
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => {
                    let _ = out.push(c);
                }
            }
        }
    }

    /// Serialize request to JSON (simplified for embedded).
    ///
    /// Emits `tools` when present — `add_tools` populated the field but the
    /// serializer ignored it, so a caller that supplied tools sent none and
    /// the model never used function calling.
    pub fn serialize_request(&self, request: &OllamaRequest) -> heapless::String<2048> {
        let mut json = heapless::String::new();

        // Model and stream
        let _ = json.push_str(r#"{"model":""#);
        Self::push_json_escaped(&mut json, &request.model);
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
            Self::push_json_escaped(&mut json, &msg.role);
            let _ = json.push_str(r#"","content":""#);
            Self::push_json_escaped(&mut json, &msg.content);
            let _ = json.push_str(r#""}"#);
        }
        let _ = json.push(']');

        if let Some(tools) = &request.tools {
            let _ = json.push_str(r#","tools":["#);
            for (i, tool) in tools.iter().enumerate() {
                if i > 0 {
                    let _ = json.push(',');
                }
                let _ = json.push_str(r#"{"type":""#);
                Self::push_json_escaped(&mut json, &tool.tool_type);
                let _ = json.push_str(r#"","function":{"name":""#);
                Self::push_json_escaped(&mut json, &tool.function.name);
                let _ = json.push_str(r#"","description":""#);
                Self::push_json_escaped(&mut json, &tool.function.description);
                let params = &tool.function.parameters;
                let _ = json.push_str(r#"","parameters":{"type":""#);
                Self::push_json_escaped(&mut json, &params.schema_type);
                let _ = json.push_str(r#"","properties":{"#);
                for (j, prop) in params.properties.iter().enumerate() {
                    if j > 0 {
                        let _ = json.push(',');
                    }
                    let _ = json.push('"');
                    Self::push_json_escaped(&mut json, &prop.name);
                    let _ = json.push_str(r#"":{"type":""#);
                    Self::push_json_escaped(&mut json, &prop.param_type);
                    let _ = json.push_str(r#"","description":""#);
                    Self::push_json_escaped(&mut json, &prop.description);
                    let _ = json.push_str(r#""}"#);
                }
                let _ = json.push_str(r#"},"required":["#);
                for (j, req) in params.required.iter().enumerate() {
                    if j > 0 {
                        let _ = json.push(',');
                    }
                    let _ = json.push('"');
                    Self::push_json_escaped(&mut json, req);
                    let _ = json.push('"');
                }
                let _ = json.push_str("]}}}");
            }
            let _ = json.push(']');
        }

        let _ = json.push('}');

        json
    }

    /// Parse JSON response (simplified for embedded)
    pub fn parse_response(&self, json: &str) -> Result<OllamaResponse> {
        // Simplified JSON parsing - in production, use a proper JSON parser
        // Look for "content" field
        let content_start = match json.find("\"content\":\"") {
            Some(pos) => pos + 11,
            None => {
                return Err(AgentError::ConfigurationError {
                    field: "response",
                    reason: crate::error::ConfigError::TypeMismatch,
                });
            }
        };

        let content_end = match json[content_start..].find("\",\"") {
            Some(pos) => content_start + pos,
            None => {
                // No closing `","` — treat the content as running to the end
                // of the response minus the trailing `"}`. `json.len() - 2` is
                // a raw *byte* offset: a reply that ends in a multi-byte
                // character (any non-English answer) puts it inside a code
                // point, and the slice below then panics with "byte index N is
                // not a char boundary". Clamp back to a boundary.
                let mut end = json.len().saturating_sub(2);
                while end > content_start && !json.is_char_boundary(end) {
                    end -= 1;
                }
                end
            }
        };

        let content = &json[content_start..content_end];

        // Extract tool_calls if present
        let mut tool_calls: heapless::Vec<ToolCallSpec, 4> = heapless::Vec::new();

        if json.contains("\"tool_calls\"") {
            // Parse tool calls (simplified)
            let mut search_pos = 0;
            while let Some(func_start) = json[search_pos..].find("\"function\":{\"name\":\"") {
                let actual_pos = search_pos + func_start + 20;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_with_defaults_expose_fields() {
        let c = OllamaClient::new("http://localhost:11434/", "qwen2.5:3b", 5000).unwrap();
        assert_eq!(c.base_url(), "http://localhost:11434/");
        assert_eq!(c.model(), "qwen2.5:3b");
        assert_eq!(c.timeout_ms(), 5000);

        let d = OllamaClient::with_defaults().unwrap();
        assert_eq!(d.base_url(), "http://localhost:11434");
        assert_eq!(d.model(), "llama3.2");
        assert_eq!(d.timeout_ms(), 30000);
    }

    #[test]
    fn build_request_starts_empty() {
        let c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let req = c.build_request();
        assert_eq!(req.model.as_str(), "m");
        assert!(req.messages.is_empty());
        assert!(!req.stream);
        assert!(req.tools.is_none());
    }

    #[test]
    fn add_message_and_system_message() {
        let mut c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let mut req = c.build_request();
        c.add_message(&mut req, "user", "hello").unwrap();
        c.add_system_message(&mut req, "be brief").unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role.as_str(), "user");
        assert_eq!(req.messages[0].content.as_str(), "hello");
        assert_eq!(req.messages[1].role.as_str(), "system");
        assert_eq!(req.messages[1].content.as_str(), "be brief");
    }

    #[test]
    fn add_message_overflows_at_capacity() {
        let mut c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let mut req = c.build_request();
        for _ in 0..8 {
            assert!(c.add_message(&mut req, "user", "x").is_ok());
        }
        let err = c.add_message(&mut req, "user", "x").unwrap_err();
        assert!(matches!(
            err,
            AgentError::BufferOverflow { capacity: 8, .. }
        ));
    }

    #[test]
    fn add_tools_builds_definitions() {
        let mut c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let mut req = c.build_request();
        c.add_tools(&mut req, &TOOL_DEFINITIONS).unwrap();
        let tools = req.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 5);
        let first = &tools[0];
        assert_eq!(first.tool_type.as_str(), "function");
        assert_eq!(first.function.name.as_str(), "read_sensor");
        assert_eq!(first.function.description.as_str(), "Read sensor data");
        assert_eq!(first.function.parameters.schema_type.as_str(), "object");
        assert_eq!(first.function.parameters.properties.len(), 1);
        assert_eq!(first.function.parameters.required[0].as_str(), "sensor");
    }

    #[test]
    fn add_tools_overflow_errors() {
        let mut c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let mut req = c.build_request();
        // Repeating one tool def 9× overflows the per-request cap of 8.
        let one: (&str, &str, &[(&str, &str, &str)]) = ("t", "d", &[]);
        let many = [one; 9];
        assert!(c.add_tools(&mut req, &many).is_err());
    }

    #[test]
    fn serialize_request_emits_expected_shape() {
        let mut c = OllamaClient::new("http://h", "qwen", 1000).unwrap();
        let mut req = c.build_request();
        c.add_message(&mut req, "user", "hi").unwrap();
        let json = c.serialize_request(&req);
        let s = json.as_str();
        assert!(s.contains("\"model\":\"qwen\""));
        assert!(s.contains("\"stream\":false"));
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"content\":\"hi\""));
        assert!(s.ends_with("]}"));
    }

    /// Quotes/newlines in message content must be escaped; the old
    /// concatenation produced invalid JSON for any model reply containing
    /// them.
    #[test]
    fn serialize_request_escapes_message_content() {
        let mut c = OllamaClient::new("http://h", "qwen", 1000).unwrap();
        let mut req = c.build_request();
        c.add_message(&mut req, "user", "say \"hi\"\nplease\\now")
            .unwrap();
        let json = c.serialize_request(&req);
        // The escaped form is valid JSON.
        let parsed: serde_json::Value = serde_json::from_str(json.as_str()).unwrap();
        assert_eq!(parsed["messages"][0]["content"], "say \"hi\"\nplease\\now");
    }

    /// `add_tools` populated `request.tools`, but the serializer dropped it.
    #[test]
    fn serialize_request_emits_tool_definitions() {
        let mut c = OllamaClient::new("http://h", "qwen", 1000).unwrap();
        let mut req = c.build_request();
        let tools: (&str, &str, &[(&str, &str, &str)]) = (
            "read_sensor",
            "Read a sensor",
            &[("sensor", "string", "sensor name")],
        );
        c.add_tools(&mut req, &[tools]).unwrap();
        let json = c.serialize_request(&req);
        let parsed: serde_json::Value = serde_json::from_str(json.as_str()).unwrap();
        let tool = &parsed["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "read_sensor");
        assert_eq!(
            tool["function"]["parameters"]["properties"]["sensor"]["type"],
            "string"
        );
        assert_eq!(tool["function"]["parameters"]["required"][0], "sensor");
    }

    #[test]
    fn parse_response_extracts_content() {
        let c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let resp = c
            .parse_response(r#"{"content":"hello there","done":true}"#)
            .unwrap();
        assert_eq!(resp.message.content.as_str(), "hello there");
        assert_eq!(resp.message.role.as_str(), "assistant");
        assert!(resp.done);
        assert!(resp.message.tool_calls.is_none());
    }

    #[test]
    fn parse_response_missing_content_is_error() {
        let c = OllamaClient::new("http://h", "m", 1000).unwrap();
        assert!(c.parse_response(r#"{"error":"boom"}"#).is_err());
    }

    /// A reply whose content is the last field and ends in multi-byte text
    /// used to panic: the `json.len() - 2` fallback lands inside a code point,
    /// and slicing there aborts with "not a char boundary".
    #[test]
    fn parse_response_with_trailing_multibyte_content_does_not_panic() {
        let c = OllamaClient::new("http://h", "m", 1000).unwrap();
        // No closing `","` after the content, so the fallback path is taken.
        let resp = c.parse_response(r#"{"content":"温度"#).unwrap();
        assert!(resp.message.content.starts_with("温"));
    }

    #[test]
    fn parse_response_extracts_tool_calls() {
        let c = OllamaClient::new("http://h", "m", 1000).unwrap();
        let resp = c
            .parse_response(
                r#"{"content":"","tool_calls":[{"function":{"name":"read_sensor","arguments":{}}},{"function":{"name":"write_gpio"}}],"done":false}"#,
            )
            .unwrap();
        let calls = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name.as_str(), "read_sensor");
        assert_eq!(calls[1].name.as_str(), "write_gpio");
        assert!(!resp.done);
    }

    #[test]
    fn tool_definitions_constant_is_stable() {
        let defs = get_tool_definitions();
        assert_eq!(defs.len(), 5);
        assert_eq!(defs[0].0, "read_sensor");
        assert_eq!(defs[4].0, "ble_send");
    }

    #[test]
    fn system_prompt_mentions_agent_identity() {
        assert!(SYSTEM_PROMPT.contains("mAgent"));
        assert!(SYSTEM_PROMPT.contains("read_sensor"));
    }
}
