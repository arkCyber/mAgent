//! Agent Runner - Real ReAct Loop Implementation
//!
//! Implements the complete ReAct (Reasoning + Acting) loop with:
//! - Real LLM integration via Ollama
//! - Tool execution
//! - State machine
//! - Budget enforcement
//!
//! ## Performance notes (v2)
//!
//! v2 removes the dual `messages` / `conversation_text` representation: only
//! `messages` is the source of truth. The Ollama wire format is built lazily
//! from `messages` without intermediate `String` allocations for prefixes.
//! A single pre-sized JSON body is reused across requests where possible.
//!
//! Mirrors `runner_test/src/runner.rs` (the standalone test crate); both
//! files should stay byte-identical modulo std macro imports.

#![cfg(feature = "std")]

use std::boxed::Box;
use std::collections::HashMap;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

// ============================================================================
// Trace plumbing
// ============================================================================
//
// `RealAgentRunner::run()` historically called `println!` directly to
// emit its step-by-step trace. That coupled the ReAct loop to the
// process's stdout, which broke two things:
//
//   1. JSON-mode (`magent --json`) — bare `println!` mixed trace lines
//      into the stdout envelope and made `jq` blow up.
//   2. Embedded callers — they don't have stdout at all.
//
// `TraceSink` + `TraceEvent` are a small no_std-friendly shim that
// lets callers decide how each event is rendered (stderr trace line,
// JSON-line on stdout, dropped entirely, sent over BLE, …). The
// default `NullSink` matches the old behaviour 1:1 (no output) so the
// no_std build doesn't pay for a feature it can't use.

/// Structured event emitted by the runner at every observable step.
///
/// The variants are deliberately small — anything that the runner
/// already printed today becomes one of these. Callers that need
/// richer telemetry can wrap their sink in their own enrichment
/// layer without forcing a library release.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum TraceEvent {
    /// Run banner — emitted once at the start of `run()`.
    RunStart {
        /// Task description that was handed to the agent.
        task: String,
    },
    /// Backend availability reported. Either the real backend
    /// (Ollama/DeepSeek/...) or the simulated fallback.
    BackendReady {
        /// Provider name (e.g. `"ollama"`, `"deepseek"`, `"sim"`).
        provider: String,
        /// `true` if the runner is talking to a real LLM endpoint,
        /// `false` if it's using the deterministic simulator.
        using_real_llm: bool,
    },
    /// LLM-call budget exhausted.
    BudgetExhausted {
        /// Which budget fired (e.g. `"iterations"`, `"tool_calls"`).
        kind: &'static str,
        /// Cap that was hit.
        limit: usize,
    },
    /// Beginning of a `think()` invocation. Iteration / tool counts
    /// are at the *start* of the call (the runner increments
    /// `iteration` before calling `think()`).
    ThinkingStart {
        /// ReAct loop iteration index at the start of this `think()`.
        iteration: usize,
        /// Total tool calls dispatched so far.
        tool_calls: usize,
    },
    /// Compression ran and trimmed the conversation. `kept` /
    /// `dropped` / `truncated` are the same numbers the existing
    /// verbose path prints.
    CompressionApplied {
        /// Number of conversation turns kept after compression.
        kept: usize,
        /// Number of older turns dropped from history.
        dropped: usize,
        /// Number of tool-result blocks truncated in place.
        tool_results_truncated: usize,
        /// Approximate bytes reclaimed from history.
        bytes_saved: usize,
    },
    /// Assistant message returned from the LLM (or the simulated
    /// planner). Body is the full message; the sink decides whether
    /// to truncate.
    LlmResponse {
        /// Full assistant response body.
        body: String,
    },
    /// About to dispatch a tool call.
    ToolCallStart {
        /// Tool name as it appeared in the assistant's tool-call block.
        name: String,
        /// Pre-rendered argument string (JSON pretty-printed when
        /// the runner had time to format it).
        arguments: String,
    },
    /// Tool finished. `success = false` means the executor returned
    /// an error string.
    ToolCallEnd {
        /// Tool name (matches the corresponding [`Self::ToolCallStart`]).
        name: String,
        /// Tool result or error string, as returned by the executor.
        result: String,
        /// `true` for a successful execution, `false` for an error.
        success: bool,
    },
    /// The assistant produced a terminal `{"result": ...}` payload.
    /// Emitted once per `Finished` transition.
    FinalResult {
        /// Body of the assistant's terminal result message.
        body: String,
    },
    /// Neither tool call nor a final result — the agent is
    /// continuing the loop with a free-form assistant message.
    ObservingNoAction,
    /// State transition `Observing -> Thinking`.
    Observing,
}

/// Sink for [`TraceEvent`]s emitted by [`RealAgentRunner::run`].
///
/// Implementors must be cheap and non-blocking. The runner calls
/// `event` synchronously from inside the ReAct loop, so a slow
/// sink (e.g. one that writes to a network socket) will starve the
/// LLM call that follows it. If you need async / blocking I/O,
/// push events into a channel and drain them on a separate task.
pub trait TraceSink {
    /// Emit a single event. Called from the runner thread.
    fn event(&mut self, event: TraceEvent);
}

/// Sink that drops every event. Used as the default when no sink is
/// configured — matches the historical "no println at all" behaviour
/// of the no_std builds.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl TraceSink for NullSink {
    #[inline]
    fn event(&mut self, _event: TraceEvent) {}
}

/// Sink that writes events through the [`log`] facade. Lets the
/// runner feed events into whatever backend the binary has wired
/// up (`env_logger`, `defmt`, `simple_logger`, …) without taking a
/// direct dependency on any of them.
///
/// Inert by default (the `log` crate compiles to no-ops when no
/// logger is initialised), so embedded / test builds that never
/// call `env_logger::init()` get the same silent behaviour as
/// `NullSink`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogSink;

impl TraceSink for LogSink {
    fn event(&mut self, event: TraceEvent) {
        match event {
            TraceEvent::RunStart { task } => {
                log::info!(target: "magent.run", "run-start task=\"{}\"", task);
            }
            TraceEvent::BackendReady {
                provider,
                using_real_llm,
            } => {
                if using_real_llm {
                    log::info!(
                        target: "magent.run",
                        "backend-ready provider={} mode=llm",
                        provider
                    );
                } else {
                    log::warn!(
                        target: "magent.run",
                        "backend-ready provider={} mode=simulated",
                        provider
                    );
                }
            }
            TraceEvent::BudgetExhausted { kind, limit } => {
                log::warn!(
                    target: "magent.run",
                    "budget-exhausted kind={} limit={}",
                    kind,
                    limit
                );
            }
            TraceEvent::ThinkingStart {
                iteration,
                tool_calls,
            } => {
                log::debug!(
                    target: "magent.run",
                    "thinking-start iteration={} tool_calls={}",
                    iteration,
                    tool_calls
                );
            }
            TraceEvent::CompressionApplied {
                kept,
                dropped,
                tool_results_truncated,
                bytes_saved,
            } => {
                log::debug!(
                    target: "magent.run",
                    "compression kept={} dropped={} truncated={} bytes_saved={}",
                    kept,
                    dropped,
                    tool_results_truncated,
                    bytes_saved
                );
            }
            TraceEvent::LlmResponse { body } => {
                log::trace!(target: "magent.llm", "response body=\"{}\"", body);
            }
            TraceEvent::ToolCallStart { name, arguments } => {
                log::debug!(
                    target: "magent.tool",
                    "tool-call-start name={} args={}",
                    name,
                    arguments
                );
            }
            TraceEvent::ToolCallEnd {
                name,
                result,
                success,
            } => {
                if success {
                    log::debug!(
                        target: "magent.tool",
                        "tool-call-end name={} result=\"{}\"",
                        name,
                        result
                    );
                } else {
                    log::warn!(
                        target: "magent.tool",
                        "tool-call-end name={} error=\"{}\"",
                        name,
                        result
                    );
                }
            }
            TraceEvent::FinalResult { body } => {
                log::info!(target: "magent.run", "final-result body=\"{}\"", body);
            }
            TraceEvent::ObservingNoAction => {
                log::debug!(target: "magent.run", "observing (no tool call)");
            }
            TraceEvent::Observing => {
                log::trace!(target: "magent.run", "observing -> thinking");
            }
        }
    }
}

/// Convenience: route events to a closure. Useful in tests and in
/// one-off scripts that just want to dump events to stdout.
pub struct FnSink<F: FnMut(TraceEvent)> {
    f: F,
}

impl<F: FnMut(TraceEvent)> FnSink<F> {
    /// Wrap a closure as a [`TraceSink`].
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F: FnMut(TraceEvent)> TraceSink for FnSink<F> {
    fn event(&mut self, event: TraceEvent) {
        (self.f)(event)
    }
}

/// Boxed-erased sink so the runner can hold any concrete
/// implementation behind a single field without paying for a
/// generic parameter on every public method.
///
/// Note: `TraceSink` itself is **not** `Send`. The runner is
/// always single-threaded (one ReAct loop per `RealAgentRunner`)
/// and the CLI's `Output` type holds non-`Send` stdout / stderr
/// locks, so a `Send` bound would be both impossible to satisfy
/// and pointless to require.
pub type BoxedTraceSink = Box<dyn TraceSink>;

/// Thread-safe interior-mutability wrapper around an optional
/// boxed sink. The runner owns this and only ever calls
/// [`Self::with_sink`] from inside the ReAct loop, so a `Mutex` is
/// the right shape — no async, no `RwLock` pessimisation.
///
/// Marked `Send + Sync` (the locks inside are `Send + Sync`)
/// even though the sinks themselves aren't, so the wrapper can
/// live on a shared runner across threads. The `with_sink`
/// accessor takes the lock and then hands the sink out for the
/// duration of a closure; callers can't smuggle the sink across
/// threads because the lock guard is short-lived.
#[derive(Default)]
pub struct SharedTraceSink {
    inner: std::sync::Mutex<Vec<BoxedTraceSink>>,
}

impl SharedTraceSink {
    /// Construct a new, empty shared sink container.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the sink list, recovering from a poisoned mutex.
    ///
    /// A panic in *another* thread while it held this mutex would poison it;
    /// `expect`-ing here would then cascade into a second panic inside the
    /// trace/logging path. The guarded data is still valid — only the poison
    /// flag is set — so recover the guard and keep going.
    fn lock_sinks(&self) -> std::sync::MutexGuard<'_, Vec<BoxedTraceSink>> {
        // HARDENING (clippy/redundant_closure_for_method_calls):
        // `PoisonError::into_inner` is available directly on the error.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append `sink` to the list. Subsequent events are delivered
    /// to every installed sink, in installation order.
    pub fn install(&self, sink: BoxedTraceSink) {
        let mut guard = self.lock_sinks();
        guard.push(sink);
    }

    /// Borrow every installed sink for the duration of `f`. If
    /// no sinks are installed, `f` is not called and `None` is
    /// returned — the runner falls back to its in-line verbose
    /// printing path.
    pub fn with_sinks<R>(
        &self,
        f: impl FnOnce(&mut [&mut dyn TraceSink]) -> R,
    ) -> Option<R> {
        let mut guard = self.lock_sinks();
        if guard.is_empty() {
            return None;
        }
        // Collect a vector of mutable references to the inner
        // trait objects, then hand it to the closure as a slice.
        // The vector lives only for the duration of the call so no
        // sink escapes the borrow scope.
        let mut refs: Vec<&mut dyn TraceSink> =
            guard.iter_mut().map(|b| b.as_mut() as &mut dyn TraceSink).collect();
        Some(f(&mut refs))
    }

    /// Drop every installed sink. After this call the runner is
    /// back to its default silent behaviour.
    pub fn clear(&self) {
        let mut guard = self.lock_sinks();
        guard.clear();
    }

    /// Number of installed sinks. Mostly useful for tests and
    /// diagnostics — production callers should treat the sink
    /// list as opaque.
    pub fn len(&self) -> usize {
        let guard = self.lock_sinks();
        guard.len()
    }

    /// `true` when no sinks are installed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for SharedTraceSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.lock_sinks();
        f.debug_struct("SharedTraceSink")
            .field("installed", &guard.len())
            .finish()
    }
}

/// Agent state in ReAct loop
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentState {
    /// No task has been handed to the agent yet.
    Idle = 0,
    /// Currently generating a model response (calling the LLM).
    Thinking = 1,
    /// Invoking a tool that the LLM requested.
    Executing = 2,
    /// Inspecting the result of a tool call and deciding what to do next.
    Observing = 3,
    /// The agent has emitted its terminal result and the loop is done.
    Finished = 4,
    /// An unrecoverable error was raised; the loop has been aborted.
    Error = 5,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "Idle"),
            AgentState::Thinking => write!(f, "Thinking"),
            AgentState::Executing => write!(f, "Executing"),
            AgentState::Observing => write!(f, "Observing"),
            AgentState::Finished => write!(f, "Finished"),
            AgentState::Error => write!(f, "Error"),
        }
    }
}

/// Tool call from LLM
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Name of the tool the LLM wants to invoke.
    pub name: String,
    /// Parsed JSON arguments, keyed by parameter name.
    pub arguments: HashMap<String, serde_json::Value>,
}

/// Tool result
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Name of the tool that produced this result.
    pub tool_name: String,
    /// Result body (or error string — see [`Self::success`]).
    pub content: String,
    /// `true` for a successful execution, `false` for an error.
    pub success: bool,
}

impl ToolResult {
    /// Build a successful tool result.
    pub fn success(tool_name: &str, content: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            content: content.to_string(),
            success: true,
        }
    }

    /// Build a failed tool result. `error` is stored verbatim in
    /// [`Self::content`].
    pub fn error(tool_name: &str, error: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            content: error.to_string(),
            success: false,
        }
    }
}

