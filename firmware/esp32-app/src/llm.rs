//! DeepSeek chat-LLM backend for the ESP32 firmware.
//!
//! Implements [`magent_core::agent::LlmBackend`] so the agent's `think`
//! phase can call DeepSeek's OpenAI-compatible `/chat/completions` endpoint
//! to reason about a task and decide a tool call (or give a final answer).
//! The model + API key come from `AT+LLMCFG` (stored in NVS).

use std::sync::mpsc;
use std::time::Duration;

use embedded_svc::http::client::Client as HttpClient;
use embedded_svc::http::Method;
use embedded_svc::io::Write as _;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
use magent_core::agent::LlmBackend;
use magent_core::error::AgentError;
use magent_core::escape::json as escape_json;

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
        // REQ-SCHED-001 / mem-3 (heap-blast guard): parsing the raw JSON into a
        // `serde_json::Value` transiently allocates several times the payload
        // (strings are copied), and on the S3 this runs on the shared 8 MB
        // PSRAM pool shared with the conversation cache / workers. If free heap
        // has fallen below the floor, refuse to proceed so a low-memory
        // condition surfaces as a clean agent error instead of an OOM abort.
        const HEAP_FLOOR: u32 = 64 * 1024;
        let free = crate::free_heap();
        if free < HEAP_FLOOR {
            return Err(AgentError::MemoryAllocationFailed {
                requested: buf.len() as usize,
                available: free as usize,
            });
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

// ---------------------------------------------------------------------------
// Cross-core LLM pipeline (REQ-SCHED-001 / P1)
// ---------------------------------------------------------------------------
// The blocking DeepSeek TLS/HTTP call must NOT run on the real-time agent
// thread (Core 1): an 8s HTTPS round-trip there would starve the higher-
// priority ingress / sensor tasks on the same core. Instead the agent
// submits a request over an mpsc channel to a dedicated worker pinned to
// Core 0, which owns the real `Esp32DeepSeekBackend` and does the heavy
// TLS/JSON work. The agent blocks on a one-shot reply channel — a condvar/
// queue wait, which *yields* Core 1's CPU while it waits.

/// A request queued from the agent thread (Core 1) to the LLM network
/// worker (Core 0). Each request carries its own one-shot reply channel so
/// the caller blocks on *its* response and concurrent callers never mix.
pub enum LlmRequest {
    /// Run a DeepSeek chat-completions call. `reply` receives the backend's
    /// `Result` (or an `AgentError` the caller surfaces to the ReAct loop).
    Complete {
        system: String,
        user: String,
        reply: mpsc::Sender<Result<String, AgentError>>,
    },
}

/// An [`LlmBackend`] that does NOT run the blocking DeepSeek call on the
/// calling thread. It forwards the request over an mpsc channel to a worker
/// pinned to Core 0 and blocks on the reply.
///
/// The blocking `recv_timeout` is a condvar/queue wait, so it yields the
/// calling core's CPU — the agent thread (Core 1) keeps Core 1 free for the
/// real-time tasks while the TLS/JSON runs on Core 0.
pub struct ChannelLlmBackend {
    tx: mpsc::Sender<LlmRequest>,
}

impl ChannelLlmBackend {
    /// Wrap a sender to the LLM worker.
    pub fn new(tx: mpsc::Sender<LlmRequest>) -> Self {
        Self { tx }
    }
}

impl LlmBackend for ChannelLlmBackend {
    fn complete(&mut self, system: &str, user: &str) -> core::result::Result<String, AgentError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let t0 = crate::latency_metrics::now_us();
        self.tx
            .send(LlmRequest::Complete {
                system: system.to_string(),
                user: user.to_string(),
                reply: reply_tx,
            })
            .map_err(|e| llm_err(e))?;
        // Block for the worker's answer. A condvar wait yields the CPU, so
        // the agent's wait does not spin Core 1. We poll in 1s slices and
        // re-feed the RT watchdog between polls, so a long multi-iteration
        // ReAct task (several back-to-back LLM calls) never looks like an
        // agent hang to the P3 watchdog. Bounded to 10s total; the backend
        // itself already has an 8s TLS timeout.
        let deadline = crate::latency_metrics::now_us() + 10_000_000;
        let out = loop {
            match reply_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(res) => break res,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    crate::rt_watchdog::feed();
                    if crate::latency_metrics::now_us() >= deadline {
                        break Err(AgentError::NetworkTimeout {
                            operation: "deepseek",
                            duration_ms: 10_000,
                        });
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(AgentError::NetworkTimeout {
                        operation: "deepseek",
                        duration_ms: 10_000,
                    });
                }
            }
        };
        // P3: record the cross-core LLM round-trip (the dominant, variable
        // latency) as a WCET observation.
        crate::latency_metrics::llm_rt()
            .record(crate::latency_metrics::now_us().wrapping_sub(t0));
        out
    }
}

/// LLM worker loop — runs pinned to Core 0 (I/O domain). Owns the real
/// [`Esp32DeepSeekBackend`] and services requests from the agent thread.
/// Exits when the channel closes (all senders dropped).
pub fn run_llm_worker(rx: mpsc::Receiver<LlmRequest>, mut backend: Esp32DeepSeekBackend) {
    log::info!("[llm-worker] started on Core 0");
    while let Ok(req) = rx.recv() {
        match req {
            LlmRequest::Complete { system, user, reply } => {
                let res = backend.complete(&system, &user);
                // Ignore a closed reply channel (caller gave up / timed out).
                let _ = reply.send(res);
            }
        }
    }
    log::info!("[llm-worker] channel closed — exiting");
}

