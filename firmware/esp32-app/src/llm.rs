//! DeepSeek chat-LLM backend for the ESP32 firmware.
//!
//! Implements [`magent_core::agent::LlmBackend`] so the agent's `think`
//! phase can call DeepSeek's OpenAI-compatible `/chat/completions` endpoint
//! to reason about a task and decide a tool call (or give a final answer).
//! The model + API key come from `AT+LLMCFG` (stored in NVS).

use std::time::Duration;

use embedded_svc::http::client::Client as HttpClient;
use embedded_svc::http::Method;
use embedded_svc::io::{Read as _, Write as _};
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
use magent_core::agent::LlmBackend;
use magent_core::error::AgentError;

/// DeepSeek's public OpenAI-compatible endpoint.
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// A chat-LLM backend backed by DeepSeek. `model`/`api_key` are read from
/// NVS via `AT+LLMCFG` at construction.
pub struct Esp32DeepSeekBackend {
    model: String,
    api_key: String,
}

impl Esp32DeepSeekBackend {
    /// Build a backend for the given DeepSeek model + API key.
    pub fn new(model: &str, api_key: &str) -> Self {
        Self {
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }
}

impl LlmBackend for Esp32DeepSeekBackend {
    fn complete(&mut self, system: &str, user: &str) -> core::result::Result<String, AgentError> {
        // OpenAI / DeepSeek chat-completions body.
        let body = format!(
            "{{\"model\":\"{}\",\"stream\":false,\"temperature\":0.2,\"max_tokens\":256,\
             \"messages\":[{{\"role\":\"system\",\"content\":\"{}\"}},\
             {{\"role\":\"user\",\"content\":\"{}\"}}]}}",
            escape_json(&self.model),
            escape_json(system),
            escape_json(user),
        );
        let url = format!("{DEEPSEEK_BASE_URL}/chat/completions");

        let cfg = HttpConfig {
            // PATCHED (MicroAgent): keep this short — a hung HTTPS/TLS attempt
            // must not block the agent thread long enough to trip a watchdog.
            // (Note: TLS on this C61 hangs regardless — see sdkconfig.defaults
            // "Network / TLS" comment. A longer timeout here only stalls the
            // agent longer, so we keep it bounded.)
            timeout: Some(Duration::from_secs(8)),
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };
        let conn = EspHttpConnection::new(&cfg).map_err(|e| llm_err(e))?;
        let mut client = HttpClient::wrap(conn);
        let auth = format!("Bearer {}", self.api_key);
        let headers = [
            ("content-type", "application/json"),
            ("authorization", auth.as_str()),
        ];
        let mut request = client
            .request(Method::Post, &url, &headers)
            .map_err(|e| llm_err(e))?;
        request.write_all(body.as_bytes()).map_err(|e| llm_err(e))?;
        request.flush().map_err(|e| llm_err(e))?;
        let mut response = request.submit().map_err(|e| llm_err(e))?;

        let status = response.status();
        if status != 200 {
            return Err(AgentError::NetworkTimeout {
                operation: "deepseek",
                duration_ms: status as u32,
            });
        }

        // Read the (bounded) response body and extract the assistant text.
        let mut buf = [0u8; 8192];
        let mut read = 0usize;
        while read < buf.len() {
            match response.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(_) => break,
            }
        }
        let v: serde_json::Value = serde_json::from_slice(&buf[..read]).map_err(|e| llm_err(e))?;
        // HARDENING (audit-2026-08): a malformed LLM JSON response
        // (e.g. the model returned a refusal, a tool-call-only reply,
        // or an API error object) would previously fall through to an
        // empty string silently, confusing the agent's ReAct loop.
        // We propagate an explicit error so the agent can fall back
        // gracefully.
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| AgentError::NetworkTimeout {
                operation: "deepseek",
                duration_ms: 8000,
            })?
            .to_string();
        Ok(content)
    }
}

/// Normalise any backend failure into a compact `AgentError` (the reason is
/// logged by the agent's `think` fallback path).
fn llm_err(_e: impl std::fmt::Debug) -> AgentError {
    AgentError::NetworkTimeout {
        operation: "deepseek",
        duration_ms: 8000,
    }
}

/// Escape a string for safe embedding inside a JSON string literal.
fn escape_json(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}