/// Message role in the conversation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Conversation-level system prompt (sent once at the start).
    System,
    /// End-user message.
    User,
    /// Assistant message generated by the LLM.
    Assistant,
    /// Tool-result message injected after a tool call returns.
    Tool,
}

impl Role {
    #[inline]
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// Message in conversation.
///
/// v2: `tool_call` and `tool_call_id` are stored separately from `content`
/// so we don't need a separate `conversation_text` mirror.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Speaker role (system / user / assistant / tool).
    pub role: Role,
    /// Body of the message. For tool messages this is the tool's
    /// result string; for assistant messages it's the model's text
    /// (tool-call arguments live in [`Self::tool_call`]).
    pub content: String,
    /// Set on assistant messages that include a tool invocation.
    pub tool_call: Option<ToolCall>,
    /// ID that links a tool-result message back to its
    /// corresponding assistant tool-call message.
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Build a user message.
    #[inline]
    pub fn user(content: &str) -> Self {
        Self {
            role: Role::User,
            content: content.to_string(),
            tool_call: None,
            tool_call_id: None,
        }
    }

    /// Build an assistant message containing only text (no tool call).
    #[inline]
    pub fn assistant_text(content: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: content.to_string(),
            tool_call: None,
            tool_call_id: None,
        }
    }

    /// Build an assistant message that wraps a tool call.
    #[inline]
    pub fn assistant_tool_call(call: ToolCall) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_call: Some(call),
            tool_call_id: None,
        }
    }

    /// Build an assistant tool-call message that carries the same
    /// `tool_call_id` the corresponding tool-result message will use.
    ///
    /// DeepSeek (and OpenAI-compatible APIs generally) require the
    /// assistant's `tool_calls[].id` to match the follow-up `tool`
    /// message's `tool_call_id`; a mismatch makes the API reject the
    /// request with "tool_call_ids did not have response messages".
    #[inline]
    pub fn assistant_tool_call_id(call: ToolCall, tool_call_id: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_call: Some(call),
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    /// Build a tool-result message linked to the assistant's
    /// tool-call by `tool_call_id`.
    #[inline]
    pub fn tool(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: Role::Tool,
            content: content.to_string(),
            tool_call: None,
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    /// Build a system-prompt message.
    #[inline]
    pub fn system(content: &str) -> Self {
        Self {
            role: Role::System,
            content: content.to_string(),
            tool_call: None,
            tool_call_id: None,
        }
    }
}

/// LLM sampling parameters.
#[derive(Debug, Clone, Copy)]
pub struct SamplingParams {
    /// Sampling temperature in `0.0..=2.0`. `0.0` makes the model
    /// greedy; `1.0` is the provider default.
    pub temperature: f32,
    /// Maximum tokens to predict in a single response.
    pub num_predict: usize,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            num_predict: 512,
        }
    }
}

/// Agent configuration
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Maximum ReAct-loop iterations before the runner gives up.
    pub max_iterations: usize,
    /// Maximum total tool calls permitted per run.
    pub max_tool_calls: usize,
    /// System prompt prepended to every conversation.
    pub system_prompt: String,
    /// Whether to print progress to stderr on `cfg(std)` builds.
    pub verbose: bool,
    /// LLM sampling parameters (temperature, max tokens).
    pub sampling: SamplingParams,
    /// Set to `false` to skip LLM-backend discovery on every `run()`
    /// call. Useful when the caller has already probed the backend
    /// (e.g. the `magent` CLI does its own reachability check up-front
    /// and then sets this flag to avoid the redundant network call).
    pub probe_ollama_on_run: bool,
    /// Policy applied to the conversation history before each LLM
    /// call. Tune the limits to stay within the model's context window
    /// (8k for typical local Ollama models, 32k for `deepseek-chat`).
    /// Use `CompressionPolicy::disabled()` to opt out.
    pub compression: crate::conversation::CompressionPolicy,
    /// Tool inventory surfaced to the LLM as part of the system
    /// prompt. Each entry is `(name, description)`. The runner
    /// appends a `## Available tools` section to the system prompt
    /// listing these in declaration order.
    ///
    /// The default is empty — the LLM only sees the tool list you
    /// explicitly populate. Callers that want the default embedded
    /// toolset should populate this from `ToolRegistry::describe()` /
    /// `ToolRegistry::all_tools()` via [`RealAgentRunner::set_tools`].
    pub tool_descriptions: Vec<(String, String)>,
    /// Optional sink for [`TraceEvent`]s. When `Some`, the runner
    /// routes every event through it instead of the legacy
    /// `println!` path. When `None`, the runner falls back to the
    /// historical behaviour (silent for no_std builds, `println!` on
    /// `cfg(std)` builds gated by `verbose`).
    ///
    /// The CLI installs an [`crate::agent_runner::SharedTraceSink`]
    /// backed by an `OutputTraceSink` so `--json` mode stays clean
    /// and embedded callers get full control over routing.
    pub trace_sink: Option<std::sync::Arc<SharedTraceSink>>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_tool_calls: 8,
            system_prompt: HEALTH_SYSTEM_PROMPT.to_string(),
            verbose: true,
            sampling: SamplingParams::default(),
            probe_ollama_on_run: true,
            compression: crate::conversation::CompressionPolicy::default(),
            tool_descriptions: Vec::new(),
            trace_sink: None,
        }
    }
}

impl RunnerConfig {
    /// Render the tool inventory as a human-readable markdown-style
    /// bullet list. Returns an empty string when no tools are
    /// configured, so callers can concatenate defensively:
    ///
    /// ```text
    /// format!("{base}\n\n{tools}", tools = config.tool_prompt())
    /// ```
    ///
    /// Each line is `- <name>: <description>`. Tools with an empty
    /// description are skipped (same policy as
    /// `ToolRegistry::describe()`).
    pub fn tool_prompt(&self) -> String {
        let mut out = String::new();
        for (name, desc) in &self.tool_descriptions {
            if desc.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("- ");
            out.push_str(name);
            out.push_str(": ");
            out.push_str(desc);
        }
        out
    }

    /// Replace the configured tool inventory in one call. Convenience
    /// for hosts that have a `ToolRegistry` and want its full contents
    /// mirrored onto the runner config.
    pub fn set_tool_descriptions(&mut self, tools: Vec<(String, String)>) {
        self.tool_descriptions = tools;
    }
}

const HEALTH_SYSTEM_PROMPT: &str = r#"You are mAgent, an embedded AI health coach agent running on a microcontroller (nRF52840) for smart wearable devices.

You MUST respond with ONLY valid JSON. No explanations, no markdown, no text outside JSON.

Available tools:

ENVIRONMENTAL SENSORS:
- read_sensor(sensor): Read sensor - sensor can be: temperature, accelerometer, humidity, pressure, light, heart_rate, hrv, glucose, ecg

HEALTH & EXERCISE:
- start_exercise(type): Start exercise session - type can be: running, walking, cycling, swimming, strength, yoga, hiit
- end_exercise(): End current exercise session
- get_exercise_goal(): Get current exercise goal
- adjust_exercise_goal(intensity, duration): Adjust exercise goal

HEALTH MONITORING:
- read_heart_rate(): Read current heart rate and HRV
- read_glucose(): Read current blood glucose level
- read_ecg(): Read ECG data
- check_stress_level(): Get current stress level from HRV
- get_sleep_quality(): Get last night's sleep quality metrics

ALERTS & NOTIFICATIONS:
- send_health_alert(type, severity, message): Send health alert notification
- queue_voice(text, priority): Queue voice message to speak
- send_meditation(script): Send meditation guidance script
- set_dnd(enabled, start_hour, end_hour): Set do-not-disturb mode

EMERGENCY:
- notify_emergency_contact(name, message): Notify emergency contact
- find_nearest_hospital(specialty): Find nearest hospital with specialty

GPIO & STORAGE:
- write_gpio(pin, state): Control GPIO - pin (0-31), state (high or low)
- flash_read(address): Read from flash memory - address (integer)
- flash_write(address, data): Write to flash memory - address (integer), data (string)
- ble_send(data): Send via Bluetooth - data (string)

WEB (host-side only, requires internet):
- web_search(query): Search the web via DuckDuckGo and return up to 8 hits as JSON with title/url/snippet.
- fetch_url(url): HTTP GET a URL and return the page body as plain text (HTML stripped, scripts removed).
- webpage_summary(url, query?): Fetch a URL and return a short extractive summary of the page, optionally ranked by relevance to `query`.
- get_weather(city): Current conditions + 3-day forecast for a city as compact JSON (Open-Meteo, no API key). Prefer this over fetch_url for weather questions.

Rules:
1. ALWAYS respond with ONLY JSON
2. To call a tool, use: {"tool": "tool_name", "args": {"param": "value"}}
3. When task is done, use: {"result": "description of what was done"}
4. Be concise and efficient for embedded device
5. Prioritize user health and safety in all decisions
6. For exercise coaching, monitor heart rate and provide breathing corrections
7. For stress detection, use HRV data and offer meditation when stress is high
8. For chronic disease monitoring, track glucose/ECG trends and alert on anomalies"#;

/// Agent runner trait - abstracts the tool execution backend.
pub trait ToolExecutor: Send {
    /// Dispatch `tool` with JSON-formatted `args` and return its
    /// stringified result. An `Err` here is treated as a tool
    /// failure by the runner (the loop keeps going).
    fn execute(&mut self, tool: &str, args: &str) -> std::result::Result<String, String>;
}

// ============================================================================
// LLM backend abstraction
// ============================================================================
//
// The ReAct loop talks to "the LLM" through this trait. Today we ship
// two implementations:
//
//   * [`OllamaClient`] — local-first, talks to anything speaking the
//     Ollama HTTP API (`/api/chat`). Default for the CLI.
//   * [`DeepSeekClient`] — OpenAI-compatible, talks to DeepSeek's
//     hosted API at `https://api.deepseek.com/v1/chat/completions`.
//
// Adding more providers (OpenAI direct, Anthropic, etc.) is a matter
// of implementing [`LlmBackend`] — the agent runner doesn't need to
// change. Wire-format conversion is the implementor's problem.

/// Identifier for the LLM provider backing a [`LlmBackend`]. Returned by
/// [`LlmBackend::provider`] so callers (CLI doctor, logs) can show
/// which backend is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    /// Anything speaking the Ollama HTTP API (local `ollama serve`
    /// or remote Ollama-compatible host).
    Ollama,
    /// DeepSeek's hosted OpenAI-compatible endpoint at
    /// `api.deepseek.com`.
    DeepSeek,
    /// Fallback for any user-supplied backend; we don't know its name
    /// without inspecting it.
    Custom,
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmProvider::Ollama => f.write_str("ollama"),
            LlmProvider::DeepSeek => f.write_str("deepseek"),
            LlmProvider::Custom => f.write_str("custom"),
        }
    }
}

/// Unified interface between the ReAct loop and any chat-completions API.
pub trait LlmBackend: Send + std::any::Any {
    /// Cheap reachability probe. Used to decide whether to enable the
    /// backend on a given `run()` call.
    fn check_connection(&self) -> bool;

    /// Run a chat completion against the configured model. Returns the
    /// assistant message text (which may be a tool-call JSON, a result
    /// JSON, or free-form chatter — the runner's parser handles all
    /// three).
    fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<String, String>;

    /// Which provider this is. Used for logging / `--json` envelopes.
    fn provider(&self) -> LlmProvider;

    /// Display name of the configured model. Used for diagnostics.
    fn model(&self) -> &str;

    /// Base URL or endpoint the backend talks to. Used for diagnostics.
    fn base_url(&self) -> &str;
}

// ============================================================================
// Ollama client
// ============================================================================

/// Ollama client for real LLM inference.
///
/// v2: now caches the URL components and supports `chat_with_messages`, which
/// consumes structured `&[Message]` directly and avoids the prefix-parsing
/// round-trip in v1.
pub struct OllamaClient {
    /// Base URL of the Ollama HTTP server, with trailing `/` trimmed.
    base_url: String,
    /// Model name (e.g. `"qwen2.5:3b"`) to request on every chat call.
    model: String,
    /// Shared blocking `reqwest` client.
    client: reqwest::blocking::Client,
    /// Reusable JSON body buffer; cleared and refilled each call.
    body_buf: Vec<u8>,
    /// Reusable response buffer.
    response_buf: Vec<u8>,
}

impl std::fmt::Debug for OllamaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl OllamaClient {
    /// Construct a client targeting the Ollama server at `base_url`
    /// using the supplied `model`. The HTTP client has a 60-second
    /// timeout per call and disables keep-alive so tests see a
    /// deterministic connection lifecycle.
    pub fn new(base_url: &str, model: &str) -> Self {
        // Use a fresh client per call would defeat keep-alive benefits, but
        // for `chat_with_messages` we send the body once and read once, so
        // disabling keep-alive is the safest configuration. Tests in
        // particular benefit from a deterministic connection lifecycle.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(0)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            // Trim trailing slashes so `--ollama http://host:port/` doesn't
            // produce URLs like `http://host:port//api/chat`. DeepSeek
            // already does this; do it here too for consistency.
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            client,
            // See `DeepSeekClient::with_endpoint` for why `body_buf`
            // starts empty: the first `chat_with_messages` moves it
            // out via `mem::take`, so pre-allocating 8KB would just
            // be thrown away.
            body_buf: Vec::new(),
            response_buf: Vec::with_capacity(16 * 1024),
        }
    }

    /// Issue an unauthenticated `GET /api/tags` against the server and
    /// report whether the response was `2xx`. Useful for the CLI's
    /// `--doctor` flow and for the runner's optional backend probe.
    pub fn check_connection(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Chat completion from structured messages. This is the preferred entry
    /// point: it serializes messages directly to JSON without intermediate
    /// `String` allocations.
    pub fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<String, String> {
        // `write_chat_body` writes into `body_buf`. After the previous
        // call `mem::take` left it empty, so we don't need to clear.
        self.write_chat_body(messages, sampling)?;
        let body = std::mem::take(&mut self.body_buf);

        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        self.response_buf.clear();
        self.response_buf
            .extend_from_slice(&response.bytes().map_err(|e| format!("Read body failed: {}", e))?);

        let json: serde_json::Value = serde_json::from_slice(&self.response_buf)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        let content = json["message"]["content"]
            .as_str()
            .ok_or_else(|| "No content in response".to_string())?;

        Ok(content.to_string())
    }

    fn write_chat_body(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<(), String> {
        use std::io::Write;
        let buf = &mut self.body_buf;
        buf.push(b'{');
        write!(buf, "\"model\":\"{}\",\"stream\":false,\"options\":{{\"temperature\":{},\"num_predict\":{}}}",
               escape_json(&self.model), sampling.temperature, sampling.num_predict)
            .map_err(|e| format!("Write body: {}", e))?;
        buf.extend_from_slice(b",\"messages\":[");
        let mut first = true;
        for m in messages {
            if !first {
                buf.push(b',');
            }
            first = false;
            write_message_json(buf, m).map_err(|e| format!("Write msg: {}", e))?;
        }
        buf.push(b']');
        buf.push(b'}');
        Ok(())
    }

    /// Backward-compatible string-array entry point.
    pub fn chat(
        &mut self,
        messages: &[String],
        system_prompt: &str,
    ) -> std::result::Result<String, String> {
        // Translate the legacy "[User] ..." / "[Assistant] ..." / "[Tool] ..."
        // representations into structured `Message`s.
        let mut structured: Vec<Message> = Vec::with_capacity(messages.len() + 1);
        structured.push(Message::system(system_prompt));
        for raw in messages {
            if let Some(rest) = raw.strip_prefix("[User] ") {
                structured.push(Message::user(rest));
            } else if let Some(rest) = raw.strip_prefix("[Assistant] ") {
                structured.push(Message::assistant_text(rest));
            } else if let Some(rest) = raw.strip_prefix("[Tool] ") {
                // Strip "name: content" -> content; tool_call_id is unknown.
                let (id, content) = match rest.find(": ") {
                    Some(i) => (&rest[..i], &rest[i + 2..]),
                    None => ("call", rest),
                };
                structured.push(Message::tool(id, content));
            } else if let Some(rest) = raw.strip_prefix("[System] ") {
                let task = rest.strip_prefix("Task: ").unwrap_or(rest);
                structured.push(Message::user(task));
            } else {
                structured.push(Message::user(raw));
            }
        }
        self.chat_with_messages(&structured, SamplingParams::default())
    }

    /// List the models currently available on the Ollama server by
    /// calling `GET /api/tags`. Returns an empty list on any error
    /// (network failure, non-JSON body, missing `models` array).
    pub fn get_models(&self) -> Vec<String> {
        if let Ok(response) = self.client.get(format!("{}/api/tags", self.base_url)).send() {
            if let Ok(json) = response.json::<serde_json::Value>() {
                return json["models"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| m["name"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
        vec![]
    }

    /// Override the model name used for subsequent chat calls.
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// Currently configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Currently configured base URL (with trailing `/` already trimmed).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Generate text from a raw prompt via `POST /api/generate`. Use
    /// [`Self::chat_with_messages`] instead when the caller has a
    /// structured `Message` history — that path avoids the
    /// prefix-parsing round-trip and is the one the runner uses.
    pub fn generate(&self, prompt: &str) -> std::result::Result<String, String> {
        let request_body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "options": {
                "temperature": 0.3,
                "num_predict": 512
            }
        });

        let response = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&request_body)
            .send()
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let json: serde_json::Value = response.json().map_err(|e| format!("JSON parse error: {}", e))?;
        let content = json["response"]
            .as_str()
            .ok_or_else(|| "No content in response".to_string())?;

        Ok(content.to_string())
    }
}

// ============================================================================
// LlmBackend impl for Ollama
// ============================================================================
//
// `check_connection` and `chat_with_messages` are forwarded to the
// existing inherent methods. The trait exposes read-only accessors so
// the CLI's diagnostics can show "ollama @ http://...:11434 (model: qwen2.5)".
impl LlmBackend for OllamaClient {
    fn check_connection(&self) -> bool {
        // Delegate to the inherent method so behaviour stays identical.
        OllamaClient::check_connection(self)
    }

    fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<String, String> {
        OllamaClient::chat_with_messages(self, messages, sampling)
    }

    fn provider(&self) -> LlmProvider {
        LlmProvider::Ollama
    }

    fn model(&self) -> &str {
        OllamaClient::model(self)
    }

    fn base_url(&self) -> &str {
        OllamaClient::base_url(self)
    }
}

// ============================================================================
// DeepSeek client
// ============================================================================
//
// DeepSeek exposes an OpenAI-compatible chat-completions API at
// `https://api.deepseek.com/v1/chat/completions`. We POST the same
// `messages` array we use for Ollama, but the body shape and auth
// header differ:
//
//   POST {base_url}/chat/completions
//   Authorization: Bearer {api_key}
//   Content-Type:  application/json
//   {
//     "model":       "<model>",
//     "messages":    [{"role": "...", "content": "..."}],
//     "temperature": <f32>,
//     "max_tokens":  <usize>,
//     "stream":      false
//   }
//
// Response: `{"choices": [{"message": {"role": "assistant", "content": "..."}}]}`.
//
// The agent runner only ever reads `choices[0].message.content`, which
// matches DeepSeek's documented response shape.

/// Client for DeepSeek's hosted chat-completions API.
///
/// Construction refuses an empty API key (returns `None` from
/// [`DeepSeekClient::try_new`]). The infallible [`DeepSeekClient::new`]
/// constructor exists for backwards compatibility but asserts on the
/// empty key — prefer `try_new` in new code.
pub struct DeepSeekClient {
    base_url: String,
    model: String,
    /// API key. Kept in a private field and **redacted** in the
    /// `Debug` impl so it doesn't leak into logs / panic messages.
    api_key: Redacted<String>,
    client: reqwest::blocking::Client,
    /// Reusable JSON body buffer; cleared and refilled each call.
    body_buf: Vec<u8>,
    /// Reusable response buffer.
    response_buf: Vec<u8>,
}

/// Tiny newtype so we can implement `Debug` without leaking the key.
/// `Redacted<T>` derefs to `T` and forwards every other method.
#[derive(Clone)]
struct Redacted<T>(T);

impl<T> std::ops::Deref for Redacted<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for Redacted<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl std::fmt::Debug for Redacted<String> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"<redacted>\"")
    }
}

impl DeepSeekClient {
    /// Build a client pointing at the public DeepSeek API with the
    /// default `deepseek-chat` model. Panics if `api_key` is empty —
    /// use [`try_new`] for fallible construction.
    pub fn new(api_key: &str) -> Self {
        Self::try_new(api_key).expect("DeepSeekClient::new requires a non-empty API key")
    }

    /// Build a client pointing at the public DeepSeek API with the
    /// default `deepseek-chat` model. Returns `None` if `api_key` is
    /// empty or contains only whitespace.
    pub fn try_new(api_key: &str) -> Option<Self> {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self::with_endpoint(
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            trimmed,
        ))
    }

    /// Build a client with an explicit base URL, model name, and key.
    /// Useful for tests against a DeepSeek-compatible mock server.
    ///
    /// Trims trailing slashes from `base_url`. Does **not** validate
    /// `api_key` (use `try_with_endpoint` for that).
    pub fn with_endpoint(base_url: &str, model: &str, api_key: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(0)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: Redacted(api_key.to_string()),
            client,
            // `Vec::new()` — the first `chat_with_messages` call
            // moves the buffer out via `mem::take` and we let the
            // allocator grow it as needed. The next call starts from
            // an empty `Vec::new()` again, so pre-allocating 8KB here
            // would just get thrown away.
            body_buf: Vec::new(),
            // Pre-allocate the response buffer because it stays in
            // the struct across calls (the request consumes the body
            // buffer but the response we read into a separate Vec).
            response_buf: Vec::with_capacity(16 * 1024),
        }
    }

    /// Like [`with_endpoint`] but refuses an empty / whitespace-only
    /// API key.
    pub fn try_with_endpoint(base_url: &str, model: &str, api_key: &str) -> Option<Self> {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self::with_endpoint(base_url, model, trimmed))
    }

    /// Currently configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Currently configured base URL (with trailing `/` already trimmed).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Override the model name used for subsequent chat calls.
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// Borrow the API key without exposing it through `Debug`.
    pub fn api_key(&self) -> &str {
        &self.api_key.0
    }

    /// Run a chat completion, returning the assistant text.
    pub fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<String, String> {
        // Take the body buffer out so we can move it into the request
        // without cloning. After the request returns, `self.body_buf`
        // is still the empty `Vec::new()` from `mem::take` — no heap
        // allocation needed.
        let mut body = std::mem::take(&mut self.body_buf);
        DeepSeekClient::write_chat_body_into(&mut body, &self.model, messages, sampling)?;

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key.0),
            )
            .body(body)
            .send()
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            // Try to surface the error message DeepSeek sends in the
            // body — it's far more useful than a bare 4xx code. Use
            // `from_utf8_lossy` so a non-UTF-8 body doesn't blow up
            // the diagnostic, and `chars().take(N)` so we never
            // slice in the middle of a multi-byte UTF-8 sequence.
            let body = response.text().unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            return Err(format!("HTTP error: {} — {}", status, snippet));
        }

        self.response_buf.clear();
        self.response_buf.extend_from_slice(
            &response
                .bytes()
                .map_err(|e| format!("Read body failed: {}", e))?,
        );

        let json: serde_json::Value = serde_json::from_slice(&self.response_buf)
            .map_err(|e| format!("JSON parse error: {}", e))?;

        // OpenAI / DeepSeek shape:
        //   {"choices": [{"message": {"role": "assistant", "content": "..."}}]}
        //
        // Note: `choices` is an array — a non-streaming response
        // always has exactly one element, but we still need to index
        // `[0]` defensively in case the server returns something
        // weird (e.g. an empty `choices` during a transient outage).
        let content = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!(
                    "No content in response (choices={})",
                    json.get("choices").map(ToString::to_string).unwrap_or_else(|| "<missing>".into())
                )
            })?;
        Ok(content.to_string())
    }

    /// Write the OpenAI-compatible chat body into `buf`. Spelled out
    /// as a free function so tests can call it without instantiating
    /// a client.
    fn write_chat_body_into(
        buf: &mut Vec<u8>,
        model: &str,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<(), String> {
        use std::io::Write;
        buf.push(b'{');
        // We explicitly disable DeepSeek's thinking mode. DeepSeek-V4 is a
        // reasoning model: with thinking enabled it emits a `reasoning_content`
        // field on every assistant message and then REQUIRES that field to be
        // passed back verbatim on the next turn (the API rejects the request
        // with "reasoning_content in the thinking mode must be passed back").
        // Our ReAct loop rebuilds assistant messages from `parse_tool_call` /
        // `parse_result` and does not carry `reasoning_content`, so any
        // multi-turn (tool) conversation would fail and silently fall back to
        // the canned simulated response. Disabling thinking keeps the wire
        // format compatible with the loop and makes tool calling work across
        // turns (verified against deepseek-v4-pro / v4-flash / deepseek-chat).
        write!(
            buf,
            "\"model\":\"{}\",\"thinking\":{{\"type\":\"disabled\"}},\"stream\":false,\"temperature\":{},\"max_tokens\":{}",
            escape_json(model),
            sampling.temperature,
            sampling.num_predict
        )
        .map_err(|e| format!("Write body: {}", e))?;
        buf.extend_from_slice(b",\"messages\":[");
        let mut first = true;
        for m in messages {
            if !first {
                buf.push(b',');
            }
            first = false;
            DeepSeekClient::write_message_into(buf, m)?;
        }
        buf.extend_from_slice(b"]}");
        Ok(())
    }

    /// Serialise a single message in OpenAI/DeepSeek shape. Handles
    /// three message kinds:
    ///
    /// * plain `{role, content}` (system / user / assistant text)
    /// * assistant `{role: "assistant", content: null, tool_calls: [...]}` (the
    ///   model decided to call a tool)
    /// * tool result `{role: "tool", tool_call_id: "...", content: "..."}`
    ///
    /// The ReAct loop sends assistant tool_call messages *and* tool
    /// result messages back to the model, so we need to honour both
    /// — losing `tool_call_id` on a tool-result message makes
    /// DeepSeek reject the conversation as malformed.
    ///
    /// Delegates to the module-level [`write_message_into`] so we
    /// only have one place that knows the wire format. The Ollama
    /// client uses the same helper.
    fn write_message_into(
        buf: &mut Vec<u8>,
        m: &Message,
    ) -> std::result::Result<(), String> {
        write_message_into(buf, m)
    }

    #[allow(dead_code)]
    fn write_chat_body(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<(), String> {
        Self::write_chat_body_into(&mut self.body_buf, &self.model, messages, sampling)
    }

    /// Test-only entry point that returns the JSON body bytes instead
    /// of writing them into the internal buffer. Lets unit tests
    /// assert on the wire format without spinning up a mock server.
    #[cfg(test)]
    fn write_chat_body_for_test(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<Vec<u8>, String> {
        // Unlike the production path which is preceded by `mem::take`,
        // tests may call this multiple times on the same client. Clear
        // so we never emit a JSON document that starts with leftover
        // bytes from the previous call.
        self.body_buf.clear();
        self.write_chat_body(messages, sampling)?;
        Ok(self.body_buf.clone())
    }
}

/// Custom `Debug` impl that redacts the API key. Without this, a
/// `dbg!(client)` would print the full key into the terminal / log.
impl std::fmt::Debug for DeepSeekClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .field("body_buf.capacity", &self.body_buf.capacity())
            .field("response_buf.capacity", &self.response_buf.capacity())
            .finish()
    }
}

/// JSON-string-serialise a string into a `Vec<u8>` buffer, escaping the
/// characters that would break the wire format. Shared by DeepSeek's
/// message encoder (Ollama has its own custom writer because it also
/// has to handle tool_call fields).
fn write_json_string(buf: &mut Vec<u8>, s: &str) -> std::io::Result<()> {
    use std::io::Write;
    buf.push(b'"');
    for c in s.chars() {
        match c {
            '"' => buf.extend_from_slice(b"\\\""),
            '\\' => buf.extend_from_slice(b"\\\\"),
            '\n' => buf.extend_from_slice(b"\\n"),
            '\r' => buf.extend_from_slice(b"\\r"),
            '\t' => buf.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => {
                let mut buf_c = [0u8; 4];
                let s = c.encode_utf8(&mut buf_c);
                buf.extend_from_slice(s.as_bytes());
            }
        }
    }
    buf.push(b'"');
    Ok(())
}

impl LlmBackend for DeepSeekClient {
    fn check_connection(&self) -> bool {
        // DeepSeek exposes `/models` (OpenAI-compatible list). We hit
        // it just to confirm reachability + auth. Auth failure still
        // counts as "reachable" because it tells us the network path
        // works; the runner will surface the auth error later when
        // it actually tries to chat.
        //
        // An empty key (which shouldn't happen with `try_new` /
        // `try_with_endpoint`, but is still possible if a caller
        // built the struct via the lower-level `with_endpoint`) is
        // treated as "not connected" so the runner falls back to
        // simulated reasoning instead of trying chat and getting a
        // confusing 401.
        if self.api_key.0.trim().is_empty() {
            return false;
        }
        self.client
            .get(format!("{}/models", self.base_url))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key.0),
            )
            .send()
            .map(|r| r.status().as_u16() < 500)
            .unwrap_or(false)
    }

    fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> std::result::Result<String, String> {
        DeepSeekClient::chat_with_messages(self, messages, sampling)
    }

    fn provider(&self) -> LlmProvider {
        LlmProvider::DeepSeek
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Minimal JSON string escaper for the model name. Avoids pulling in
/// `serde_json::to_writer` (which would re-allocate a Vec) on the hot path.
#[inline]
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[inline]
fn write_message_json(buf: &mut Vec<u8>, m: &Message) -> std::io::Result<()> {
    use std::io::Write;
    buf.push(b'{');
    write!(buf, "\"role\":\"{}\"", m.role.as_str())?;
    if let Some(ref tc) = m.tool_call {
        // Tool calls are encoded as OpenAI-style "assistant with tool_calls".
        buf.extend_from_slice(b",\"content\":null");
        buf.extend_from_slice(b",\"tool_calls\":[{\"id\":\"");
        if let Some(ref id) = m.tool_call_id {
            // Escape: tool_call_id is allowed to contain arbitrary
            // characters per the OpenAI spec, so we must escape
            // quotes / backslashes just like DeepSeek does.
            buf.extend_from_slice(escape_json(id).as_bytes());
        } else {
            buf.extend_from_slice(b"call_0");
        }
        buf.extend_from_slice(b"\",\"type\":\"function\",\"function\":{\"name\":\"");
        buf.extend_from_slice(escape_json(&tc.name).as_bytes());
        buf.extend_from_slice(b"\",\"arguments\":\"");
        // Arguments are serialised as compact JSON inside a string field;
        // most Ollama models accept either object or string-encoded JSON.
        let args_str = serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string());
        buf.extend_from_slice(escape_json(&args_str).as_bytes());
        buf.extend_from_slice(b"\"}}]");
    } else {
        // Write the content string directly into the byte buffer
        // (no intermediate `String` allocation).
        buf.extend_from_slice(b",\"content\":");
        write_json_string(buf, &m.content)?;
        if let Some(ref id) = m.tool_call_id {
            buf.extend_from_slice(b",\"tool_call_id\":\"");
            buf.extend_from_slice(escape_json(id).as_bytes());
            buf.push(b'"');
        }
    }
    buf.push(b'}');
    Ok(())
}

/// Free-function wrapper around [`write_message_json`] that converts
/// the `io::Result` into a `Result<(), String>` so callers in the
/// DeepSeek / Ollama modules can keep their existing `Result<(), String>`
/// return type. Both clients delegate to this helper so they share
/// one implementation of the wire format.
#[inline]
fn write_message_into(buf: &mut Vec<u8>, m: &Message) -> std::result::Result<(), String> {
    write_message_json(buf, m).map_err(|e| format!("Write msg: {}", e))
}

// ============================================================================
// Tool-call wire-format tolerant parsing
// ============================================================================

/// Remove a surrounding markdown code fence (and any language tag) from an
/// LLM response so both JSON and XML tool-call payloads parse cleanly even
/// when the model wraps them in ```` ```json ```` / ```` ```xml ```` blocks.
fn strip_code_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(body) = t.strip_prefix("```") {
        // Drop the trailing fence, then any leading language-tag line.
        let body = body.strip_suffix("```").unwrap_or(body);
        match body.find('\n') {
            Some(i) => body[i + 1..].trim().to_string(),
            None => body.trim().to_string(),
        }
    } else {
        t.to_string()
    }
}

/// Find `needle` inside `haystack`, returning its byte offset.
fn find_sub(haystack: &str, needle: &str) -> Option<usize> {
    haystack.find(needle)
}

/// Extract the value of a double-quoted XML attribute, e.g. `name="X"`.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let marker = format!("{attr}=\"");
    let idx = tag.find(&marker)?;
    let rest = &tag[idx + marker.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Parse an Anthropic-style tool-use block:
///
/// ```xml
/// <invoke name="fetch_url">
///   <parameter name="url">https://example.com</parameter>
/// </invoke>
/// ```
///
/// Both the bare (`<invoke>` / `<parameter>`) and Claude-namespace
/// (`<antml:invoke>` / `<antml:parameter>`) spellings are accepted. Values
/// are surfaced as JSON strings. Returns `None` if no usable block is found.
fn parse_anthropic_tool_call(
    response: &str,
) -> Option<(String, HashMap<String, serde_json::Value>)> {
    // Normalise the `antml:` namespace prefix away so one code path handles
    // both spellings.
    let s = response.replace("<antml:", "<").replace("</antml:", "</");

    let invoke_off = find_sub(&s, "<invoke")?;
    let invoke_tag_end = find_sub(&s[invoke_off..], ">")? + invoke_off;
    let invoke_tag = &s[invoke_off..invoke_tag_end];
    let name = extract_attr(invoke_tag, "name")?;
    if name.is_empty() {
        return None;
    }

    let close_off = find_sub(&s[invoke_tag_end..], "</invoke>")? + invoke_tag_end;
    let inner = &s[invoke_tag_end..close_off];

    let mut args: HashMap<String, serde_json::Value> = HashMap::new();
    let mut pos = 0;
    while let Some(p) = find_sub(&inner[pos..], "<parameter") {
        let p_abs = pos + p;
        let tag_end = find_sub(&inner[p_abs..], ">")? + p_abs;
        let tag = &inner[p_abs..tag_end];
        if let Some(key) = extract_attr(tag, "name") {
            let val_start = tag_end + 1;
            if let Some(body_close_rel) = find_sub(&inner[val_start..], "</parameter>") {
                let body_close = val_start + body_close_rel;
                let value = inner[val_start..body_close].trim().to_string();
                args.insert(key, serde_json::Value::String(value));
                pos = body_close + "</parameter>".len();
                continue;
            }
        }
        // Malformed parameter block — stop to avoid an infinite loop.
        break;
    }

    if args.is_empty() {
        return None;
    }
    Some((name, args))
}

/// Minimum byte length for a plain-text (non-JSON, non-fenced) response to
/// be treated as a final result rather than mid-loop chatter. Keeps short
/// interjections ("OK", "Let me check") from terminating the loop early while
/// still capturing real prose answers.
const MIN_PROSE_RESULT_LEN: usize = 12;

/// Convert a JSON `result` value to its canonical string form, preferring the
/// most specific numeric type so an integer doesn't silently become "42.0".
fn result_value_to_string(result: &serde_json::Value) -> String {
    if let Some(n) = result.as_i64() {
        return n.to_string();
    }
    if let Some(n) = result.as_u64() {
        return n.to_string();
    }
    if let Some(n) = result.as_f64() {
        return format!("{:.1}", n);
    }
    if let Some(s) = result.as_str() {
        return s.to_string();
    }
    result.to_string()
}

/// Heuristic guard: does this trimmed string read like mid-plan chatter
/// ("I'll now read the sensor…") rather than a final answer? Used so the
/// prose fallback in `parse_result` doesn't terminate the loop prematurely.
fn looks_like_chatter(s: &str) -> bool {
    let low = s.to_lowercase();
    const PREFIXES: [&str; 14] = [
        "i will",
        "i'll",
        "let me",
        "let's",
        "now ",
        "next",
        "first",
        "ok",
        "okay",
        "sure",
        "alright",
        "then ",
        "to continue",
        "on the other hand",
    ];
    PREFIXES.iter().any(|p| low.starts_with(p))
}

/// Find the first *balanced* JSON object `{...}` in `s`, skipping any prose or
/// markdown that precedes it. Returns the object substring including braces.
///
/// This is how we recover a tool call or result that a model wrapped in
/// surrounding text — a common format violation. The scanner tracks string
/// literals and brace depth so it doesn't stop at a `}` inside a string value.
fn find_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut depth = 0i32;
            let mut j = i;
            let mut in_string = false;
            let mut escaped = false;
            while j < bytes.len() {
                let b = bytes[j];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        in_string = false;
                    }
                } else {
                    match b {
                        b'"' => in_string = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(&s[i..=j]);
                            }
                        }
                        _ => {}
                    }
                }
                j += 1;
            }
            // Unbalanced starting at `i` — try the next opening brace.
        }
        i += 1;
    }
    None
}

/// Shared tool-call recogniser over a parsed JSON value. Covers the three JSON
/// shapes mAgent emits/accepts; returns `None` if `json` isn't a tool call.
fn parse_tool_call_from_json(
    json: &serde_json::Value,
) -> Option<(String, HashMap<String, serde_json::Value>)> {
    // Format 1: {"tool": "name", "args": {...}}
    if let (Some(tool), Some(args)) = (json.get("tool"), json.get("args")) {
        let name = tool.as_str()?.to_string();
        let args_map: HashMap<String, serde_json::Value> = args
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        return Some((name, args_map));
    }

    // Format 2: OpenAI-style "tool_calls" array.
    if let Some(tcs) = json.get("tool_calls").and_then(|v| v.as_array()) {
        if let Some(first) = tcs.first() {
            if let Some(func) = first.get("function") {
                let name = func
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from)?;
                let args_map: HashMap<String, serde_json::Value> =
                    if let Some(args_str) = func.get("arguments").and_then(|a| a.as_str()) {
                        // arguments is itself a JSON string
                        serde_json::from_str(args_str).unwrap_or_default()
                    } else if let Some(args_obj) =
                        func.get("arguments").and_then(|a| a.as_object())
                    {
                        args_obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    } else {
                        HashMap::new()
                    };
                return Some((name, args_map));
            }
        }
    }

    // Format 3: {"<tool_name>": {"args": {...}}} - generic, no whitelist.
    if let Some(obj) = json.as_object() {
        for key in obj.keys() {
            if matches!(key.as_str(), "result" | "error" | "reasoning" | "thought") {
                continue;
            }
            if let Some(tool_obj) = obj.get(key) {
                if tool_obj.is_object() {
                    let name = key.clone();
                    let args_map: HashMap<String, serde_json::Value> =
                        if let Some(args) = tool_obj.get("args").and_then(|a| a.as_object()) {
                            args.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                        } else if let Some(args) = tool_obj.as_object() {
                            args.iter()
                                .filter(|(k, _)| *k != "args")
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect()
                        } else {
                            HashMap::new()
                        };
                    return Some((name, args_map));
                }
            }
        }
    }

    None
}


// ============================================================================
// ReAct agent runner
// ============================================================================

/// Real agent runner with full ReAct loop.
///
/// v2: drops the dual `messages` / `conversation_text` representation, removes
/// the hard-coded tool-name whitelist in `parse_tool_call`, and adds a fast
/// path for the simulated planner.
pub struct RealAgentRunner<E: ToolExecutor> {
    executor: E,
    state: AgentState,
    messages: Vec<Message>,
    iteration: usize,
    tool_call_count: usize,
    config: RunnerConfig,
    /// Snapshot of the [`CompressionStats`] from the most recent
    /// call to [`Self::apply_compression`]. `None` until the first
    /// LLM call completes. Used by the CLI's `--save-summary` path
    /// to populate the `stats` field of the persisted summary record
    /// with the actual compression counts rather than a conservative
    /// zero-filled placeholder.
    last_compression_stats: Option<crate::conversation::CompressionStats>,
    /// Snapshot of the [`CompressionPolicy`] that produced
    /// `last_compression_stats`. Stored verbatim so the summary record
    /// can tell whether the window was generated with aggressive or
    /// conservative limits.
    last_compression_policy: Option<crate::summary::CompressionPolicySnapshot>,
    /// Whether the ReAct loop should route the next chat through the
    /// configured [`LlmBackend`]. Flipped to `true` automatically by
    /// `probe_ollama_on_run` when the probe succeeds.
    backend_enabled: bool,
    /// Pluggable LLM backend. `Some` by default (Ollama at
    /// `http://localhost:11434` with `llama3.2`); users can swap in a
    /// `DeepSeekClient` or any other [`LlmBackend`].
    backend: Option<Box<dyn LlmBackend>>,
}

impl<E: ToolExecutor> RealAgentRunner<E> {
    /// Build a runner with the default [`OllamaClient`] backend at
    /// `http://localhost:11434` using the `llama3.2` model.
    pub fn new(executor: E) -> Self {
        Self {
            executor,
            state: AgentState::Idle,
            messages: Vec::with_capacity(32),
            iteration: 0,
            tool_call_count: 0,
            config: RunnerConfig::default(),
            last_compression_stats: None,
            last_compression_policy: None,
            backend_enabled: false,
            backend: Some(Box::new(OllamaClient::new("http://localhost:11434", "llama3.2"))),
        }
    }

    /// Build a runner that uses the supplied tool `executor` and the
    /// provided `config` verbatim.
    pub fn with_config(executor: E, config: RunnerConfig) -> Self {
        Self {
            executor,
            state: AgentState::Idle,
            messages: Vec::with_capacity(32),
            iteration: 0,
            tool_call_count: 0,
            config,
            last_compression_stats: None,
            last_compression_policy: None,
            backend_enabled: false,
            backend: Some(Box::new(OllamaClient::new("http://localhost:11434", "llama3.2"))),
        }
    }

    /// Mirror a `ToolRegistry`'s contents onto the runner's tool
    /// inventory. This is the convenience entry point for the
    /// `describe()→LLM` task: after this call, every LLM chat request
    /// the runner makes will prepend a `## Available tools` section
    /// to the system prompt listing the registered tools.
    ///
    /// The shape is intentionally identical to what the embedded
    /// firmware sees (the `ToolRegistry::describe()` contract), so a
    /// host CLI and a firmware build can share the same description
    /// string without translation.
    pub fn set_tools(&mut self, registry: &crate::tools::ToolRegistry) {
        let descriptions: Vec<(String, String)> = registry
            .all_tools()
            .iter()
            .map(|t| (t.name.to_string(), t.description.to_string()))
            .collect();
        self.config.set_tool_descriptions(descriptions);
    }

    /// Render the runner's effective system prompt: the configured
    /// base prompt concatenated with the `## Available tools` section
    /// (when `tool_descriptions` is non-empty). Use this when you
    /// want to inspect what the LLM is about to see, or when
    /// embedding the prompt in a custom runner.
    pub fn build_system_prompt(&self) -> String {
        let tool_section = self.config.tool_prompt();
        if tool_section.is_empty() {
            return self.config.system_prompt.clone();
        }
        // Header is two newlines + the section title + a blank line,
        // matching the markdown shape `ToolRegistry::describe()` uses
        // for its bullets. We deliberately do NOT mutate
        // `self.config.system_prompt` — the runner rebuilds the
        // prompt on every chat call, so storing the rendered prompt
        // would only cause confusion.
        let mut out = String::with_capacity(
            self.config.system_prompt.len() + tool_section.len() + 32,
        );
        out.push_str(&self.config.system_prompt);
        out.push_str("\n\n## Available tools\n");
        out.push_str(&tool_section);
        out
    }

    /// Install (or replace) the trace sink. See
    /// [`RunnerConfig::trace_sink`] for the rationale. Pass `None`
    /// to revert to the historical silent / `println!` behaviour.
    pub fn set_trace_sink(
        &mut self,
        sink: Option<std::sync::Arc<SharedTraceSink>>,
    ) {
        self.config.trace_sink = sink;
    }

    /// Toggle the legacy `println!` fallback used when no trace
    /// sink is configured. Defaults to `true`. Callers wiring up
    /// a `SharedTraceSink` typically want to leave this on so the
    /// pre-refactor stdout output still appears for users that
    /// don't install their own sink.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.config.verbose = verbose;
    }

    /// Borrow the configured trace sink (if any). Used by the
    /// internal loop helpers below; not part of the public API
    /// beyond being reachable through [`Self::set_trace_sink`].
    fn trace_sink(&self) -> Option<&std::sync::Arc<SharedTraceSink>> {
        self.config.trace_sink.as_ref()
    }

    /// Insert `msg` immediately **after** the leading system prompt.
    ///
    /// Used by the CLI's `--load-summary <TOPIC>` path to prepend
    /// a "context from the previous run" note without disturbing
    /// the user's `--prompt` / `--prompt-name` content. The
    /// insertion point is `messages[1]` when `messages[0]` is a
    /// system message (the common case after `ensure_system_prompt`
    /// has run); otherwise we prepend at the very front.
    ///
    /// Cheap (one `Vec::insert`) and safe to call multiple times.
    /// Callers that need to inject several messages should batch
    /// them into a single call rather than calling this in a loop.
    pub fn insert_after_system_prompt(&mut self, msg: Message) {
        let pos = match self.messages.first() {
            Some(m) if m.role == Role::System => 1,
            _ => 0,
        };
        self.messages.insert(pos, msg);
    }

    /// Internal: dispatch a `TraceEvent` to the installed sink, or
    /// fall back to the historical `println!` path when no sink is
    /// configured and `config.verbose` is on.
    ///
    /// The dual path is intentional: existing tests / host binaries
    /// that don't install a sink continue to see the same stdout
    /// output they always did, and the new sink-based path is
    /// purely additive.
    fn emit_trace(&self, event: TraceEvent) {
        if let Some(sink) = self.trace_sink() {
            // Deliver to every installed sink. `with_sinks` returns
            // `None` when the list is empty, in which case we fall
            // through to the legacy `println!` path below.
            if sink.with_sinks(|sinks| {
                for s in sinks.iter_mut() {
                    s.event(event.clone());
                }
            })
            .is_some()
            {
                return;
            }
        }
        // No sink installed → reproduce the historical `println!`
        // behaviour so un-migrated callers don't notice the change.
        if !self.config.verbose {
            return;
        }
        match event {
            TraceEvent::RunStart { task } => {
                println!();
                println!("{}", "=".repeat(60));
                println!("mAgent Starting Task: {}", task);
                println!("{}", "=".repeat(60));
            }
            TraceEvent::BackendReady {
                provider,
                using_real_llm,
            } => {
                if using_real_llm {
                    println!("[Agent] Connected to {} - using real AI reasoning", provider);
                } else {
                    println!("[Agent] LLM backend not available - using simulated reasoning");
                }
            }
            TraceEvent::BudgetExhausted { kind, limit } => {
                println!("[Agent] Max {} ({}) reached", kind, limit);
            }
            TraceEvent::ThinkingStart {
                iteration,
                tool_calls,
            } => {
                println!(
                    "\n[Thinking] Iteration {} (tool_calls: {})",
                    iteration, tool_calls
                );
            }
            TraceEvent::CompressionApplied {
                kept,
                dropped,
                tool_results_truncated,
                bytes_saved,
            } => {
                if dropped > 0 || tool_results_truncated > 0 {
                    println!(
                        "[Compress] kept={} dropped={} tools_truncated={} bytes_saved≈{}",
                        kept, dropped, tool_results_truncated, bytes_saved
                    );
                }
            }
            TraceEvent::LlmResponse { body } => {
                let display = if body.len() > 200 { &body[..200] } else { &body };
                println!("[LLM Response] {}", display);
            }
            TraceEvent::ToolCallStart { name, arguments } => {
                println!("[Action] Calling tool: {} with {}", name, arguments);
            }
            TraceEvent::ToolCallEnd {
                name,
                result,
                success,
            } => {
                if success {
                    println!("[Tool Result] {}: {}", name, result);
                } else {
                    println!("[Tool Error] {}: {}", name, result);
                }
            }
            TraceEvent::FinalResult { body } => {
                println!("[Result] {}", body);
            }
            TraceEvent::ObservingNoAction => {
                println!("[Observing] No tool call or result, continuing...");
            }
            TraceEvent::Observing => {
                println!("[Observing] Processing result...");
            }
        }
    }

    /// Mutable access to the configured LLM backend, if any. Use this
    /// to tweak e.g. the model name on the default Ollama backend
    /// without replacing the whole thing.
    pub fn backend_mut(&mut self) -> Option<&mut dyn LlmBackend> {
        self.backend.as_deref_mut()
    }

    /// Swap in a different LLM backend (e.g. a `DeepSeekClient`).
    pub fn set_backend<B: LlmBackend + 'static>(&mut self, backend: B) {
        self.backend = Some(Box::new(backend));
        // Reset the auto-enable flag so the next run() probes the new
        // backend instead of silently skipping LLM calls because the
        // old probe already succeeded.
        self.backend_enabled = false;
    }

    /// Backwards-compatible accessor for the Ollama backend, if it's
    /// still the one wired up. Returns `None` for any other provider.
    pub fn ollama_mut(&mut self) -> Option<&mut OllamaClient> {
        // We downcast through Any. If the backend is an `OllamaClient`
        // we expose it directly; for DeepSeek or anything else we hide
        // it so callers that mutate Ollama-specific state don't
        // accidentally poke the wrong backend.
        match self.backend.as_deref_mut() {
            Some(boxed) => {
                let any_ref: &mut dyn std::any::Any = boxed;
                any_ref.downcast_mut::<OllamaClient>()
            }
            None => None,
        }
    }

    /// Replace the Ollama backend in one step. Convenience for the
    /// historical `set_ollama(client)` API; new code should prefer
    /// [`Self::set_backend`].
    pub fn set_ollama(&mut self, client: OllamaClient) {
        self.set_backend(client);
    }

    /// Reports whether the runner is currently routing chat traffic
    /// through an LLM backend (as opposed to a pure simulation).
    pub fn using_backend(&self) -> bool {
        self.backend_enabled && self.backend.is_some()
    }

    /// Force-enable the wired-up backend, skipping the auto-probe.
    /// Useful for CLI subcommands that have already probed the
    /// backend directly and just want to tell the runner "trust me,
    /// it's reachable, use it on the next `run()` call".
    ///
    /// No-op if no backend is wired up.
    ///
    /// The "force_" prefix makes it obvious this bypasses the normal
    /// reachability check inside `run()` — callers take responsibility
    /// for having probed the backend themselves.
    pub fn force_enable_backend(&mut self) {
        if self.backend.is_some() {
            self.backend_enabled = true;
        }
    }

    /// Force-disable the backend. The runner will fall back to
    /// simulated responses for the rest of this run. The next
    /// `run()` call will re-probe (if `probe_ollama_on_run` is true).
    pub fn force_disable_backend(&mut self) {
        self.backend_enabled = false;
    }

    /// Reports whether the wired-up backend is Ollama specifically.
    /// Kept for backwards compatibility; prefer
    /// [`Self::backend_provider`].
    pub fn using_ollama(&self) -> bool {
        self.using_backend()
            && matches!(
                self.backend.as_ref().map(|b| b.provider()),
                Some(LlmProvider::Ollama)
            )
    }

    /// Provider name of the wired-up backend, if any.
    /// Provider name of the wired-up backend, if any.
    pub fn backend_provider(&self) -> Option<LlmProvider> {
        self.backend.as_ref().map(|b| b.provider())
    }

    /// Run the ReAct loop on `task` until the assistant emits a
    /// terminal result, a budget is exhausted, or an unrecoverable
    /// error occurs. Returns the assistant's terminal payload.
    pub fn run(&mut self, task: &str) -> std::result::Result<String, String> {
        self.reset();

        self.emit_trace(TraceEvent::RunStart {
            task: task.to_string(),
        });

        // Probe the LLM backend — only on the first run, or when
        // explicitly requested. After we know it is reachable we keep
        // using it; if it isn't we stick with simulated responses.
        if self.config.probe_ollama_on_run && !self.backend_enabled {
            if let Some(ref backend) = self.backend {
                if backend.check_connection() {
                    self.backend_enabled = true;
                }
            }
        }
        let provider = self
            .backend
            .as_ref()
            .map(|b| b.provider())
            .unwrap_or(LlmProvider::Custom);
        let provider_label = if self.backend_enabled {
            provider.to_string()
        } else {
            "none".to_string()
        };
        self.emit_trace(TraceEvent::BackendReady {
            provider: provider_label,
            using_real_llm: self.backend_enabled,
        });

        // Seed the conversation with the user task. The system prompt
        // is *not* sent separately any more — v3 persists it as the
        // first message of `self.messages` (see `think()`), so
        // `runner.messages()` returns a faithful snapshot of what
        // the LLM will see and compression can keep it across
        // `slice_messages()` calls. We insert the system prompt
        // here, ahead of the user task, so the very first LLM call
        // already has the correct head.
        let system_prompt = self.build_system_prompt();
        self.messages.push(Message::system(&system_prompt));
        self.messages.push(Message::user(task));
        self.state = AgentState::Thinking;

// Main ReAct loop.
//
// v2: `iteration` counts LLM invocations (the only expensive step), not
// raw loop iterations. State transitions (Executing -> Observing -> Thinking)
// are free and don't consume budget. `max_tool_calls` still bounds the number
// of tool executions.
while self.state != AgentState::Finished && self.state != AgentState::Error {
    if self.iteration >= self.config.max_iterations {
        self.emit_trace(TraceEvent::BudgetExhausted {
            kind: "iterations",
            limit: self.config.max_iterations,
        });
        break;
    }

    if self.tool_call_count >= self.config.max_tool_calls {
        self.emit_trace(TraceEvent::BudgetExhausted {
            kind: "tool_calls",
            limit: self.config.max_tool_calls,
        });
        break;
    }

    match self.state {
        AgentState::Thinking => {
            self.iteration += 1;
            self.think()?;
        }
        AgentState::Executing => {
            // Tool was just executed; ready to observe the result.
            self.state = AgentState::Observing;
        }
        AgentState::Observing => {
            self.observe();
        }
        _ => break,
    }
}

        self.get_result()
    }

    fn think(&mut self) -> std::result::Result<(), String> {
        self.emit_trace(TraceEvent::ThinkingStart {
            iteration: self.iteration,
            tool_calls: self.tool_call_count,
        });

        // Compress the conversation history before handing it to the
        // LLM. This is intentionally unconditional: the cost is
        // microseconds for the slicing step and a few hundred bytes
        // for the truncation step, but the savings are quadratic in
        // the number of iterations and the call-site can't easily
        // decide "should I compress this time?" on its own.
        if self.config.compression.max_messages > 0
            || self.config.compression.tool_content_max_chars > 0
        {
            let stats = crate::conversation::compress_messages(
                &mut self.messages,
                &self.config.compression,
            );
            self.emit_trace(TraceEvent::CompressionApplied {
                kept: stats.kept,
                dropped: stats.dropped,
                tool_results_truncated: stats.tool_results_truncated,
                bytes_saved: stats.bytes_saved,
            });
            // Persist the snapshot so the CLI's `--save-summary` path
            // can read it without re-running compression.
            self.last_compression_stats = Some(stats);
            self.last_compression_policy = Some((&self.config.compression).into());
        }

        // Build the effective system prompt BEFORE we enter the
        // mutable-borrow scope on `self.backend`. The path through
        // LlmBackend is `&mut self`, so any extra `&self` calls
        // inside that block refuse to compile.
        let system_prompt = self.build_system_prompt();

        // Persist the system prompt as the first message of the
        // conversation. v3 decision: the prompt is cheap to re-send
        // every iteration (it's a fixed ~1–2k chars), and keeping it
        // in `self.messages` makes the entire pipeline auditable —
        // `runner.messages()` returns a faithful snapshot of what the
        // LLM sees, compression has a chance to keep it across
        // `slice_messages()` calls, and `approx_total_tokens()` is
        // accurate.
        //
        // We only push if the head isn't already a matching system
        // message, so consecutive `think()` calls don't accumulate
        // duplicates.
        self.ensure_system_prompt(&system_prompt);

        let response = if self.backend_enabled {
            if let Some(ref mut backend) = self.backend {
                // The system prompt is already at index 0 (just
                // pushed by `ensure_system_prompt`), so we can call
                // the backend directly without transient insert /
                // remove. This also makes the conversation history
                // visible to the compression pipeline that ran
                // earlier in `think()`.
                match backend.chat_with_messages(&self.messages, self.config.sampling) {
                    Ok(r) => r,
                    Err(e) => {
                        // The backend is unreachable. Push a fallback
                        // hint at the tail so the next iteration (and
                        // the user reading the trace) knows what
                        // happened. The system prompt at the head
                        // stays put — `ensure_system_prompt` will
                        // reconcile any drift on the next `think()`.
                        log::warn!(target: "magent.llm", "backend error: {}", e);
                        self.messages.push(Message::system(&format!(
                            "[fallback] LLM unreachable: {}",
                            e
                        )));
                        self.generate_simulated_response()
                    }
                }
            } else {
                self.generate_simulated_response()
            }
        } else {
            self.generate_simulated_response()
        };

        self.emit_trace(TraceEvent::LlmResponse {
            body: response.clone(),
        });

        // Record the assistant message. If it contains a tool call, store
        // it as a structured tool_call (no need to also keep `content`).
        if let Some((name, args)) = self.parse_tool_call(&response) {
            let tc = ToolCall { name: name.clone(), arguments: args.clone() };
            // Allocate the call id up-front and reuse it for BOTH the
            // assistant tool-call message and the tool-result message.
            // DeepSeek rejects the request if these ids differ.
            self.tool_call_count += 1;
            let call_id = format!("call_{}", self.tool_call_count);
            self.messages
                .push(Message::assistant_tool_call_id(tc.clone(), &call_id));
            self.emit_trace(TraceEvent::ToolCallStart {
                name: name.clone(),
                arguments: serde_json::to_string(&tc.arguments)
                    .unwrap_or_else(|_| "{}".to_string()),
            });
            let tool_result = self.execute_tool(&name, &tc.arguments);
            let (result_content, success) = match tool_result {
                Ok(content) => (content, true),
                Err(e) => {
                    log::warn!(target: "magent.tool", "tool '{}' failed: {}", name, e);
                    (format!("Error: {}", e), false)
                }
            };
            self.emit_trace(TraceEvent::ToolCallEnd {
                name: name.clone(),
                result: result_content.clone(),
                success,
            });
            self.messages.push(Message::tool(&call_id, &result_content));
            if success {
                self.state = AgentState::Executing;
            } else {
                // Surface the error but keep iterating so the agent can
                // recover with another tool call.
                self.state = AgentState::Observing;
            }
        } else if let Some(result) = self.parse_result(&response) {
            self.emit_trace(TraceEvent::FinalResult {
                body: result.clone(),
            });
            // Replace the assistant content with the cleaned result so
            // downstream LLM calls don't get markdown noise.
            self.messages.push(Message::assistant_text(&result));
            self.state = AgentState::Finished;
        } else {
            // Neither tool call nor a result. Treat this as plain assistant
            // chatter and observe (the LLM is usually explaining its plan).
            self.messages.push(Message::assistant_text(&response));
            self.emit_trace(TraceEvent::ObservingNoAction);
            self.state = AgentState::Observing;
        }

        Ok(())
    }

    fn observe(&mut self) {
        self.emit_trace(TraceEvent::Observing);
        self.state = AgentState::Thinking;
    }

    /// Execute a tool. v2: passes `args` as a serialised JSON string without
    /// allocating an intermediate `HashMap<String, String>`.
    fn execute_tool(
        &mut self,
        name: &str,
        args: &HashMap<String, serde_json::Value>,
    ) -> std::result::Result<String, String> {
        let args_str = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
        self.executor.execute(name, &args_str)
    }

    fn generate_simulated_response(&self) -> String {
        let user_task_lower = self.first_user_message().to_lowercase();
        let tool_count = self.tool_call_count;

        // Note: order matters because some tasks match multiple keywords.
        // v2: more conservative keyword matching; less nested branches.
        if contains_any(&user_task_lower, &["monitor", "environment", "all sensor"]) {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "read_sensor", "args": {"sensor": "humidity"}}"#.to_string(),
                2 => r#"{"tool": "read_sensor", "args": {"sensor": "pressure"}}"#.to_string(),
                3 => r#"{"tool": "ble_send", "args": {"data": "Environmental data logged"}}"#.to_string(),
                _ => r#"{"result": "Environmental monitoring complete"}"#.to_string(),
            };
        }

        if contains_any(&user_task_lower, &["read"]) && contains_any(&user_task_lower, &["sensor", "vital"])
            && contains_any(&user_task_lower, &["log", "flash", "save", "morning"])
        {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "read_sensor", "args": {"sensor": "accelerometer"}}"#.to_string(),
                2 => r#"{"tool": "flash_write", "args": {"address": 1024, "data": "Vital signs logged"}}"#.to_string(),
                3 => r#"{"tool": "ble_send", "args": {"data": "Good morning notification"}}"#.to_string(),
                _ => r#"{"result": "Morning monitoring complete, data logged and notification sent"}"#.to_string(),
            };
        }

        if (contains_any(&user_task_lower, &["temperature", "check temperature"]))
            && (contains_any(&user_task_lower, &["above"]) || contains_any(&user_task_lower, &["fan", "cooling"]))
        {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "write_gpio", "args": {"pin": 14, "state": "high"}}"#.to_string(),
                _ => r#"{"result": "Temperature was above threshold, fan turned on at GPIO pin 14"}"#.to_string(),
            };
        }

        if contains_any(&user_task_lower, &["humidity"]) && !contains_any(&user_task_lower, &["temperature", "pressure"]) {
            return tool_then_done(tool_count, "read_sensor", r#"{"sensor": "humidity"}"#, "Humidity sensor reading completed");
        }
        if contains_any(&user_task_lower, &["pressure"]) && !contains_any(&user_task_lower, &["humidity", "temperature"]) {
            return tool_then_done(tool_count, "read_sensor", r#"{"sensor": "pressure"}"#, "Pressure sensor reading completed");
        }
        if contains_any(&user_task_lower, &["temperature"]) && !contains_any(&user_task_lower, &["humidity", "pressure"]) {
            return tool_then_done(tool_count, "read_sensor", r#"{"sensor": "temperature"}"#, "Temperature sensor reading completed");
        }

        if contains_any(&user_task_lower, &["accelerometer", "step", "motion"]) {
            return tool_then_done(tool_count, "read_sensor", r#"{"sensor": "accelerometer"}"#, "Accelerometer reading completed");
        }

        if contains_any(&user_task_lower, &["led"]) && contains_any(&user_task_lower, &["on", "turn on", "enable"]) {
            return tool_then_done(
                tool_count,
                "write_gpio",
                r#"{"pin": 13, "state": "high"}"#,
                "LED turned on successfully",
            );
        }
        if contains_any(&user_task_lower, &["led"]) && contains_any(&user_task_lower, &["off", "turn off", "disable"]) {
            return tool_then_done(
                tool_count,
                "write_gpio",
                r#"{"pin": 13, "state": "low"}"#,
                "LED turned off successfully",
            );
        }

        if contains_any(&user_task_lower, &["ble"])
            && contains_any(&user_task_lower, &["notification", "alert", "send", "message"])
        {
            return tool_then_done(
                tool_count,
                "ble_send",
                r#"{"data": "Alert from mAgent"}"#,
                "BLE notification sent successfully",
            );
        }

        if contains_any(&user_task_lower, &["flash"]) && contains_any(&user_task_lower, &["log", "save", "write"]) {
            return tool_then_done(
                tool_count,
                "flash_write",
                r#"{"address": 1024, "data": "Log entry"}"#,
                "Data logged to flash memory",
            );
        }
        if contains_any(&user_task_lower, &["flash"]) && contains_any(&user_task_lower, &["read"]) {
            return tool_then_done(
                tool_count,
                "flash_read",
                r#"{"address": 1024}"#,
                "Flash data read successfully",
            );
        }

        if contains_any(&user_task_lower, &["health", "status", "verify", "system"]) {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "ble_send", "args": {"data": "System health OK"}}"#.to_string(),
                _ => r#"{"result": "Health check complete - all systems operational"}"#.to_string(),
            };
        }

        // Default
        if tool_count == 0 {
            return r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string();
        }
        r#"{"result": "Task completed successfully"}"#.to_string()
    }

    /// v2: parse any JSON key that maps to a tool name, not just the
    /// hard-coded 5. Falls back to looking up `tool_calls[0].function.name`
    /// (OpenAI-style) as well.
    #[doc(hidden)]
    pub fn parse_tool_call(
        &self,
        response: &str,
    ) -> Option<(String, HashMap<String, serde_json::Value>)> {
        let cleaned = strip_code_fence(response);

        // 1. Whole response is a JSON object that describes a tool call.
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&cleaned) {
            if let Some(found) = parse_tool_call_from_json(&json) {
                return Some(found);
            }
        }

        // 2. The model wrapped the tool call in prose/markdown (a common
        //    format violation). Recover the first balanced JSON object and
        //    re-parse it.
        if let Some(obj) = find_json_object(&cleaned) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(obj) {
                if let Some(found) = parse_tool_call_from_json(&json) {
                    return Some(found);
                }
            }
        }

        // 3. Anthropic-style tool use. Some providers (and some model
        //    checkpoints) emit `<invoke name="X"><parameter name="k">v</
        //    parameter></invoke>` (optionally namespace-prefixed as
        //    `<antml:invoke>`) instead of our JSON shape.
        parse_anthropic_tool_call(&cleaned)
    }

    #[doc(hidden)]
    pub fn parse_result(&self, response: &str) -> Option<String> {
        // 1. Strict OpenAI-style {"result": ...}.
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(result) = json.get("result") {
                return Some(result_value_to_string(result));
            }
        }

        // 2. A fenced block (code or JSON): the inner content IS the answer.
        //    This is how a model that's asked to write code (but forgets the
        //    {"result":...} envelope) usually responds.
        if response.contains("```") {
            let cleaned = strip_code_fence(response);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }

        // 3. A {"result": ...} object embedded anywhere in prose/markdown.
        if let Some(obj) = find_json_object(response) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
                if let Some(result) = v.get("result") {
                    return Some(result_value_to_string(result));
                }
            }
        }

        // 4. Substantial plain-text final answer. `parse_tool_call` already
        //    ran and found no tool call, so this is either a prose answer or
        //    mid-plan chatter. We only treat reasonably long prose that
        //    doesn't read like a plan as a final result, so we don't
        //    terminate the loop while the model is still explaining a
        //    multi-step approach.
        let t = response.trim();
        if !t.is_empty()
            && !t.starts_with('{')
            && !t.starts_with('[')
            && t.len() >= MIN_PROSE_RESULT_LEN
            && !looks_like_chatter(t)
        {
            return Some(t.to_string());
        }

        None
    }

    fn first_user_message(&self) -> String {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    /// Make sure `self.messages` starts with the system prompt
    /// `prompt`. Used by `think()` on every iteration to reconcile
    /// the head of the conversation history:
    ///
    /// * If the head is already a system message with the same
    ///   content, this is a no-op.
    /// * If the head is a system message with **different** content
    ///   (the user changed `RunnerConfig::system_prompt` mid-run,
    ///   which `set_system_prompt` handles), we replace it in place
    ///   instead of appending — otherwise we'd accumulate a chain
    ///   of stale prompts.
    /// * If the head isn't a system message at all, we insert at
    ///   index 0.
    ///
    /// Always cheap (one allocation at worst) and idempotent.
    fn ensure_system_prompt(&mut self, prompt: &str) {
        match self.messages.first() {
            Some(m) if m.role == Role::System && m.content == prompt => {
                // Already correct.
            }
            Some(m) if m.role == Role::System => {
                // Stale prompt (different content) — replace it
                // in place so the user's mid-run edit takes effect
                // without leaking the old text into the history.
                self.messages[0] = Message::system(prompt);
            }
            _ => {
                self.messages.insert(0, Message::system(prompt));
            }
        }
    }

    /// Update the system prompt at runtime. Triggers a reconciliation
    /// on the next `think()` call (see `ensure_system_prompt`).
    pub fn set_system_prompt(&mut self, prompt: String) {
        self.config.system_prompt = prompt;
    }

    /// Reset the runner's per-run state (state machine, conversation,
    /// iteration / tool-call counters). Does **not** touch the
    /// runner configuration.
    fn reset(&mut self) {
        self.state = AgentState::Idle;
        self.messages.clear();
        self.iteration = 0;
        self.tool_call_count = 0;
    }

    /// Reset the conversation history while keeping the system prompt.
    /// Useful for REPL mode when user wants to start fresh but keep config.
    pub fn reset_conversation(&mut self) {
        self.state = AgentState::Idle;
        self.messages.retain(|m| m.role == Role::System);
        self.iteration = 0;
        self.tool_call_count = 0;
    }

    /// Current ReAct-loop state.
    pub fn state(&self) -> AgentState {
        self.state
    }

    /// Number of iterations executed so far in the current run.
    pub fn iteration(&self) -> usize {
        self.iteration
    }

    /// Total tool calls dispatched so far in the current run.
    pub fn tool_call_count(&self) -> usize {
        self.tool_call_count
    }

    /// Borrow the current conversation history.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Estimated token count of the current conversation history.
    /// Uses the same 4-chars-per-token heuristic as
    /// `conversation::approx_total_tokens` so the number is consistent
    /// with what the LLM will see.
    pub fn approx_total_tokens(&self) -> usize {
        crate::conversation::approx_total_tokens(&self.messages)
    }

    /// Apply the configured `CompressionPolicy` to the live
    /// conversation history and return the resulting counters.
    /// Exposed for tests and CLI tooling that want to inspect / report
    /// the compression stats without running the full ReAct loop.
    pub fn compress_now(&mut self) -> crate::conversation::CompressionStats {
        crate::conversation::compress_messages(&mut self.messages, &self.config.compression)
    }

    /// Immutable view of the runner configuration.
    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// Mutable access to the runner configuration — useful for
    /// hot-swapping the trace sink or compression policy between runs.
    pub fn config_mut(&mut self) -> &mut RunnerConfig {
        &mut self.config
    }

    /// Snapshot of the [`CompressionStats`] from the most recent
    /// call to [`Self::compress_now`] (i.e. from the last ReAct
    /// iteration). Returns `None` if no LLM call has completed yet.
    ///
    /// The stats are recorded **after** compression runs on each
    /// iteration, so a run that compressed zero times will also
    /// return `None`. Call [`Self::compress_now`] explicitly
    /// before `run_summary_after_run` to force a snapshot.
    pub fn last_compression_stats(&self) -> Option<crate::conversation::CompressionStats> {
        self.last_compression_stats
    }

    /// Snapshot of the [`CompressionPolicy`] that produced
    /// [`Self::last_compression_stats`]. Returns `None` when
    /// no compression has run yet. The snapshot is taken verbatim
    /// from `self.config.compression` so the caller can tell whether
    /// the window was generated with aggressive or conservative limits.
    pub fn last_compression_policy(&self) -> Option<crate::summary::CompressionPolicySnapshot> {
        self.last_compression_policy
    }

    fn get_result(&self) -> std::result::Result<String, String> {
        for msg in self.messages.iter().rev() {
            if msg.role == Role::Assistant && !msg.content.is_empty() {
                if let Some(result) = self.parse_result(&msg.content) {
                    return Ok(result);
                }
                // A fenced (code) answer surfaces cleanly even when
                // parse_result doesn't classify it as a terminal result.
                return Ok(strip_code_fence(&msg.content));
            }
        }
        Ok("Task completed".to_string())
    }
}

#[inline]
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Pattern: on the first call, return a tool invocation with the given
/// `args` JSON object (without surrounding braces); on subsequent calls,
/// return a done result.
#[inline]
fn tool_then_done(tool_count: usize, tool_name: &str, args_json: &str, done_msg: &str) -> String {
    if tool_count == 0 {
        format!(r#"{{"tool": "{}", "args": {}}}"#, tool_name, args_json)
    } else {
        format!(r#"{{"result": "{}"}}"#, done_msg)
    }
}

// ============================================================================
// DeepSeekClient unit tests
// ============================================================================
//
// These don't talk to the network — they only exercise the on-host
// pieces (JSON body builder, base-URL trimming, set_model, the trait
// surface). End-to-end behaviour against the real DeepSeek API is
// covered by manual smoke tests documented in `docs/`.

#[cfg(test)]
mod deepseek_client_tests {
    use super::*;

    #[test]
    fn strips_trailing_slash_from_base_url() {
        let c = DeepSeekClient::with_endpoint(
            "https://api.deepseek.com/v1/",
            "deepseek-chat",
            "test-key",
        );
        assert_eq!(c.base_url(), "https://api.deepseek.com/v1");
        assert_eq!(c.model(), "deepseek-chat");
    }

    #[test]
    fn default_constructor_uses_public_api() {
        let c = DeepSeekClient::new("test-key");
        assert_eq!(c.base_url(), "https://api.deepseek.com/v1");
        assert_eq!(c.model(), "deepseek-chat");
    }

    #[test]
    fn set_model_replaces_model_name() {
        let mut c = DeepSeekClient::new("test-key");
        c.set_model("deepseek-coder");
        assert_eq!(c.model(), "deepseek-coder");
    }

    #[test]
    fn provider_reports_deepseek() {
        let c = DeepSeekClient::new("test-key");
        assert_eq!(LlmBackend::provider(&c), LlmProvider::DeepSeek);
    }

    #[test]
    fn trait_object_can_be_boxed() {
        // Compile-time check that `Box<dyn LlmBackend>` accepts a
        // DeepSeekClient (this is what `RealAgentRunner::set_backend`
        // does at runtime).
        let c: Box<dyn LlmBackend> = Box::new(DeepSeekClient::new("test-key"));
        assert_eq!(c.provider(), LlmProvider::DeepSeek);
    }

    #[test]
    fn write_chat_body_produces_openai_compatible_shape() {
        // We can't intercept the outgoing HTTP request without a mock
        // server, so instead we re-implement the same JSON body
        // building by hand and compare the result. This catches
        // regressions in escaping / field order.
        let mut c = DeepSeekClient::new("test-key");
        let msgs = vec![
            Message::system("You are a helpful agent"),
            Message::user("Read the temperature"),
        ];
        let body = c
            .write_chat_body_for_test(&msgs, SamplingParams::default())
            .expect("write body");
        let body_str = std::str::from_utf8(&body).expect("utf8");
        // Must be valid JSON we can re-parse.
        let parsed: serde_json::Value =
            serde_json::from_str(body_str).expect("parse generated JSON");
        assert_eq!(parsed["model"], "deepseek-chat");
        assert_eq!(parsed["stream"], false);
        // Thinking mode is disabled so multi-turn (tool) conversations
        // don't trip DeepSeek's "reasoning_content must be passed back"
        // requirement.
        assert_eq!(parsed["thinking"]["type"], "disabled");
        assert!(parsed["messages"].is_array());
        assert_eq!(parsed["messages"][0]["role"], "system");
        assert_eq!(parsed["messages"][1]["role"], "user");
        assert_eq!(
            parsed["messages"][1]["content"],
            "Read the temperature"
        );
    }

    // ---- New tests for the v2 (audited) implementation ----

    #[test]
    fn try_new_rejects_empty_key() {
        assert!(DeepSeekClient::try_new("").is_none());
        assert!(DeepSeekClient::try_new("   ").is_none());
        // But accepts a non-empty key.
        assert!(DeepSeekClient::try_new("sk-abc").is_some());
    }

    #[test]
    fn try_with_endpoint_rejects_empty_key() {
        assert!(DeepSeekClient::try_with_endpoint(
            "https://x/v1", "m", ""
        )
        .is_none());
        assert!(DeepSeekClient::try_with_endpoint(
            "https://x/v1", "m", "\t\n"
        )
        .is_none());
        assert!(DeepSeekClient::try_with_endpoint(
            "https://x/v1", "m", "sk-abc"
        )
        .is_some());
    }

    #[test]
    fn new_panics_on_empty_key() {
        // `new` is the infallible constructor — it must reject empty
        // keys loudly instead of silently building a useless client.
        let result = std::panic::catch_unwind(|| DeepSeekClient::new(""));
        assert!(result.is_err(), "new(\"\") should panic");
    }

    #[test]
    fn check_connection_returns_false_on_empty_key() {
        // Build with the low-level `with_endpoint` (which doesn't
        // validate) and confirm the high-level probe refuses to
        // call out.
        let c = DeepSeekClient::with_endpoint(
            "http://127.0.0.1:1",
            "deepseek-chat",
            "",
        );
        assert!(!c.check_connection());
    }

    #[test]
    fn debug_redacts_api_key() {
        let c = DeepSeekClient::new("sk-should-not-leak");
        let dbg = format!("{:?}", c);
        assert!(
            !dbg.contains("sk-should-not-leak"),
            "Debug output leaked the API key: {}",
            dbg
        );
        assert!(dbg.contains("<redacted>"));
        // Other fields are visible for diagnostics.
        assert!(dbg.contains("deepseek-chat"));
        assert!(dbg.contains("https://api.deepseek.com/v1"));
    }

    #[test]
    fn debug_redacts_key_even_when_unusual_chars() {
        // Make sure the redacted wrapper doesn't accidentally leak
        // keys that contain special characters (quotes, backslashes).
        let c = DeepSeekClient::new("\"weird\\key\"");
        let dbg = format!("{:?}", c);
        assert!(!dbg.contains("weird"));
    }

    #[test]
    fn assistant_tool_call_message_round_trips() {
        // A ReAct-loop assistant message that decided to call a
        // tool must serialise with `content: null` and a
        // `tool_calls` array. We can't deserialise the call_id back
        // out without our own parser, but we can assert the shape
        // round-trips as JSON.
        let mut c = DeepSeekClient::new("k");
        let tc = ToolCall {
            name: "read_sensor".to_string(),
            arguments: serde_json::from_value(serde_json::json!({"sensor": "temperature"})).unwrap(),
        };
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_call: Some(tc),
            tool_call_id: Some("call_42".to_string()),
        };
        let body = c
            .write_chat_body_for_test(&[msg], SamplingParams::default())
            .expect("write body");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("parse JSON");
        assert_eq!(parsed["messages"][0]["role"], "assistant");
        assert!(parsed["messages"][0]["content"].is_null());
        let tool_calls = parsed["messages"][0]["tool_calls"]
            .as_array()
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_42");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(
            tool_calls[0]["function"]["name"],
            "read_sensor"
        );
    }

    #[test]
    fn tool_result_message_carries_tool_call_id() {
        // Tool-result messages MUST have `role: "tool"` and
        // `tool_call_id` set; otherwise DeepSeek rejects the turn.
        let mut c = DeepSeekClient::new("k");
        let msg = Message {
            role: Role::Tool,
            content: "23.5".to_string(),
            tool_call: None,
            tool_call_id: Some("call_42".to_string()),
        };
        let body = c
            .write_chat_body_for_test(&[msg], SamplingParams::default())
            .expect("write body");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("parse JSON");
        assert_eq!(parsed["messages"][0]["role"], "tool");
        assert_eq!(parsed["messages"][0]["content"], "23.5");
        assert_eq!(parsed["messages"][0]["tool_call_id"], "call_42");
    }

    #[test]
    fn plain_message_has_no_tool_call_id_field() {
        // A plain user message must NOT carry a stray `tool_call_id`
        // (we never set one in `Message::user`, but assert the wire
        // format is clean anyway).
        let mut c = DeepSeekClient::new("k");
        let body = c
            .write_chat_body_for_test(
                &[Message::user("hi")],
                SamplingParams::default(),
            )
            .expect("write body");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("parse JSON");
        assert!(parsed["messages"][0].get("tool_call_id").is_none());
    }

    #[test]
    fn content_with_special_chars_is_escaped() {
        // Quotes, backslashes, newlines, control chars — all must
        // be JSON-escaped so we don't break the wire format.
        let mut c = DeepSeekClient::new("k");
        let body = c
            .write_chat_body_for_test(
                &[Message::user(r#"she said "hi"\nnew line"#)],
                SamplingParams::default(),
            )
            .expect("write body");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("parse JSON");
        assert_eq!(
            parsed["messages"][0]["content"],
            r#"she said "hi"\nnew line"#
        );
    }

    #[test]
    fn api_key_accessor_returns_trimmed() {
        // `with_endpoint` stores the raw value, but the public
        // accessor surfaces it unchanged. `try_with_endpoint` (used
        // by the CLI) trims first, so callers that pass
        // `"  sk-...  "` get the trimmed key.
        let raw = DeepSeekClient::with_endpoint(
            "https://x/v1", "m", "  sk-abc  "
        );
        assert_eq!(raw.api_key(), "  sk-abc  ");
        let trimmed =
            DeepSeekClient::try_with_endpoint("https://x/v1", "m", "  sk-abc  ")
                .expect("non-empty");
        assert_eq!(trimmed.api_key(), "sk-abc");
    }

    // ---- URL trimming parity tests ----

    #[test]
    fn ollama_client_trims_trailing_slashes() {
        // The CLI lets users pass `--ollama http://gpu:11434/` with a
        // trailing slash. Without trimming, we'd build URLs like
        // `http://gpu:11434//api/chat` and the server would 404.
        let c = OllamaClient::new("http://gpu:11434/", "llama3.2");
        assert_eq!(c.base_url(), "http://gpu:11434");
    }

    #[test]
    fn ollama_client_trims_multiple_trailing_slashes() {
        let c = OllamaClient::new("http://gpu:11434////", "llama3.2");
        assert_eq!(c.base_url(), "http://gpu:11434");
    }

    #[test]
    fn ollama_client_keeps_no_trailing_slash_unchanged() {
        // Regression: don't accidentally trim too aggressively.
        let c = OllamaClient::new("http://gpu:11434", "llama3.2");
        assert_eq!(c.base_url(), "http://gpu:11434");
    }

    #[test]
    fn tool_call_id_is_escaped_in_ollama_wire_format() {
        // Regression: an early version of `write_message_json` wrote
        // the tool_call_id raw, which would break on ids containing
        // quotes / backslashes. Make sure that never regresses.
        let _c = OllamaClient::new("http://x", "m"); // existence check
        // We can't intercept the outbound HTTP request here, so we
        // round-trip via the same JSON encoder Ollama uses.
        let id = r#"call_"weird\\id""#;
        let msg = Message {
            role: Role::Tool,
            content: "23.5".to_string(),
            tool_call: None,
            tool_call_id: Some(id.to_string()),
        };
        // The Ollama path doesn't expose the body encoder, so build
        // it via the same free function DeepSeek uses.
        let mut buf = Vec::new();
        write_message_json(&mut buf, &msg).expect("write");
        let parsed: serde_json::Value =
            serde_json::from_slice(&buf).expect("parse JSON");
        assert_eq!(parsed["tool_call_id"], id);
    }
}

// ============================================================================
// Conversation compression integration tests
// ============================================================================
//
// These exercise the `compression` field on `RunnerConfig` that the
// `magent` CLI plumbs through `--max-messages` / `--tool-max-chars`.
// Behaviour is verified against a `RealAgentRunner<SimulatorExecutor>` so
// the defaults match the host smoke-test that the CLI runs.

#[cfg(test)]
mod compression_tests {
    use super::*;
    use crate::conversation::CompressionPolicy;
    use crate::real_tools::SimulatorExecutor;

    fn runner_with_policy(policy: CompressionPolicy) -> RealAgentRunner<SimulatorExecutor> {
        let mut config = RunnerConfig::default();
        config.probe_ollama_on_run = false;
        config.compression = policy;
        RealAgentRunner::with_config(SimulatorExecutor::new(), config)
    }

    #[test]
    fn default_policy_is_conservative() {
        let config = RunnerConfig::default();
        assert_eq!(config.compression.max_messages, 32);
        assert_eq!(config.compression.tool_content_max_chars, 800);
    }

    #[test]
    fn disabled_policy_skips_compression() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        // Push a long tool result and many messages.
        runner.messages.push(Message::user("hello"));
        runner.messages.push(Message::tool("c1", &"x".repeat(5_000)));
        for i in 0..50 {
            runner.messages.push(Message::assistant_text(&format!("a{}", i)));
        }
        let stats = runner.compress_now();
        assert_eq!(stats.dropped, 0, "disabled policy must not drop");
        assert_eq!(stats.tool_results_truncated, 0, "disabled policy must not truncate");
        assert_eq!(runner.messages().len(), 52);
    }

    #[test]
    fn policy_truncates_long_tool_results() {
        let mut runner = runner_with_policy(CompressionPolicy {
            max_messages: 0, // disabled
            tool_content_max_chars: 100,
        });
        runner.messages.push(Message::tool("c1", &"y".repeat(2_000)));
        let stats = runner.compress_now();
        assert_eq!(stats.tool_results_truncated, 1);
        let msg = &runner.messages()[0];
        assert!(msg.content.len() < 200);
        assert!(msg.content.contains("[...truncated"));
        // The tool_call_id is preserved.
        assert_eq!(msg.tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn policy_slices_to_max_messages() {
        let mut runner = runner_with_policy(CompressionPolicy {
            max_messages: 4,
            tool_content_max_chars: 0, // disabled
        });
        runner.messages.push(Message::system("SYS"));
        runner.messages.push(Message::user("task"));
        for i in 0..20 {
            runner.messages.push(Message::assistant_text(&format!("a{}", i)));
        }
        let stats = runner.compress_now();
        assert_eq!(stats.kept, 4);
        // We sliced from 22 messages down to 4, so 18 were dropped.
        // The original test compared against `runner.messages().len() - 4`,
        // which equals 0 (the post-slicing length), but the actual
        // `dropped` counter records how many messages were removed —
        // 22 - 4 = 18. Hard-coding 18 keeps the assertion honest
        // even if we change the slicing strategy later.
        assert_eq!(stats.dropped, 18);
        // System prompt survives.
        assert_eq!(runner.messages()[0].role, Role::System);
        // Original task survives.
        assert!(runner
            .messages()
            .iter()
            .any(|m| m.role == Role::User && m.content == "task"));
    }

    #[test]
    fn approx_total_tokens_matches_heuristic() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.messages.push(Message::user("abcd")); // 1 token
        runner.messages.push(Message::assistant_text("efghijkl")); // 2 tokens
        assert_eq!(runner.approx_total_tokens(), 3);
    }

    #[test]
    fn custom_policy_propagates_to_runner() {
        let mut runner = runner_with_policy(CompressionPolicy {
            max_messages: 2,
            tool_content_max_chars: 10,
        });
        runner.config_mut().compression.max_messages = 5;
        assert_eq!(runner.config().compression.max_messages, 5);
        assert_eq!(runner.config().compression.tool_content_max_chars, 10);
    }

    #[test]
    fn ensure_system_prompt_inserts_when_empty() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.ensure_system_prompt("you are a coach");
        assert_eq!(runner.messages().len(), 1);
        assert_eq!(runner.messages()[0].role, Role::System);
        assert_eq!(runner.messages()[0].content, "you are a coach");
    }

    #[test]
    fn ensure_system_prompt_is_idempotent() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.ensure_system_prompt("you are a coach");
        runner.ensure_system_prompt("you are a coach");
        runner.ensure_system_prompt("you are a coach");
        assert_eq!(runner.messages().len(), 1);
    }

    #[test]
    fn ensure_system_prompt_replaces_stale() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.ensure_system_prompt("old prompt");
        runner.ensure_system_prompt("new prompt");
        assert_eq!(runner.messages().len(), 1);
        assert_eq!(runner.messages()[0].content, "new prompt");
    }

    #[test]
    fn ensure_system_prompt_prepends_when_head_is_user() {
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.messages.push(Message::user("hello"));
        runner.ensure_system_prompt("you are a coach");
        assert_eq!(runner.messages().len(), 2);
        assert_eq!(runner.messages()[0].role, Role::System);
        assert_eq!(runner.messages()[1].role, Role::User);
        assert_eq!(runner.messages()[1].content, "hello");
    }

    #[test]
    fn approx_total_tokens_includes_system_prompt() {
        // Once we persist the system prompt in `self.messages`, the
        // token estimate must include it. This is what makes
        // `approx_total_tokens` auditable as "what the LLM saw".
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        runner.ensure_system_prompt("abcdabcdabcdabcd"); // 16 chars / 4 = 4 tokens
        runner.messages.push(Message::user("abcd")); // 1 token
        assert_eq!(runner.approx_total_tokens(), 5);
    }
}

// ============================================================================
// TraceSink tests
// ============================================================================
//
// The new trace plumbing replaces a pile of bare `println!` calls.
// These tests pin:
//   * the `TraceEvent` variants and their payloads,
//   * the `NullSink` / `LogSink` / `FnSink` / `SharedTraceSink`
//     implementations,
//   * the runner's `emit_trace` dispatch (sink → fallback).

#[cfg(test)]
mod trace_sink_tests {
    use super::*;
    use crate::conversation::CompressionPolicy;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    #[test]
    fn null_sink_swallows_events() {
        // The whole point of `NullSink` is that it does nothing —
        // even if the test runner captures stdout, no event should
        // leak. We can only assert the absence of panics here.
        let mut s = NullSink;
        s.event(TraceEvent::RunStart {
            task: "t".to_string(),
        });
    }

    #[test]
    fn fn_sink_forwards_events() {
        let captured: Rc<RefCell<Vec<TraceEvent>>> = Default::default();
        let captured_clone = captured.clone();
        let mut sink = FnSink::new(move |event| {
            captured_clone.borrow_mut().push(event);
        });
        sink.event(TraceEvent::FinalResult {
            body: "done".to_string(),
        });
        assert_eq!(captured.borrow().len(), 1);
        assert_eq!(
            captured.borrow()[0],
            TraceEvent::FinalResult {
                body: "done".to_string()
            }
        );
    }

    #[test]
    fn shared_sink_drops_events_before_install() {
        let shared = Arc::new(SharedTraceSink::new());
        let called = Rc::new(RefCell::new(false));
        let called_c = called.clone();
        let result = shared.with_sinks(|sinks| {
            for s in sinks.iter_mut() {
                s.event(TraceEvent::Observing);
            }
            *called_c.borrow_mut() = true;
        });
        assert!(result.is_none(), "no sink installed → closure must not run");
        assert!(!*called.borrow(), "and no event was delivered");
    }

    #[test]
    fn shared_sink_routes_after_install() {
        let shared = Arc::new(SharedTraceSink::new());
        let captured: Rc<RefCell<Vec<TraceEvent>>> = Default::default();
        let captured_c = captured.clone();
        shared.install(Box::new(FnSink::new(move |event| {
            captured_c.borrow_mut().push(event);
        })));
        shared.with_sinks(|sinks| {
            for s in sinks.iter_mut() {
                s.event(TraceEvent::RunStart {
                    task: "t".to_string(),
                });
            }
        });
        assert_eq!(captured.borrow().len(), 1);
    }

    #[test]
    fn shared_sink_clear_returns_to_silent() {
        let shared = Arc::new(SharedTraceSink::new());
        shared.install(Box::new(NullSink));
        shared.clear();
        assert!(shared.with_sinks(|_| ()).is_none());
    }

    #[test]
    fn shared_sink_delivers_to_all_installed_sinks() {
        // The regression that motivated switching from
        // `Option<Box<dyn TraceSink>>` to `Vec<Box<...>>`: a
        // second `install` call used to overwrite the first sink,
        // so only `LogSink` survived in the CLI. Make sure both
        // sinks now get every event.
        let shared = Arc::new(SharedTraceSink::new());
        let captured_a: Rc<RefCell<usize>> = Default::default();
        let captured_b: Rc<RefCell<usize>> = Default::default();
        let ca = captured_a.clone();
        let cb = captured_b.clone();
        shared.install(Box::new(FnSink::new(move |_| {
            *ca.borrow_mut() += 1;
        })));
        shared.install(Box::new(FnSink::new(move |_| {
            *cb.borrow_mut() += 1;
        })));
        shared.with_sinks(|sinks| {
            for s in sinks.iter_mut() {
                s.event(TraceEvent::Observing);
            }
        });
        assert_eq!(*captured_a.borrow(), 1, "first sink got the event");
        assert_eq!(*captured_b.borrow(), 1, "second sink got the event too");
    }

    #[test]
    fn shared_sink_len_reports_installed_count() {
        let shared = Arc::new(SharedTraceSink::new());
        assert_eq!(shared.len(), 0);
        assert!(shared.is_empty());
        shared.install(Box::new(NullSink));
        assert_eq!(shared.len(), 1);
        shared.install(Box::new(NullSink));
        assert_eq!(shared.len(), 2);
        shared.clear();
        assert_eq!(shared.len(), 0);
        assert!(shared.is_empty());
    }

    fn runner_with_sink_and_policy(
        policy: CompressionPolicy,
    ) -> RealAgentRunner<crate::real_tools::SimulatorExecutor> {
        let mut cfg = RunnerConfig::default();
        cfg.probe_ollama_on_run = false;
        cfg.compression = policy;
        let mut exec = crate::real_tools::SimulatorExecutor::new();
        exec.connect_ble();
        RealAgentRunner::with_config(exec, cfg)
    }

    #[test]
    fn runner_with_sink_runs_events_through_it() {
        let shared = Arc::new(SharedTraceSink::new());
        let captured: Rc<RefCell<Vec<TraceEvent>>> = Default::default();
        let captured_c = captured.clone();
        shared.install(Box::new(FnSink::new(move |event| {
            captured_c.borrow_mut().push(event);
        })));

        let mut runner = runner_with_sink_and_policy(CompressionPolicy::disabled());
        runner.set_trace_sink(Some(shared));
        runner.set_verbose(false); // ensure fallback path is silent
        // Drive the runner through the simulated path so no
        // network probe happens.
        let result = runner.run("noop");
        assert!(result.is_ok(), "simulated run must succeed");
        let events = captured.borrow();
        assert!(
            events.iter().any(|e| matches!(e, TraceEvent::RunStart { .. })),
            "missing RunStart event: got {} events",
            events.len()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TraceEvent::BackendReady { .. })),
            "missing BackendReady event: got {} events",
            events.len()
        );
    }

    #[test]
    fn runner_without_sink_still_supports_legacy_verbose() {
        // When no sink is installed and `verbose = true`, the
        // legacy `println!` fallback fires. We can't easily capture
        // stdout in a unit test, so the assertion is just that the
        // run completes without panicking — the visible side
        // effect is exercised manually / in integration tests.
        let mut runner = runner_with_sink_and_policy(CompressionPolicy::disabled());
        runner.set_verbose(true);
        assert!(runner.run("noop").is_ok());
    }

    #[test]
    fn runner_verbose_false_no_sink_means_silent() {
        // The exact contract users relied on pre-refactor: no
        // sink + verbose=false ⇒ no output. The legacy fallback
        // path is also gated on `verbose`, so we just need to
        // make sure the run still succeeds.
        let mut runner = runner_with_sink_and_policy(CompressionPolicy::disabled());
        runner.set_verbose(false);
        assert!(runner.run("noop").is_ok());
    }
}
