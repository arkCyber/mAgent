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
    /// A run budget was exhausted.
    BudgetExhausted {
        /// Which budget fired: `"iterations"`, `"tool_calls"`, or
        /// `"wall_clock_ms"` (in which case `limit` is milliseconds).
        kind: &'static str,
        /// Cap that was hit, in the unit named by `kind`.
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
///
/// `Send + Sync` is *not* required on the trait itself: a sink's
/// `event` method takes `&mut self`, so two threads calling it
/// concurrently is impossible by construction. Adding the bound
/// would forbid legitimate non-`Send` closures (e.g. those holding
/// `Rc<RefCell<...>>` for test capture) without buying anything.
/// Callers that need cross-thread delivery should wrap the sink in
/// a `Mutex` and install it via [`SharedTraceSink`].
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
/// The `+ Send + Sync` bound is needed *here*, not on the trait, so
/// that [`SharedTraceSink`] (which wraps these in a `Mutex<Vec<_>>`)
/// remains `Send + Sync` and can travel across threads inside an
/// `Arc`. The trait itself intentionally drops the bound so
/// non-`Send` sinks (e.g. test closures holding `Rc<RefCell<...>>`)
/// can be installed when the runner is provably single-threaded.
pub type BoxedTraceSink = Box<dyn TraceSink + Send + Sync>;

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
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    pub fn with_sinks<R>(&self, f: impl FnOnce(&mut [&mut dyn TraceSink]) -> R) -> Option<R> {
        let mut guard = self.lock_sinks();
        if guard.is_empty() {
            return None;
        }
        // Collect a vector of mutable references to the inner
        // trait objects, then hand it to the closure as a slice.
        // The vector lives only for the duration of the call so no
        // sink escapes the borrow scope.
        let mut refs: Vec<&mut dyn TraceSink> = guard
            .iter_mut()
            .map(|b| b.as_mut() as &mut dyn TraceSink)
            .collect();
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
    /// The run failed: either an unrecoverable error was raised, or the loop
    /// ran out of budget without producing a final answer. Every `Err` from
    /// [`RealAgentRunner::run`] leaves the state here, so a caller that only
    /// watches [`RealAgentRunner::state`] can still tell success from failure.
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
    /// Optional wall-clock budget for one `run()` / `run_continuing()`
    /// call.
    ///
    /// Checked at the head of every ReAct-loop iteration, so it bounds
    /// the time spent *between* LLM and tool calls (for example, ten slow
    /// model replies) — an in-flight request is still bounded by the
    /// client's own timeout. `None` keeps the historical behaviour: only
    /// `max_iterations` and `max_tool_calls` bound a run.
    pub max_run_duration: Option<std::time::Duration>,
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
            max_run_duration: None,
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
- read_sensor(sensor): Read sensor - sensor can be: temperature, accelerometer, humidity, pressure, light, heart_rate, hrv, glucose, ecg, stress, battery, memory

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

/// One tool as the default system prompt advertises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvertisedTool {
    /// Tool name, e.g. `start_exercise`.
    pub name: &'static str,
    /// The signature the prompt writes, e.g. `start_exercise(type)`.
    pub signature: &'static str,
    /// Everything after the colon on the prompt's tool line.
    pub description: &'static str,
}

/// Every tool the default system prompt ([`HEALTH_SYSTEM_PROMPT`]) advertises,
/// parsed out of the prompt text itself.
///
/// Parsed rather than kept as a second hand-maintained list, because a second
/// list is exactly what drifts: add a tool line and forget the list, and both
/// the `/tools` listing and the executor guard below stop seeing the new tool.
/// Every advertised name is a promise made to the model, and an executor with
/// no arm for one turns that promise into a tool error at run time — the
/// failure rounds 14-18 fixed one name at a time.
pub fn advertised_tools() -> alloc::vec::Vec<AdvertisedTool> {
    let mut tools = alloc::vec::Vec::new();
    for line in HEALTH_SYSTEM_PROMPT.lines() {
        // A tool line is `- name(args): description`; the Rules section and
        // the prose around the list have no leading `- name(`.
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let Some(open) = rest.find('(') else {
            continue;
        };
        let Some(close) = rest[open..].find(')').map(|i| open + i) else {
            continue;
        };
        let name = &rest[..open];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        let signature = &rest[..close + 1];
        let description = rest[close + 1..].strip_prefix(": ").unwrap_or("");
        tools.push(AdvertisedTool {
            name,
            signature,
            description,
        });
    }
    tools
}

/// Every sensor name the default system prompt's `read_sensor` line
/// advertises, parsed out of the prompt text itself.
///
/// The prompt is a contract with the model, and the sensor vocabulary it
/// lists is the tool's real capability: a model told only about nine
/// sensors will not offer `battery` or `memory` even though the executor
/// resolves them. Parsed rather than duplicated so the prompt and the
/// guard tests in [`crate::simulator`] and [`crate::tools`] cannot drift
/// apart — they all iterate the same contract.
pub fn advertised_sensors() -> alloc::vec::Vec<&'static str> {
    let Some(line) = HEALTH_SYSTEM_PROMPT
        .lines()
        .find(|l| l.starts_with("- read_sensor("))
    else {
        return alloc::vec::Vec::new();
    };
    let Some((_, rest)) = line.split_once("sensor can be:") else {
        return alloc::vec::Vec::new();
    };
    rest.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

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
// LLM transport resilience
// ============================================================================
//
// A single transient failure — a connection reset, a 503 while the model
// server restarts, a 429 from a rate limiter — used to fail the whole
// `magent run`, because the LLM backends sent one request and gave up.
// (The blockchain HTTP client already retried; the LLM path did not.)
//
// The retry below is deliberately narrow: only conditions that a *second*
// attempt could plausibly clear. A 4xx that isn't 408/429 means the
// request itself was wrong, so retrying it would just waste the user's
// time and quota before reporting the same error.

/// Extra attempts after the first for a transient LLM transport failure.
const LLM_MAX_RETRIES: u32 = 2;
/// First backoff delay; doubles per attempt (`base * 2^attempt`).
const LLM_RETRY_BASE_MS: u64 = 250;
/// Ceiling on the backoff delay, so a long retry chain stays bounded.
const LLM_RETRY_MAX_MS: u64 = 2_000;
/// Ceiling on a *server-supplied* `Retry-After`, which is usually far larger
/// than our own backoff. Honouring the header is the point — a `429` that says
/// "30 seconds" is not cleared by a 250 ms retry — but the value comes from a
/// remote server, so it is capped rather than trusted: a hostile or broken
/// endpoint must not be able to hang `magent run` for a day.
const LLM_RETRY_AFTER_MAX_MS: u64 = 30_000;

/// Whether `status` describes a condition worth retrying.
///
/// `408` (the server gave up on our request), `429` (slow down), and a
/// `5xx` are transient by definition — **except** `501 Not Implemented`
/// and `505 HTTP Version Not Supported`, which say the request can never
/// succeed as written and would 501/505 again on a second attempt.
/// Everything else — including every other 4xx — is deterministic: the
/// same request gets the same answer, so the caller should see the error
/// instead of a delay.
fn is_transient_status(status: reqwest::StatusCode) -> bool {
    if status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return true;
    }
    status.is_server_error()
        && status != reqwest::StatusCode::NOT_IMPLEMENTED
        && status != reqwest::StatusCode::HTTP_VERSION_NOT_SUPPORTED
}

/// Exponential backoff for `attempt` (0-based), capped at
/// [`LLM_RETRY_MAX_MS`].
fn retry_backoff(attempt: u32) -> std::time::Duration {
    let shift = attempt.min(20); // guard the shift itself against overflow
    let ms = LLM_RETRY_BASE_MS.saturating_mul(1u64 << shift);
    std::time::Duration::from_millis(ms.min(LLM_RETRY_MAX_MS))
}

/// Parse an RFC 7231 `Retry-After` value into a wait.
///
/// Both forms the header allows are honoured. The `delta-seconds` form
/// (`Retry-After: 30`) is the number itself. The `HTTP-date` form
/// (`Retry-After: Wed, 21 Oct 2015 07:28:00 GMT`) is converted to a delta
/// against `now`: a date already in the past yields a zero delta, which
/// [`retry_delay`] then floors at the exponential backoff, so the header can
/// still only *extend* a wait. `now` is a parameter rather than a clock read
/// so the conversion is deterministic under test; the caller passes
/// `SystemTime::now()`.
///
/// A value in neither form returns `None`, so a malformed header degrades to
/// the backoff rather than to "retry at once".
fn parse_retry_after(value: &str, now: std::time::SystemTime) -> Option<std::time::Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    let now_unix = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let target = http_date_to_unix(value, now_unix)?;
    Some(std::time::Duration::from_secs(
        (target - now_unix).max(0) as u64
    ))
}

/// The server's retry hint for this response, if it gave one.
fn retry_after_hint(
    headers: &reqwest::header::HeaderMap,
    now: std::time::SystemTime,
) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_retry_after(v, now))
}

/// Parse an HTTP-date (RFC 7231 §7.1.1.1) into Unix seconds UTC.
///
/// All three formats a recipient is required to accept are handled:
///
/// * IMF-fixdate — `Sun, 06 Nov 1994 08:49:37 GMT` (the format senders MUST
///   use, and the only one seen in practice)
/// * RFC 850 — `Sunday, 06-Nov-94 08:49:37 GMT` (obsolete, two-digit year)
/// * asctime — `Sun Nov  6 08:49:37 1994`
///
/// The day-of-week is not validated: it is redundant with the date, and a
/// delta computed from a date with the *wrong* weekday is no worse than the
/// `None` we would otherwise return. The obsolete RFC 850 two-digit year is
/// resolved by the RFC 7231 rule — a year more than 50 years ahead of `now`
/// is read as the most recent year in the past with the same last two digits.
fn http_date_to_unix(s: &str, now_unix: i64) -> Option<i64> {
    let tokens: Vec<&str> = s.split(' ').filter(|t| !t.is_empty()).collect();
    match tokens.len() {
        // IMF-fixdate: "Sun, 06 Nov 1994 08:49:37 GMT"
        6 => {
            let day = tokens[1].parse::<u32>().ok()?;
            let month = month_from_name(tokens[2])?;
            let year = parse_year4(tokens[3])?;
            let hms = parse_hms(tokens[4])?;
            if !tokens[5].eq_ignore_ascii_case("GMT") {
                return None;
            }
            unix_from_parts(year, month, day, hms)
        }
        // asctime: "Sun Nov  6 08:49:37 1994"
        5 => {
            let month = month_from_name(tokens[1])?;
            let day = tokens[2].parse::<u32>().ok()?;
            let hms = parse_hms(tokens[3])?;
            let year = parse_year4(tokens[4])?;
            unix_from_parts(year, month, day, hms)
        }
        // RFC 850: "Sunday, 06-Nov-94 08:49:37 GMT"
        4 => {
            let mut date = tokens[1].split('-');
            let day = date.next()?.parse::<u32>().ok()?;
            let month = month_from_name(date.next()?)?;
            let year = rfc850_year(date.next()?, now_unix)?;
            if date.next().is_some() {
                return None;
            }
            let hms = parse_hms(tokens[2])?;
            if !tokens[3].eq_ignore_ascii_case("GMT") {
                return None;
            }
            unix_from_parts(year, month, day, hms)
        }
        _ => None,
    }
}

/// Resolve an RFC 7231 month name (`Jan`, `feb`, …) to `1..=12`.
fn month_from_name(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(name))
        .map(|i| i as u32 + 1)
}

/// Parse a `HH:MM:SS` time-of-day, rejecting out-of-range fields.
fn parse_hms(t: &str) -> Option<(u32, u32, u32)> {
    let mut parts = t.split(':');
    let h = parts.next()?.parse::<u32>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?;
    let s = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some((h, m, s))
}

/// Parse an exactly-four-digit year.
fn parse_year4(s: &str) -> Option<i64> {
    if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// Resolve an RFC 850 two-digit year against `now_unix` per RFC 7231: a year
/// that would be more than 50 years ahead is read as the most recent past year
/// with the same last two digits.
fn rfc850_year(s: &str, now_unix: i64) -> Option<i64> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let yy = s.parse::<i64>().ok()?;
    let now_year = crate::time_sync::unix_to_calendar(now_unix)?.0 as i64;
    let mut year = now_year - now_year.rem_euclid(100) + yy;
    if year > now_year + 50 {
        year -= 100;
    }
    Some(year)
}

/// Convert a civil date and time-of-day (UTC) to Unix seconds.
fn unix_from_parts(year: i64, month: u32, day: u32, (h, m, s): (u32, u32, u32)) -> Option<i64> {
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + h as i64 * 3_600 + m as i64 * 60 + s as i64)
}

/// Days from 1970-01-01 to `year-month-day`, Hinnant's `days_from_civil`.
/// `month` must be `1..=12`; the day is assumed valid (the caller range-checks
/// it) and is normalised arithmetically if it is not.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146_096]
    era * 146_097 + doe - 719_468
}

/// The wait before retrying `attempt` (0-based).
///
/// The server's `Retry-After` **replaces** the backoff when it asks for a
/// longer wait — the whole point of the header is that our guess at how long
/// the condition lasts is worse than the server's knowledge. It is never
/// allowed to *shorten* the wait (a `Retry-After: 0` from a limiter that is
/// still shedding load must not turn the retry into a busy loop), and it is
/// capped at [`LLM_RETRY_AFTER_MAX_MS`].
fn retry_delay(attempt: u32, hint: Option<std::time::Duration>) -> std::time::Duration {
    let backoff = retry_backoff(attempt);
    match hint {
        None => backoff,
        Some(hint) => {
            let capped = hint.min(std::time::Duration::from_millis(LLM_RETRY_AFTER_MAX_MS));
            backoff.max(capped)
        }
    }
}

/// Build the warning for a retry that is about to happen.
///
/// A retry is otherwise invisible: the call simply takes longer and the
/// run succeeds as if nothing happened, which makes "why is this slow?"
/// unanswerable from the outside. Every backoff is therefore logged.
///
/// Split out of [`warn_retry`] so the message itself can be tested: a
/// global `log` logger cannot be installed per-test, so asserting on the
/// string the logger is handed is the only way to pin what a retry says.
fn retry_warning(
    label: &str,
    attempt: u32,
    max_retries: u32,
    cause: &str,
    delay: std::time::Duration,
) -> String {
    format!(
        "{label}: attempt {}/{} failed ({}); retrying in {}ms",
        attempt + 1,
        max_retries + 1,
        cause,
        delay.as_millis(),
    )
}

/// Log the retry that is about to happen on the `magent.llm` target
/// (visible under `--verbose` / `RUST_LOG=magent.llm=warn`).
fn warn_retry(
    label: &str,
    attempt: u32,
    max_retries: u32,
    cause: &str,
    delay: std::time::Duration,
) {
    let message = retry_warning(label, attempt, max_retries, cause, delay);
    log::warn!(target: "magent.llm", "{}", message);
}

/// Send a request, retrying transient failures up to `max_retries` times.
///
/// `send` is called once per attempt and must rebuild the request: a
/// `RequestBuilder` is consumed by `send()`, so the closure owns that
/// (the call sites below re-attach a cloned body). On success — or on a
/// non-transient status, or once `max_retries` is exhausted — the
/// `Response` is handed back untouched so the caller keeps its existing
/// status handling, including reading the error body.
///
/// `max_retries` is a parameter rather than a constant read inside so
/// tests can pin the attempt count without paying the real backoff.
///
/// **The retry is not a silent fallback.** It only delays the error the
/// caller would otherwise have seen immediately; the runner still fails
/// loudly when every attempt loses. A hung backend is retried too, so the
/// worst case for a call is `(max_retries + 1) × timeout` — 3 × 60s for
/// the LLM clients as configured.
fn send_with_retry<F>(
    label: &str,
    max_retries: u32,
    send: F,
) -> std::result::Result<reqwest::blocking::Response, String>
where
    F: FnMut() -> std::result::Result<reqwest::blocking::Response, reqwest::Error>,
{
    send_with_retry_sleeping(label, max_retries, send, std::thread::sleep)
}

/// [`send_with_retry`] with the sleep injected.
///
/// The wait is the one thing about this function a test cannot otherwise
/// observe without paying it, and the wait is exactly what the `Retry-After`
/// work changed. Same reason [`retry_warning`] was split out: pin the decision
/// without performing the effect.
fn send_with_retry_sleeping<F, S>(
    label: &str,
    max_retries: u32,
    mut send: F,
    mut sleep: S,
) -> std::result::Result<reqwest::blocking::Response, String>
where
    F: FnMut() -> std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    S: FnMut(std::time::Duration),
{
    // Only used by the (unreachable) fall-through below: the final
    // attempt never sleeps, so the loop always returns. Kept as a plain
    // `String` rather than an `unreachable!()` so a future edit to the
    // loop bounds degrades to an error message instead of a panic.
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match send() {
            Ok(response) => {
                let status = response.status();
                if is_transient_status(status) && attempt < max_retries {
                    // Read the hint off *this* response, before it is consumed
                    // by the next attempt.
                    let hint = retry_after_hint(response.headers(), std::time::SystemTime::now());
                    let delay = retry_delay(attempt, hint);
                    warn_retry(
                        label,
                        attempt,
                        max_retries,
                        &format!("HTTP {status}"),
                        delay,
                    );
                    sleep(delay);
                    continue;
                }
                return Ok(response);
            }
            Err(e) => {
                last_error = if attempt == 0 {
                    format!("Request failed: {}", e)
                } else {
                    format!("Request failed after {} attempts: {}", attempt + 1, e)
                };
                if attempt == max_retries {
                    return Err(last_error);
                }
                // A transport error has no response and so no hint.
                let delay = retry_delay(attempt, None);
                warn_retry(label, attempt, max_retries, &e.to_string(), delay);
                sleep(delay);
            }
        }
    }
    Err(last_error)
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
    client: std::result::Result<reqwest::blocking::Client, String>,
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
            .map_err(|e| format!("could not build the HTTP client: {e}"));
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

    /// The HTTP client, or the reason it could not be built.
    ///
    /// `ClientBuilder::build` only fails when the TLS backend cannot
    /// initialise. The old fallback — `Client::new()` — was not a
    /// fallback at all: it runs the same initialisation and *panics* on
    /// failure, and it silently dropped the 60-second timeout, which is
    /// the one setting that stops a stalled connection from hanging the
    /// agent. Carrying the error instead lets a caller report it like any
    /// other transport failure.
    fn http_client(&self) -> std::result::Result<&reqwest::blocking::Client, String> {
        self.client.as_ref().map_err(Clone::clone)
    }

    /// Issue an unauthenticated `GET /api/tags` against the server and
    /// report whether the response was `2xx`. Useful for the CLI's
    /// `--doctor` flow and for the runner's optional backend probe.
    ///
    /// Retried like a chat call, because this answer is load-bearing:
    /// `false` makes the runner abort with "LLM backend unreachable" and
    /// makes the CLI fall back to simulated answers. Un-retried, a single
    /// blip on the run's *first* network call cost the whole session, and
    /// the retry on the chat path could never help because the run never
    /// got that far.
    pub fn check_connection(&self) -> bool {
        let client = match self.http_client() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let url = format!("{}/api/tags", self.base_url);
        send_with_retry("ollama probe", LLM_MAX_RETRIES, || client.get(&url).send())
            .map(|response| response.status().is_success())
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
        let client = self.http_client()?;
        let response = send_with_retry("ollama /api/chat", LLM_MAX_RETRIES, || {
            client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone())
                .send()
        })?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        self.response_buf.clear();
        self.response_buf.extend_from_slice(
            &response
                .bytes()
                .map_err(|e| format!("Read body failed: {}", e))?,
        );

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
        let client = match self.http_client() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        if let Ok(response) = client.get(format!("{}/api/tags", self.base_url)).send() {
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

        let client = self.http_client()?;
        let url = format!("{}/api/generate", self.base_url);
        let response = send_with_retry("ollama /api/generate", LLM_MAX_RETRIES, || {
            client.post(&url).json(&request_body).send()
        })?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let json: serde_json::Value = response
            .json()
            .map_err(|e| format!("JSON parse error: {}", e))?;
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
    client: std::result::Result<reqwest::blocking::Client, String>,
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
            .map_err(|e| format!("could not build the HTTP client: {e}"));
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

    /// See [`OllamaClient::http_client`] — the client is a `Result` so a
    /// TLS-bootstrap failure is reported rather than silently swapped for
    /// a timeout-less (and, on the same failure, panicking) default.
    fn http_client(&self) -> std::result::Result<&reqwest::blocking::Client, String> {
        self.client.as_ref().map_err(Clone::clone)
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
        let auth = format!("Bearer {}", self.api_key.0);
        let client = self.http_client()?;
        let response = send_with_retry("deepseek /chat/completions", LLM_MAX_RETRIES, || {
            client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::AUTHORIZATION, &auth)
                .body(body.clone())
                .send()
        })?;

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
                    json.get("choices")
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<missing>".into())
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
    fn write_message_into(buf: &mut Vec<u8>, m: &Message) -> std::result::Result<(), String> {
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
        let client = match self.http_client() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let url = format!("{}/models", self.base_url);
        let auth = format!("Bearer {}", self.api_key.0);
        // Retried — see [`OllamaClient::check_connection`] for why a
        // transient blip on the probe must not decide the session.
        send_with_retry("deepseek probe", LLM_MAX_RETRIES, || {
            client
                .get(&url)
                .header(reqwest::header::AUTHORIZATION, &auth)
                .send()
        })
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
/// shapes mAgent emits/accepts; returns an empty vector if `json` isn't a tool
/// call.
///
/// A response is allowed to name **several** tools at once: the OpenAI
/// `tool_calls` array is precisely that, and models use it for independent
/// reads ("temperature and humidity"). Reading only `tool_calls[0]` executed
/// the first call and discarded the rest, with no error and no trace — the
/// model's next turn then saw a conversation in which it had asked for only
/// the first. Each element of the array is therefore returned.
///
/// The other two formats are single-call by construction: format 1 names one
/// `tool`, and format 3 is a guess at a bare `{"<tool>": {...}}` object whose
/// keys could equally be ordinary data, so it stays conservative and yields at
/// most one.
fn parse_tool_calls_from_json(
    json: &serde_json::Value,
    known_tools: &[(String, String)],
) -> Vec<(String, HashMap<String, serde_json::Value>)> {
    /// Read an `arguments` value that may be either an object or a JSON
    /// string (the OpenAI shape allows both).
    fn arguments_map(value: Option<&serde_json::Value>) -> HashMap<String, serde_json::Value> {
        match value {
            Some(serde_json::Value::Object(obj)) => {
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            }
            Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
            _ => HashMap::new(),
        }
    }

    // Format 1: {"tool": "name", "args": {...}}
    if let (Some(tool), Some(args)) = (json.get("tool"), json.get("args")) {
        if let (Some(name), Some(obj)) = (tool.as_str(), args.as_object()) {
            let args_map = obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            return vec![(name.to_string(), args_map)];
        }
    }

    // Format 1b: a single OpenAI-style tool call without the array wrapper —
    // `{"name": "read_sensor", "arguments": {"sensor": "temp"}}`. The old
    // generic fallback matched the alphabetic first key (`arguments`) as the
    // tool name, so this common shape invoked a tool that does not exist with
    // the wrong key.
    if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
        if json.get("arguments").is_some() && json.get("result").is_none() {
            return vec![(name.to_string(), arguments_map(json.get("arguments")))];
        }
    }

    // Format 2: OpenAI-style "tool_calls" array — every entry.
    if let Some(tcs) = json.get("tool_calls").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for tc in tcs {
            let Some(func) = tc.get("function") else {
                continue;
            };
            let Some(name) = func.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let args_map: HashMap<String, serde_json::Value> =
                if let Some(args_str) = func.get("arguments").and_then(|a| a.as_str()) {
                    // arguments is itself a JSON string
                    serde_json::from_str(args_str).unwrap_or_default()
                } else if let Some(args_obj) = func.get("arguments").and_then(|a| a.as_object()) {
                    args_obj
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                } else {
                    HashMap::new()
                };
            out.push((name.to_string(), args_map));
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Format 3: {"<tool_name>": {"args": {...}}} — a bare object keyed by a
    // tool name.
    //
    // This format cannot distinguish a tool call from ordinary data on its
    // face: `{"result": "25.5 C", "data": {"sensor": "temperature"}}` used to
    // be read as a call to a tool named `data`, discarding the real answer
    // (tool parsing runs before result parsing). Two guards:
    //
    //   1. A response carrying `result`/`error`/`reasoning`/`thought` is an
    //      answer, not a call — never scan it.
    //   2. A candidate key must either be a tool this runner advertises, or
    //      carry an explicit `"args"` object. Arbitrary data keys are not
    //      tool names.
    if let Some(obj) = json.as_object() {
        if obj.keys().any(|k| {
            matches!(
                k.as_str(),
                "result" | "error" | "reasoning" | "thought"
            )
        }) {
            return Vec::new();
        }
        for (key, tool_obj) in obj {
            let Some(tool_obj) = tool_obj.as_object() else {
                continue;
            };
            let is_known_tool = known_tools.iter().any(|(name, _)| name == key);
            let explicit_args = tool_obj
                .get("args")
                .is_some_and(serde_json::Value::is_object);
            if !is_known_tool && !explicit_args {
                continue;
            }
            let args_map: HashMap<String, serde_json::Value> =
                if let Some(args) = tool_obj.get("args").and_then(|a| a.as_object()) {
                    args.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    tool_obj
                        .iter()
                        .filter(|(k, _)| *k != "args")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };
            return vec![(key.clone(), args_map)];
        }
    }

    Vec::new()
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
    /// Monotonic source of tool-call ids, **never reset between turns**.
    ///
    /// `tool_call_count` is a per-run budget and restarts at 0 on every
    /// `run_continuing`, so using it for ids made each REPL turn re-issue
    /// `call_1` while the earlier turns' messages were still in the history —
    /// one conversation carrying several distinct calls under the same id.
    call_id_seq: usize,
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
    /// System note queued by [`Self::seed_system_context`], inserted
    /// immediately after the system prompt at the start of the next run
    /// and then cleared. See that method for why it cannot be inserted by
    /// the caller.
    pending_system_seed: Option<String>,
    /// Whether the ReAct loop should route the next chat through the
    /// configured [`LlmBackend`]. Flipped to `true` automatically by
    /// `probe_ollama_on_run` when the probe succeeds.
    backend_enabled: bool,
    /// The wall-clock limit that stopped the current run, if any. Set by
    /// the loop guard so `get_result` can say *why* the run ended without
    /// an answer instead of reporting only the iteration/tool counters.
    wall_clock_exhausted: Option<std::time::Duration>,
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
            call_id_seq: 0,
            config: RunnerConfig::default(),
            last_compression_stats: None,
            last_compression_policy: None,
            pending_system_seed: None,
            backend_enabled: false,
            wall_clock_exhausted: None,
            backend: Some(Box::new(OllamaClient::new(
                "http://localhost:11434",
                "llama3.2",
            ))),
        }
    }

    /// Drop the LLM backend, so every response comes from the built-in
    /// simulated responder.
    ///
    /// This is the **explicit** opt-in to simulation. Demos and tests use it to
    /// exercise the ReAct loop with no model reachable. It is deliberately not
    /// a fallback: `run()` now returns `Err` when a configured backend is
    /// unreachable, because a fabricated answer is indistinguishable to the
    /// caller from a real one.
    pub fn without_backend(mut self) -> Self {
        self.backend = None;
        self.backend_enabled = false;
        self
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
            call_id_seq: 0,
            config,
            last_compression_stats: None,
            last_compression_policy: None,
            pending_system_seed: None,
            backend_enabled: false,
            wall_clock_exhausted: None,
            backend: Some(Box::new(OllamaClient::new(
                "http://localhost:11434",
                "llama3.2",
            ))),
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
        let mut out =
            String::with_capacity(self.config.system_prompt.len() + tool_section.len() + 32);
        out.push_str(&self.config.system_prompt);
        out.push_str("\n\n## Available tools\n");
        out.push_str(&tool_section);
        out
    }

    /// Install (or replace) the trace sink. See
    /// [`RunnerConfig::trace_sink`] for the rationale. Pass `None`
    /// to revert to the historical silent / `println!` behaviour.
    pub fn set_trace_sink(&mut self, sink: Option<std::sync::Arc<SharedTraceSink>>) {
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
    /// The insertion point is `messages[1]` when `messages[0]` is a
    /// system message; otherwise we prepend at the very front.
    ///
    /// Cheap (one `Vec::insert`) and safe to call multiple times.
    /// Callers that need to inject several messages should batch
    /// them into a single call rather than calling this in a loop.
    ///
    /// **Not safe for seeding a run that has not started yet.** On an
    /// empty conversation there is no system message, so the message
    /// lands at index 0 — and the next `run()` treats a system message
    /// with unexpected content at index 0 as a *stale prompt* and
    /// overwrites it (see `ensure_system_prompt`), or drops it outright
    /// if it is a plain `run()` and `reset()` cleared the conversation
    /// first. Use [`Self::seed_system_context`] for that; this method is
    /// for injecting into a conversation that is already under way.
    pub fn insert_after_system_prompt(&mut self, msg: Message) {
        let pos = match self.messages.first() {
            Some(m) if m.role == Role::System => 1,
            _ => 0,
        };
        self.messages.insert(pos, msg);
    }

    /// Queue `body` as a system note to be inserted immediately after the
    /// live system prompt at the start of the next `run()` /
    /// `run_continuing()`, then forgotten.
    ///
    /// This is the reliable way to seed a not-yet-started run — the CLI's
    /// `--load-summary <TOPIC>` window is the only caller. Applying it
    /// inside `run_inner`, *after* the conversation reset and the
    /// `ensure_system_prompt` reconciliation, is what makes it survive
    /// both of them; injecting it from the caller beforehand does not:
    ///
    /// * `run()` calls `reset()`, which clears the conversation, so an
    ///   early insert is simply gone;
    /// * `run_continuing()` keeps the conversation, but the note would be
    ///   at index 0 — where `ensure_system_prompt` sees a system message
    ///   whose content is not the prompt and overwrites it.
    ///
    /// Consumed once, so a multi-turn session gets the note in its history
    /// rather than re-prepended on every turn.
    pub fn seed_system_context(&mut self, body: &str) {
        self.pending_system_seed = Some(body.to_string());
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
            if sink
                .with_sinks(|sinks| {
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
                    println!(
                        "[Agent] Connected to {} - using real AI reasoning",
                        provider
                    );
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
                        "[Compress] kept={} dropped={} tools_truncated={} bytes_saved={}",
                        kept, dropped, tool_results_truncated, bytes_saved
                    );
                }
            }
            TraceEvent::LlmResponse { body } => {
                // Cut on a char boundary: `&body[..200]` panicked with "end byte
                // index 200 is not a char boundary" whenever the model replied
                // with multi-byte text (e.g. a Chinese answer). Same regression
                // `cli::runner::truncate_utf8` exists to prevent.
                let display = match floor_char_boundary(&body, 200) {
                    Some(end) => &body[..end],
                    None => body.as_str(),
                };
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

    /// Drop the cached "this backend is reachable" verdict so the next
    /// `run()` probes again instead of reusing the earlier result.
    ///
    /// This is what turns `probe_ollama_on_run` from "probe once per
    /// process" into "probe every run" — the long-lived REPL resets
    /// the verdict between turns so a backend that came up (or went
    /// down) mid-session is noticed, rather than the first answer
    /// being latched for the whole session.
    ///
    /// No-op when no backend is wired up, or when
    /// `probe_ollama_on_run` is off. In the latter case `run()` would
    /// not probe, so clearing the flag would only silently downgrade
    /// a working backend to the simulated responder.
    pub fn reset_backend_probe(&mut self) {
        if self.backend.is_some() && self.config.probe_ollama_on_run {
            self.backend_enabled = false;
        }
    }

    /// Force-disable the backend: nothing is sent to the LLM and every response
    /// comes from the built-in simulated responder.
    ///
    /// This also clears `probe_ollama_on_run`, because otherwise the next
    /// `run()` would probe the still-wired backend — and a failed probe is now
    /// an error, not a silent fallback — turning an explicit "simulate this"
    /// into `Err`. Disabling the backend is a decision, so it sticks.
    pub fn force_disable_backend(&mut self) {
        self.backend_enabled = false;
        self.config.probe_ollama_on_run = false;
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
    pub fn backend_provider(&self) -> Option<LlmProvider> {
        self.backend.as_ref().map(|b| b.provider())
    }

    /// Run the ReAct loop on `task` until the assistant emits a
    /// terminal result, a budget is exhausted, or an unrecoverable
    /// error occurs. Returns the assistant's terminal payload.
    ///
    /// Starts from a clean conversation: any messages from a previous
    /// `run` are dropped. Use [`Self::run_continuing`] to append to the
    /// existing conversation instead.
    pub fn run(&mut self, task: &str) -> std::result::Result<String, String> {
        self.run_inner(task, false)
    }

    /// Like [`Self::run`], but **keeps** the messages from previous runs
    /// so the agent answers with the earlier turns in context
    /// (multi-turn). Per-run state — the state machine and the
    /// iteration / tool-call counters — is still reset, so budgets are
    /// per task, not per session.
    ///
    /// This is what the CLI's REPL uses; `--repl` is documented as
    /// "maintains conversation context across multiple turns", and plain
    /// [`Self::run`] would wipe it every turn.
    pub fn run_continuing(&mut self, task: &str) -> std::result::Result<String, String> {
        self.run_inner(task, true)
    }

    fn run_inner(
        &mut self,
        task: &str,
        keep_history: bool,
    ) -> std::result::Result<String, String> {
        if keep_history {
            self.reset_run_state();
        } else {
            self.reset();
        }

        self.emit_trace(TraceEvent::RunStart {
            task: task.to_string(),
        });

        // Probe the LLM backend — only on the first run, or when
        // explicitly requested. After we know it is reachable we keep using it.
        if self.config.probe_ollama_on_run && !self.backend_enabled {
            if let Some(ref backend) = self.backend {
                if backend.check_connection() {
                    self.backend_enabled = true;
                } else {
                    // A backend was configured but is unreachable. Fail here
                    // rather than falling through to the simulated responder:
                    // a device whose link dropped would otherwise report a
                    // canned scripted answer as if the model had produced it.
                    // The deliberate no-backend simulation path (nothing
                    // configured) is unaffected.
                    self.state = AgentState::Error;
                    return Err(format!(
                        "LLM backend unreachable ({}); refusing to fall back to simulated responses",
                        backend.provider()
                    ));
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
        // `ensure_system_prompt` inserts at index 0 when the conversation
        // is empty (the `run` case) and is a no-op when a matching system
        // message is already there (the `run_continuing` case) — so a
        // multi-turn session doesn't accumulate a system message per turn.
        self.ensure_system_prompt(&system_prompt);
        // A queued system note (the CLI's `--load-summary` window) lands
        // right after the prompt. Applied here — after the reset and the
        // prompt reconciliation above — because that is the only point
        // where both are guaranteed: `reset()` has already run, and
        // `ensure_system_prompt` only ever looks at index 0.
        if let Some(body) = self.pending_system_seed.take() {
            self.messages.insert(1, Message::system(&body));
        }
        self.messages.push(Message::user(task));
        self.state = AgentState::Thinking;

        // Main ReAct loop.
        //
        // v2: `iteration` counts LLM invocations (the only expensive step), not
        // raw loop iterations. State transitions (Executing -> Observing -> Thinking)
        // are free and don't consume budget. `max_tool_calls` still bounds the number
        // of tool executions. `max_run_duration` bounds the time axis; it is
        // checked at the same heads as the count budgets so every way the loop
        // can end is visible in one place.
        let run_started = std::time::Instant::now();
        while self.state != AgentState::Finished && self.state != AgentState::Error {
            if let Some(limit) = self.config.max_run_duration {
                if run_started.elapsed() >= limit {
                    self.wall_clock_exhausted = Some(limit);
                    self.emit_trace(TraceEvent::BudgetExhausted {
                        kind: "wall_clock_ms",
                        // `limit` is milliseconds; the other budgets report
                        // their own unit (iterations / calls), so the kind
                        // names this one explicitly.
                        limit: limit.as_millis().min(usize::MAX as u128) as usize,
                    });
                    break;
                }
            }

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
                    // `Error` is documented as "an unrecoverable error was
                    // raised; the loop has been aborted", and this is the one
                    // place that raises one. Leaving the state at `Thinking`
                    // (as it used to) made `RunReport.state` describe a run
                    // that just died as still in progress.
                    if let Err(e) = self.think() {
                        self.state = AgentState::Error;
                        return Err(e);
                    }
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

        match self.get_result() {
            Ok(answer) => {
                // Usually a no-op: `think` already set `Finished` when the
                // model emitted a result. It is not a no-op when the loop
                // broke on a budget and `get_result` rescued a terse answer —
                // the state was then left at whatever the last transition
                // was (`Observing`), so a run that produced an answer and
                // returned `Ok` would otherwise report itself as mid-flight
                // in `RunReport.state`. `Ok` from `run` and `Finished` from
                // `state` are the same fact.
                self.state = AgentState::Finished;
                Ok(answer)
            }
            Err(e) => {
                // The loop broke on a budget without converging, so this run
                // ends where the two error paths above do. Keeping the
                // mapping total means `run()` returning `Err` and `state()`
                // reporting `Error` are the same fact, which is what callers
                // that only read one of the two rely on.
                self.state = AgentState::Error;
                Err(e)
            }
        }
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
        let mut stats = crate::conversation::CompressionStats::default();
        // User-configurable compression (message-count slicing + tool-result
        // truncation). Can be disabled via `CompressionPolicy::disabled()`.
        if self.config.compression.max_messages > 0
            || self.config.compression.tool_content_max_chars > 0
        {
            stats = crate::conversation::compress_messages(
                &mut self.messages,
                &self.config.compression,
            );
        }
        // REQ-SCHED-001 / mem-3 (byte-GC): additionally bound the *dynamic
        // byte* footprint of the live context cache — serde_json and long
        // tool dumps can be far larger than the message count suggests, so a
        // byte budget is the real heap-blast guard. This runs **unconditionally**
        // (independent of `CompressionPolicy`) because heap-blast protection is
        // not optional: `gc_to_budget` drops the oldest non-system messages
        // until under `MAX_DYNAMIC_CONTEXT_BYTES` (512 KiB on the C61 / host,
        // 2 MiB on the S3 8 MB octal PSRAM profile), preserving the system
        // prompt and the current turn.
        let byte_stats =
            crate::conversation::gc_to_budget(&mut self.messages, crate::MAX_DYNAMIC_CONTEXT_BYTES);
        stats.dropped += byte_stats.dropped;
        stats.kept = self.messages.len();
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
                        // Fail loudly. The previous code synthesised a
                        // scripted "simulated" reply here, so an LLM call
                        // that failed mid-run (link drop, timeout, HTTP
                        // 5xx) was answered with a plausible fabricated
                        // result the caller had no way to tell apart from
                        // a real one.
                        log::error!(target: "magent.llm", "backend error: {}", e);
                        return Err(format!("LLM backend error: {e}"));
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
        //
        // A response may name several tools (an OpenAI-style `tool_calls`
        // array is how a model batches independent reads). Each one is a
        // separate assistant tool-call message paired with its own tool
        // result, so every id announced is answered — the shape DeepSeek
        // rejects the request for violating if an id goes unanswered.
        let calls = self.parse_tool_calls(&response);
        if !calls.is_empty() {
            for (name, args) in calls {
                // Stop on budget rather than mid-pair: an assistant tool-call
                // message with no matching tool result is a malformed request.
                if self.tool_call_count >= self.config.max_tool_calls {
                    break;
                }
                let tc = ToolCall {
                    name: name.clone(),
                    arguments: args.clone(),
                };
                // Allocate the call id up-front and reuse it for BOTH the
                // assistant tool-call message and the tool-result message.
                // DeepSeek rejects the request if these ids differ.
                //
                // The id comes from the session-long `call_id_seq`, not from
                // the per-run budget counter: a REPL turn must not re-issue an
                // id an earlier turn is still using in the same history.
                self.tool_call_count += 1;
                self.call_id_seq += 1;
                let call_id = format!("call_{}", self.call_id_seq);
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

    /// The simulated planner's canned "done" text assumes the tool it just
    /// called worked. When it did not — `get_sleep_quality` with no recorded
    /// night, `start_exercise` with a misspelled type — reporting that text
    /// would be a fabricated success at the very layer that exists to
    /// demonstrate the tool.
    fn last_tool_error(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Tool)
            .and_then(|m| m.content.strip_prefix("Error: "))
    }

    fn generate_simulated_response(&self) -> String {
        let user_task_lower = self.current_turn_task().to_lowercase();
        let tool_count = self.tool_call_count;

        // A tool already ran and failed. Stop and say so rather than naming the
        // task complete — the routes below only ever call one tool, so there is
        // nothing to recover into.
        if tool_count > 0 {
            if let Some(err) = self.last_tool_error() {
                // Escaped, not interpolated: tool errors carry quotes
                // (`ConfigurationError { field: "sleep", .. }`), so a raw
                // splice would emit invalid JSON that `parse_result` drops —
                // turning a reported failure back into silence.
                let text = format!("The tool did not complete: {err}");
                let quoted = serde_json::to_string(&text)
                    .unwrap_or_else(|_| r#""The tool did not complete.""#.to_string());
                return format!(r#"{{"result": {quoted}}}"#);
            }
        }

        // Note: order matters because some tasks match multiple keywords.
        // v2: more conservative keyword matching; less nested branches.
        if contains_any(&user_task_lower, &["monitor", "environment", "all sensor"]) {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "read_sensor", "args": {"sensor": "humidity"}}"#.to_string(),
                2 => r#"{"tool": "read_sensor", "args": {"sensor": "pressure"}}"#.to_string(),
                3 => r#"{"tool": "ble_send", "args": {"data": "Environmental data logged"}}"#
                    .to_string(),
                _ => r#"{"result": "Environmental monitoring complete"}"#.to_string(),
            };
        }

        if contains_any(&user_task_lower, &["read"])
            && contains_any(&user_task_lower, &["sensor", "vital"])
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
            && (contains_any(&user_task_lower, &["above"])
                || contains_any(&user_task_lower, &["fan", "cooling"]))
        {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "write_gpio", "args": {"pin": 14, "state": "high"}}"#.to_string(),
                _ => {
                    r#"{"result": "Temperature was above threshold, fan turned on at GPIO pin 14"}"#
                        .to_string()
                }
            };
        }

        if contains_any(&user_task_lower, &["humidity"])
            && !contains_any(&user_task_lower, &["temperature", "pressure"])
        {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "humidity"}"#,
                "Humidity sensor reading completed",
            );
        }
        if contains_any(&user_task_lower, &["pressure"])
            && !contains_any(&user_task_lower, &["humidity", "temperature"])
        {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "pressure"}"#,
                "Pressure sensor reading completed",
            );
        }
        if contains_any(&user_task_lower, &["temperature"])
            && !contains_any(&user_task_lower, &["humidity", "pressure"])
        {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "temperature"}"#,
                "Temperature sensor reading completed",
            );
        }

        if contains_any(&user_task_lower, &["accelerometer", "step", "motion"]) {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "accelerometer"}"#,
                "Accelerometer reading completed",
            );
        }

        // Device-status sensors. The prompt advertises `battery` and
        // `memory`, so the offline planner has to be able to reach them —
        // otherwise `--mock` answers a battery question with the generic
        // default while the executor could have served it.
        if contains_any(&user_task_lower, &["battery", "电量", "电池"]) {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "battery"}"#,
                "Battery reading completed",
            );
        }
        // A bare `memory` would hijack "read from flash memory"; the flash
        // routes below own that phrasing, so require a device-status word or
        // a `memory` that is not about flash.
        if contains_any(&user_task_lower, &["free heap", "heap", "sram", "内存"])
            || (contains_any(&user_task_lower, &["memory"])
                && !contains_any(&user_task_lower, &["flash"]))
        {
            return tool_then_done(
                tool_count,
                "read_sensor",
                r#"{"sensor": "memory"}"#,
                "Memory status reading completed",
            );
        }

        if contains_any(&user_task_lower, &["led"])
            && contains_any(&user_task_lower, &["on", "turn on", "enable"])
        {
            return tool_then_done(
                tool_count,
                "write_gpio",
                r#"{"pin": 13, "state": "high"}"#,
                "LED turned on successfully",
            );
        }
        if contains_any(&user_task_lower, &["led"])
            && contains_any(&user_task_lower, &["off", "turn off", "disable"])
        {
            return tool_then_done(
                tool_count,
                "write_gpio",
                r#"{"pin": 13, "state": "low"}"#,
                "LED turned off successfully",
            );
        }

        if contains_any(&user_task_lower, &["ble"])
            && contains_any(
                &user_task_lower,
                &["notification", "alert", "send", "message"],
            )
        {
            return tool_then_done(
                tool_count,
                "ble_send",
                r#"{"data": "Alert from mAgent"}"#,
                "BLE notification sent successfully",
            );
        }

        if contains_any(&user_task_lower, &["flash"])
            && contains_any(&user_task_lower, &["log", "save", "write"])
        {
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

        // --- Health & exercise ------------------------------------------------
        //
        // These routes cover the tools the system prompt advertises. Without
        // them every health task — "start a run", "how did I sleep", "set do
        // not disturb" — fell through to the default, which answered with a
        // *temperature* reading and reported success. The offline planner was
        // demonstrating a tool the task had not asked for.
        //
        // Ordered before the generic `["health", ...]` branch below, which
        // would otherwise swallow "health alert".
        if contains_any(&user_task_lower, &["sleep", "slept", "nap"]) {
            return tool_then_done(
                tool_count,
                "get_sleep_quality",
                "{}",
                "Sleep quality reviewed",
            );
        }

        if contains_any(
            &user_task_lower,
            &["heart rate", "heartrate", "pulse", "bpm"],
        ) {
            return tool_then_done(
                tool_count,
                "read_heart_rate",
                "{}",
                "Heart rate reading completed",
            );
        }
        if contains_any(&user_task_lower, &["stress", "stressed"]) {
            return tool_then_done(
                tool_count,
                "check_stress_level",
                "{}",
                "Stress level reading completed",
            );
        }
        if contains_any(&user_task_lower, &["glucose", "blood sugar"]) {
            return tool_then_done(
                tool_count,
                "read_glucose",
                "{}",
                "Blood glucose reading completed",
            );
        }
        if contains_any(&user_task_lower, &["ecg", "ekg", "heart rhythm"]) {
            return tool_then_done(tool_count, "read_ecg", "{}", "ECG reading completed");
        }

        if contains_any(
            &user_task_lower,
            &["meditat", "breathe", "breathing exercise"],
        ) {
            return tool_then_done(
                tool_count,
                "send_meditation",
                r#"{"script": "Breathe in for four counts, hold for four, out for six."}"#,
                "Meditation guidance sent",
            );
        }

        if contains_any(
            &user_task_lower,
            &["do not disturb", "do-not-disturb", "dnd", "quiet hours"],
        ) {
            return tool_then_done(
                tool_count,
                "set_dnd",
                r#"{"enabled": true, "start_hour": 22, "end_hour": 7}"#,
                "Do-not-disturb window set",
            );
        }

        if contains_any(&user_task_lower, &["alert", "warning"])
            && contains_any(&user_task_lower, &["send", "raise", "notify", "health"])
        {
            return tool_then_done(
                tool_count,
                "send_health_alert",
                r#"{"type": "heart_rate", "severity": 7, "message": "Heart rate outside the normal range"}"#,
                "Health alert raised",
            );
        }

        // Emergency tools. Both are advertised in the prompt's EMERGENCY
        // section and neither had a route *or* an implementation anywhere.
        // Ordered before the generic `["health", ...]` branch, which would
        // otherwise swallow "health emergency".
        if contains_any(
            &user_task_lower,
            &["emergency contact", "contact my", "notify my", "reach my"],
        ) {
            // No `name`: the executor resolves an unnamed request to the
            // registry's primary contact, the only answer it can give without
            // inventing a person.
            return tool_then_done(
                tool_count,
                "notify_emergency_contact",
                r#"{"message": "I need help, please contact me."}"#,
                "Emergency contact notified",
            );
        }
        if contains_any(
            &user_task_lower,
            &["hospital", "emergency room", "a&e", "er near"],
        ) {
            // The planner cannot know which specialties the device's hospital
            // registry holds, so it asks for the generic nearest ER; an unknown
            // specialty would be rejected rather than silently answered with a
            // different hospital.
            let specialty = if contains_any(&user_task_lower, &["cardio", "heart"]) {
                "cardiology"
            } else if contains_any(&user_task_lower, &["endocrine", "diabetes"]) {
                "endocrinology"
            } else {
                "er"
            };
            return tool_then_done(
                tool_count,
                "find_nearest_hospital",
                &format!(r#"{{"specialty": "{}"}}"#, specialty),
                "Nearest hospital found",
            );
        }

        if contains_any(
            &user_task_lower,
            &[
                "exercise",
                "workout",
                "run",
                "running",
                "jog",
                "walk",
                "cycle",
                "cycling",
                "swim",
                "yoga",
                "hiit",
                "training session",
            ],
        ) {
            // Ending, reading and adjusting come first: "adjust my exercise
            // goal" and "end my run" both contain "exercise"/"run", and the
            // start route would otherwise win.
            if contains_any(
                &user_task_lower,
                &["end", "stop", "finish", "done", "complete"],
            ) {
                return tool_then_done(tool_count, "end_exercise", "{}", "Exercise session ended");
            }
            if contains_any(&user_task_lower, &["goal", "target", "intensity"]) {
                if contains_any(
                    &user_task_lower,
                    &[
                        "adjust", "change", "update", "increase", "decrease", "set", "lower",
                        "raise",
                    ],
                ) {
                    // The prompt documents `adjust_exercise_goal(intensity,
                    // duration)`; the 0.5 below is the simulated planner's
                    // choice of what "adjust" means, not a user-supplied value.
                    return tool_then_done(
                        tool_count,
                        "adjust_exercise_goal",
                        r#"{"intensity": 0.5}"#,
                        "Exercise goal adjusted",
                    );
                }
                return tool_then_done(
                    tool_count,
                    "get_exercise_goal",
                    "{}",
                    "Exercise goal retrieved",
                );
            }
            let kind = if contains_any(&user_task_lower, &["cycle", "cycling", "bike"]) {
                "cycling"
            } else if contains_any(&user_task_lower, &["swim"]) {
                "swimming"
            } else if contains_any(&user_task_lower, &["yoga"]) {
                "yoga"
            } else if contains_any(&user_task_lower, &["hiit", "interval"]) {
                "hiit"
            } else if contains_any(&user_task_lower, &["walk"]) {
                "walking"
            } else if contains_any(&user_task_lower, &["strength", "weights"]) {
                "strength"
            } else {
                // "start a workout" names no type. The planner has to pick one
                // to call the tool at all; running is the prompt's first
                // documented type and the simulator's own default activity.
                "running"
            };
            return tool_then_done(
                tool_count,
                "start_exercise",
                &format!(r#"{{"type": "{}"}}"#, kind),
                "Exercise session started",
            );
        }

        if contains_any(&user_task_lower, &["health", "status", "verify", "system"]) {
            return match tool_count {
                0 => r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(),
                1 => r#"{"tool": "ble_send", "args": {"data": "System health OK"}}"#.to_string(),
                _ => r#"{"result": "Health check complete - all systems operational"}"#.to_string(),
            };
        }

        // Default: nothing matched. The planner used to answer with a
        // `read_sensor temperature` call and then report success — a tool the
        // task never asked for, executed against real state. It now says
        // plainly that it has no route, rather than performing an unrelated
        // action and calling it done.
        if tool_count == 0 {
            return r#"{"result": "No tool call needed for this task (simulated planner)"}"#
                .to_string();
        }
        r#"{"result": "Task completed successfully"}"#.to_string()
    }

    /// v2: parse any JSON key that maps to a tool name, not just the
    /// hard-coded 5. Falls back to the OpenAI-style `tool_calls` array as well.
    ///
    /// Returns at most the first call. Use [`Self::parse_tool_calls`] when the
    /// response may name several — the ReAct loop does.
    #[doc(hidden)]
    pub fn parse_tool_call(
        &self,
        response: &str,
    ) -> Option<(String, HashMap<String, serde_json::Value>)> {
        self.parse_tool_calls(response).into_iter().next()
    }

    /// Parse **every** tool call a response contains.
    ///
    /// The ReAct loop executes all of them; a model that batches independent
    /// reads into one OpenAI-style `tool_calls` array must not have all but the
    /// first silently dropped.
    #[doc(hidden)]
    pub fn parse_tool_calls(
        &self,
        response: &str,
    ) -> Vec<(String, HashMap<String, serde_json::Value>)> {
        let cleaned = strip_code_fence(response);

        // 1. Whole response is a JSON object that describes a tool call.
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&cleaned) {
            let found = parse_tool_calls_from_json(&json, &self.config.tool_descriptions);
            if !found.is_empty() {
                return found;
            }
        }

        // 2. The model wrapped the tool call in prose/markdown (a common
        //    format violation). Recover the first balanced JSON object and
        //    re-parse it.
        if let Some(obj) = find_json_object(&cleaned) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(obj) {
                let found = parse_tool_calls_from_json(&json, &self.config.tool_descriptions);
                if !found.is_empty() {
                    return found;
                }
            }
        }

        // 3. Anthropic-style tool use. Some providers (and some model
        //    checkpoints) emit `<invoke name="X"><parameter name="k">v</
        //    parameter></invoke>` (optionally namespace-prefixed as
        //    `<antml:invoke>`) instead of our JSON shape.
        parse_anthropic_tool_call(&cleaned).into_iter().collect()
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

    /// The task the current turn is answering: the **last** user message.
    ///
    /// Not the first: `--repl` turns share one history, so the first user
    /// message keeps describing turn one forever, and anything keyed off it
    /// answers the first question for the rest of the session. During a run
    /// no further user message is pushed, so this is stable across the
    /// iterations of one turn — which is what the keyword scripting below
    /// needs — while still moving with the session.
    fn current_turn_task(&self) -> String {
        self.messages
            .iter()
            .rev()
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

    /// Reset the per-run state machine and counters, leaving the
    /// conversation alone. Does **not** touch the runner configuration.
    fn reset_run_state(&mut self) {
        self.state = AgentState::Idle;
        self.iteration = 0;
        self.tool_call_count = 0;
        self.wall_clock_exhausted = None;
    }

    /// Reset the runner's per-run state (state machine, conversation,
    /// iteration / tool-call counters). Does **not** touch the
    /// runner configuration.
    fn reset(&mut self) {
        self.reset_run_state();
        self.messages.clear();
    }

    /// Reset the conversation history while keeping the system prompt.
    /// Useful for REPL mode when user wants to start fresh but keep config.
    pub fn reset_conversation(&mut self) {
        self.reset_run_state();
        self.messages.retain(|m| m.role == Role::System);
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

    /// Estimated dynamic heap footprint (bytes) of the current conversation
    /// history. Mirrors `conversation::estimate_bytes`; this is the figure the
    /// REQ-SCHED-001 / mem-3 byte-GC (`gc_to_budget`) bounds to
    /// `MAX_DYNAMIC_CONTEXT_BYTES`. Exposed for diagnostics so callers can see
    /// the live context-cache footprint.
    pub fn approx_total_bytes(&self) -> usize {
        crate::conversation::estimate_bytes(&self.messages)
    }

    /// Apply the configured `CompressionPolicy` to the live
    /// conversation history and return the resulting counters.
    /// Exposed for tests and CLI tooling that want to inspect / report
    /// the compression stats without running the full ReAct loop.
    pub fn compress_now(&mut self) -> crate::conversation::CompressionStats {
        let mut stats =
            crate::conversation::compress_messages(&mut self.messages, &self.config.compression);
        // REQ-SCHED-001 / mem-3: mirror the think()-path byte-GC so this
        // diagnostic path sees the same bounded footprint.
        let byte_stats =
            crate::conversation::gc_to_budget(&mut self.messages, crate::MAX_DYNAMIC_CONTEXT_BYTES);
        stats.dropped += byte_stats.dropped;
        stats.kept = self.messages.len();
        stats
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

    /// Suffix naming the wall-clock budget when *it* is what stopped the
    /// loop; empty otherwise, so a run that ended on the iteration or
    /// tool-call budget keeps its historical message.
    fn wall_clock_note(&self) -> String {
        match self.wall_clock_exhausted {
            Some(limit) => format!(", wall-clock budget of {}s exhausted", limit.as_secs_f64()),
            None => String::new(),
        }
    }

    fn get_result(&self) -> std::result::Result<String, String> {
        // The loop publishes its verdict through the state. `think` sets
        // `Finished` only when it accepted a message as the final result, and
        // it pushes that result already stripped of its `{"result": …}`
        // envelope — so on this path the answer is taken *as given*. Running
        // the text through the classifier again would second-guess the loop
        // with a weaker test: an answer of `first place` or `ok, done` came
        // out of a well-formed envelope and was accepted, but
        // `looks_like_chatter` (or the length threshold) would judge the
        // *extracted* text and fail a run that had, in fact, answered.
        if self.state == AgentState::Finished {
            if let Some(msg) = self
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
            {
                return Ok(msg.content.clone());
            }
        }

        // Otherwise the loop broke on a budget and never accepted anything.
        // Only this turn's messages may be considered: `--repl` turns share
        // one history, so an unbounded backward scan reaches into a previous
        // turn and returns its answer as this one's — a stale success. The
        // current turn begins at the last user message (the task `run_inner`
        // pushed); earlier turns' tasks sit before it.
        let turn_start = self
            .messages
            .iter()
            .rposition(|m| m.role == Role::User)
            .map_or(0, |i| i + 1);

        for msg in self.messages[turn_start..].iter().rev() {
            if msg.role == Role::Assistant && !msg.content.is_empty() {
                // Two very different kinds of text land here, and they get
                // opposite treatment.
                let text = strip_code_fence(&msg.content);
                // Mid-plan chatter — "let me check the sensor". The classifier
                // rejected it precisely because it is not an answer, so
                // printing it under the CLI's `RESULT` banner would be the
                // fabricated success `run_tool_only_exhaustion…` exists to
                // prevent. The text is quoted in the error so nothing the
                // model said is lost, only relabelled.
                if looks_like_chatter(&text) {
                    return Err(format!(
                        "run ended without a final answer: the model never emitted a \
                         result and its last message was mid-plan chatter (\"{text}\") \
                         (iterations: {}/{}, tool calls: {}/{}){}",
                        self.iteration,
                        self.config.max_iterations,
                        self.tool_call_count,
                        self.config.max_tool_calls,
                        self.wall_clock_note(),
                    ));
                }
                // A terse answer — `22.5` where the prompt asked for
                // `{"result": 22.5}`. Sloppy, but it *is* the model's answer;
                // the prose fallback in `parse_result` rejected it only for
                // being shorter than `MIN_PROSE_RESULT_LEN`. Hand it over.
                return Ok(text);
            }
        }
        // No assistant message in this turn carried any text. Reachable when
        // every iteration produced a tool call and nothing else, and the loop
        // then hit its iteration or tool-call budget. The old code returned the
        // canned string "Task completed" here — a fabricated success for a
        // run that never answered. Report the dead end instead.
        Err(format!(
            "run ended without a final answer: no assistant text was produced \
             (iterations: {}/{}, tool calls: {}/{}){}",
            self.iteration,
            self.config.max_iterations,
            self.tool_call_count,
            self.config.max_tool_calls,
            self.wall_clock_note()
        ))
    }
}

#[inline]
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Largest index `<= max` that is a UTF-8 character boundary, or `None` when
/// the whole string fits.
///
/// `str::floor_char_boundary` is still unstable, so this mirrors
/// `conversation::floor_char_boundary` (private there). Slicing a `&str` at a
/// raw byte offset panics when the offset lands inside a multi-byte code point.
fn floor_char_boundary(s: &str, max: usize) -> Option<usize> {
    if s.len() <= max {
        return None;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    Some(i)
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
mod floor_char_boundary_tests {
    use super::floor_char_boundary;

    #[test]
    fn returns_none_when_the_string_fits() {
        assert_eq!(floor_char_boundary("abc", 200), None);
        assert_eq!(floor_char_boundary("abc", 3), None);
    }

    #[test]
    fn never_lands_inside_a_multibyte_codepoint() {
        // "天气" is 3 bytes per char. Cutting at byte 4 must back off to 3.
        let s = "天气";
        let end = floor_char_boundary(s, 4).expect("longer than 4 bytes");
        assert_eq!(end, 3);
        assert_eq!(&s[..end], "天");
    }

    #[test]
    fn truncating_a_long_chinese_body_does_not_panic() {
        // Regression: the trace renderer sliced `&body[..200]`, which panicked
        // with "end byte index 200 is not a char boundary" on a Chinese LLM
        // reply whose 200th byte fell inside a character.
        let body: std::string::String = "天气".repeat(300);
        let end = floor_char_boundary(&body, 200).expect("body is longer");
        assert!(body.is_char_boundary(end));
        assert_eq!(body[..end].chars().count(), 66);
    }
}

/// A throwaway HTTP/1.1 server for the retry tests.
///
/// The retry is about *what the client does after a reply*, so a test that
/// fakes the `Response` would test nothing: the interesting behaviour is
/// "a 503 really does cause a second request, on a new connection, with
/// the same body". That needs a socket. This one is deliberately minimal —
/// one request per connection, no keep-alive — which is exactly what the
/// LLM clients ask for (`pool_max_idle_per_host(0)`).
///
/// The whole contract: one request per connection; the request is recorded
/// *before* the reply is written (so a test that has seen the reply has,
/// by the mutex's release/acquire edge, also seen the record); only
/// `Content-Length` bodies are read — `Transfer-Encoding: chunked` is not
/// implemented, and a chunked request would record an empty body and fail
/// its assertion rather than pass quietly; and the serving thread is
/// detached, so it lives until the test process exits.
#[cfg(test)]
mod http_mock {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// One scripted reply.
    #[derive(Clone)]
    pub struct Reply {
        pub status: u16,
        pub body: &'static str,
        /// Extra response headers. Empty for every ordinary reply; the only
        /// header the client under test reads is `Retry-After`.
        pub headers: Vec<(&'static str, &'static str)>,
    }

    impl Reply {
        /// An ordinary status+body reply with no headers.
        pub fn new(status: u16, body: &'static str) -> Self {
            Self {
                status,
                body,
                headers: Vec::new(),
            }
        }

        /// A reply carrying one extra response header — for headers the client
        /// is expected to *read*, like `Retry-After`.
        pub fn with_header(
            status: u16,
            body: &'static str,
            name: &'static str,
            value: &'static str,
        ) -> Self {
            Self {
                status,
                body,
                headers: vec![(name, value)],
            }
        }
    }

    /// What the server saw for one request.
    #[derive(Clone)]
    pub struct Seen {
        /// Request line and headers, verbatim (`\r\n`-separated).
        pub head: String,
        /// Request body, decoded as UTF-8.
        pub body: String,
    }

    impl Seen {
        /// The request-line + headers with the given prefix, if any.
        pub fn header(&self, name: &str) -> Option<&str> {
            let needle = format!("{}:", name.to_ascii_lowercase());
            self.head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with(&needle))
                .and_then(|l| l.split_once(':').map(|(_, v)| v.trim()))
        }
    }

    pub struct Mock {
        pub base_url: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Mock {
        /// Serve `replies[i]` to the i-th request. Past the end of the
        /// script the last reply repeats, so a test that expects *no*
        /// retry still gets a sane answer if it is wrong — it fails on the
        /// request count rather than on a hang.
        pub fn start(replies: Vec<Reply>) -> Self {
            assert!(!replies.is_empty(), "the mock needs a script");
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
            let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let index = {
                        let mut seen = recorder.lock().expect("mock recorder");
                        let index = seen.len();
                        seen.push(Seen {
                            head: String::new(),
                            body: String::new(),
                        });
                        index
                    };
                    let reply = replies
                        .get(index)
                        .or_else(|| replies.last())
                        .expect("script is non-empty")
                        .clone();
                    let read = read_request(stream.try_clone().expect("clone the socket"));
                    // Record *before* replying. A test that sees the reply
                    // has, by the unlock-then-lock edge, also seen the
                    // record — otherwise `requests()` could hand back the
                    // placeholder with an empty body.
                    recorder.lock().expect("mock recorder")[index] = read;
                    answer(stream, &reply);
                }
            });
            Self { base_url, seen }
        }

        /// Every request the server has answered, in order.
        pub fn requests(&self) -> Vec<Seen> {
            self.seen.lock().expect("mock recorder").clone()
        }

        pub fn count(&self) -> usize {
            self.seen.lock().expect("mock recorder").len()
        }
    }

    /// Drain one request off `stream` and report what was read.
    ///
    /// The whole request (headers *and* body) is read before the caller
    /// answers: replying early and closing would reset the connection while
    /// the client is still writing the body, which the client would see as
    /// a broken pipe instead of our status code.
    fn read_request(stream: TcpStream) -> Seen {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        let mut content_length = 0usize;
        // Read line by line into a scratch buffer — accumulating into
        // `head` and re-splitting it would look at the segment *after* the
        // trailing newline, which is always empty.
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            head.push_str(&line);
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line
                .trim_end()
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut bytes = vec![0u8; content_length];
        if content_length > 0 {
            // Deliberately not `let _ =`: a short read would record an
            // empty body, and the body-equality assertions would then
            // compare "" to "" and pass for the wrong reason. Fail loudly
            // instead — a truncated request means the harness is broken,
            // not the code under test.
            reader
                .read_exact(&mut bytes)
                .expect("mock: the client announced a body it did not send");
        }

        Seen {
            head,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    /// Write `reply` back on `stream` and close it.
    fn answer(mut stream: TcpStream, reply: &Reply) {
        let reason = match reply.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            503 => "Service Unavailable",
            _ => "Status",
        };
        let extra: String = reply
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        let response = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.status,
            reply.body.len(),
            reply.body,
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod llm_retry_tests {
    use super::http_mock::{Mock, Reply};
    use super::*;
    use reqwest::StatusCode as Http;

    fn ok(body: &'static str) -> Reply {
        Reply::new(200, body)
    }

    #[test]
    fn retries_only_conditions_a_second_attempt_could_clear() {
        // Transient by definition.
        assert!(is_transient_status(Http::REQUEST_TIMEOUT));
        assert!(is_transient_status(Http::TOO_MANY_REQUESTS));
        assert!(is_transient_status(Http::INTERNAL_SERVER_ERROR));
        assert!(is_transient_status(Http::BAD_GATEWAY));
        assert!(is_transient_status(Http::SERVICE_UNAVAILABLE));

        // A 5xx that is permanent: a second identical request gets the
        // same "not implemented" / "unsupported version" answer.
        assert!(!is_transient_status(Http::NOT_IMPLEMENTED));
        assert!(!is_transient_status(Http::HTTP_VERSION_NOT_SUPPORTED));

        // Deterministic — the same request would get the same answer, so
        // retrying would only delay the (identical) error.
        assert!(!is_transient_status(Http::BAD_REQUEST));
        assert!(!is_transient_status(Http::UNAUTHORIZED));
        assert!(!is_transient_status(Http::FORBIDDEN));
        assert!(!is_transient_status(Http::NOT_FOUND));
        assert!(!is_transient_status(Http::UNPROCESSABLE_ENTITY));
        assert!(!is_transient_status(Http::OK));
    }

    #[test]
    fn backoff_doubles_per_attempt_and_is_capped() {
        let ms = |a| retry_backoff(a).as_millis() as u64;
        assert_eq!(ms(0), LLM_RETRY_BASE_MS);
        assert_eq!(ms(1), LLM_RETRY_BASE_MS * 2);
        assert_eq!(ms(2), LLM_RETRY_BASE_MS * 4);
        assert_eq!(ms(3), LLM_RETRY_MAX_MS);
        // Stays capped instead of overflowing on an absurd attempt count.
        assert_eq!(ms(1_000), LLM_RETRY_MAX_MS);
    }

    /// "Retries must be visible" was the point of the warning, so the
    /// message is pinned: which attempt, out of how many, what failed, and
    /// how long the caller is about to wait. The wait is passed in rather
    /// than recomputed, so a `Retry-After` is reported as it will be slept.
    #[test]
    fn a_retry_warning_names_the_attempt_the_cause_and_the_wait() {
        let label = "ollama /api/chat";
        assert_eq!(
            retry_warning(label, 0, LLM_MAX_RETRIES, "HTTP 503", retry_backoff(0)),
            "ollama /api/chat: attempt 1/3 failed (HTTP 503); retrying in 250ms",
        );
        assert_eq!(
            retry_warning(
                label,
                1,
                LLM_MAX_RETRIES,
                "connection refused",
                retry_backoff(1)
            ),
            "ollama /api/chat: attempt 2/3 failed (connection refused); retrying in 500ms",
        );
    }

    /// The `delta-seconds` form is the number itself, and a value in neither
    /// form falls back to the backoff instead of to "retry now".
    #[test]
    fn retry_after_parses_delta_seconds() {
        use std::time::Duration;
        let now = std::time::SystemTime::UNIX_EPOCH;
        assert_eq!(parse_retry_after("30", now), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 5 ", now), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("", now), None);
        assert_eq!(parse_retry_after("-1", now), None);
    }

    /// The `HTTP-date` form is converted to a delta against `now`. `now` is
    /// injected so the conversion is exact rather than racing the clock.
    #[test]
    fn retry_after_parses_the_http_date_form() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |unix: i64| UNIX_EPOCH + Duration::from_secs(unix as u64);
        // 2015-10-21T07:28:00Z — the RFC 7231 example instant.
        const FIXDATE: i64 = 1_445_412_480;
        // 1994-11-06T08:49:37Z — a Sunday, for the obsolete two forms.
        const OLD: i64 = 784_111_777;

        // IMF-fixdate: the delta to the named instant.
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", at(FIXDATE - 30)),
            Some(Duration::from_secs(30)),
        );
        // A date already in the past floors at zero, which `retry_delay` then
        // raises back to the backoff — the header never shortens a wait.
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", at(FIXDATE)),
            Some(Duration::ZERO),
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", at(FIXDATE + 3_600)),
            Some(Duration::ZERO),
        );

        // asctime — single-digit day padded with two spaces.
        assert_eq!(
            parse_retry_after("Sun Nov  6 08:49:37 1994", at(OLD - 60)),
            Some(Duration::from_secs(60)),
        );

        // RFC 850 — the obsolete two-digit-year form.
        assert_eq!(
            parse_retry_after("Sunday, 06-Nov-94 08:49:37 GMT", at(OLD - 60)),
            Some(Duration::from_secs(60)),
        );

        // Malformed dates still degrade to the backoff.
        assert_eq!(
            parse_retry_after("Wed, 21 Xxx 2015 07:28:00 GMT", at(0)),
            None
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 25:28:00 GMT", at(0)),
            None
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 PST", at(0)),
            None
        );
        assert_eq!(parse_retry_after("21 Oct 2015 07:28:00 GMT", at(0)), None);
    }

    /// The RFC 850 two-digit year follows the RFC 7231 rule: a year more than
    /// 50 years ahead of `now` is read as the most recent past year with the
    /// same last two digits.
    #[test]
    fn an_rfc850_two_digit_year_is_resolved_against_now() {
        // now = 2026-01-01T00:00:00Z.
        const NOW_2026: i64 = 1_767_225_600;
        // "94" is not more than 50 years ahead of 2026 → 1994.
        assert_eq!(rfc850_year("94", NOW_2026), Some(1994));
        // "80" would be 2080, more than 50 years ahead → 1980.
        assert_eq!(rfc850_year("80", NOW_2026), Some(1980));
        // "10" is a plausible near-future year → 2010.
        assert_eq!(rfc850_year("10", NOW_2026), Some(2010));
        // The two-digit field is exactly two digits.
        assert_eq!(rfc850_year("1994", NOW_2026), None);
        assert_eq!(rfc850_year("9x", NOW_2026), None);
    }

    /// The civil-to-Unix conversion must agree with the core library's
    /// Unix-to-civil conversion, which is the direction that already had a
    /// tested implementation.
    #[test]
    fn http_date_conversion_matches_unix_to_calendar() {
        for unix in [0i64, 784_111_777, 1_445_412_480, 1_700_000_000] {
            let (y, mo, d, h, mi, s) = crate::time_sync::unix_to_calendar(unix).expect("in range");
            assert_eq!(
                days_from_civil(y as i64, mo, d) * 86_400
                    + h as i64 * 3600
                    + mi as i64 * 60
                    + s as i64,
                unix,
            );
        }
    }

    /// The header must never *shorten* our own backoff, must win when it asks
    /// for longer, and must be capped — it comes from a remote server.
    #[test]
    fn a_retry_after_extends_the_backoff_but_is_capped() {
        use std::time::Duration;
        let ms = |d: Duration| d.as_millis() as u64;

        // No hint: the exponential backoff, unchanged.
        assert_eq!(ms(retry_delay(0, None)), LLM_RETRY_BASE_MS);
        assert_eq!(ms(retry_delay(1, None)), LLM_RETRY_BASE_MS * 2);

        // A longer hint replaces the backoff — this is the 429 case.
        assert_eq!(ms(retry_delay(0, Some(Duration::from_secs(30)))), 30_000);

        // A shorter hint does not shorten it: `Retry-After: 0` from a limiter
        // still shedding load must not become a busy retry loop.
        assert_eq!(ms(retry_delay(3, Some(Duration::ZERO))), LLM_RETRY_MAX_MS);

        // Capped, so a hostile endpoint cannot park the run for a day.
        assert_eq!(
            ms(retry_delay(0, Some(Duration::from_secs(86_400)))),
            LLM_RETRY_AFTER_MAX_MS
        );
    }

    /// The header has to survive the trip off the wire, which is the half of
    /// the fix a pure-function test cannot see.
    #[test]
    fn retry_after_is_read_off_the_response_headers() {
        let now = std::time::SystemTime::UNIX_EPOCH;
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_hint(&headers, now), None);

        headers.insert(
            reqwest::header::RETRY_AFTER,
            "12".parse().expect("a valid header value"),
        );
        assert_eq!(
            retry_after_hint(&headers, now),
            Some(std::time::Duration::from_secs(12))
        );

        // The date form off the wire, against the very instant it names.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT"
                .parse()
                .expect("a valid header value"),
        );
        assert_eq!(
            retry_after_hint(
                &headers,
                std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_445_412_480 - 9)
            ),
            Some(std::time::Duration::from_secs(9))
        );
    }

    /// End to end: a `429` that asks for a specific wait must be waited out
    /// for that long, not for our own 250 ms guess — the guess is what made
    /// the old code burn all its attempts inside a second and report a
    /// failure the server had already told it how to avoid.
    #[test]
    fn a_429_with_retry_after_is_waited_out_for_the_header_value() {
        let mock = Mock::start(vec![
            Reply::with_header(429, "slow down", "Retry-After", "7"),
            Reply::new(200, "{}"),
        ]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let mut waits: Vec<std::time::Duration> = Vec::new();
        let response = send_with_retry_sleeping(
            "test",
            LLM_MAX_RETRIES,
            || client.get(&url).send(),
            |d| waits.push(d),
        )
        .expect("the retry succeeded");

        assert_eq!(response.status(), 200);
        assert_eq!(mock.count(), 2, "one 429 plus one retry");
        assert_eq!(
            waits,
            vec![std::time::Duration::from_secs(7)],
            "the wait must be the server's, not our backoff"
        );
    }

    /// End to end for the date form: a `429` whose `Retry-After` is an
    /// HTTP-date is waited out for the delta to that instant, so the
    /// conversion is wired into the retry path and not just unit-tested.
    #[test]
    fn a_429_with_a_date_retry_after_is_waited_out_for_the_delta() {
        // Format `now + 7s` as an IMF-fixdate. The wait is measured against
        // the clock the client reads a moment later, so the delta is 7 at
        // best and one second less if the request straddles a tick.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a post-epoch clock")
            .as_secs() as i64;
        let (y, mo, d, h, mi, s) =
            crate::time_sync::unix_to_calendar(now + 7).expect("a plausible date");
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let wd = DAYS[(((now / 86_400) % 7 + 4) % 7) as usize];
        let stamp: &'static str = Box::leak(
            format!(
                "{wd}, {d:02} {} {y:04} {h:02}:{mi:02}:{s:02} GMT",
                MONTHS[(mo - 1) as usize]
            )
            .into_boxed_str(),
        );

        let mock = Mock::start(vec![
            Reply::with_header(429, "slow down", "Retry-After", stamp),
            Reply::new(200, "{}"),
        ]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let mut waits: Vec<std::time::Duration> = Vec::new();
        send_with_retry_sleeping(
            "test",
            LLM_MAX_RETRIES,
            || client.get(&url).send(),
            |d| waits.push(d),
        )
        .expect("the retry succeeded");

        assert_eq!(mock.count(), 2, "one 429 plus one retry");
        let waited = waits.first().expect("one wait").as_secs();
        assert!(
            (4..=7).contains(&waited),
            "the wait must come from the date, not the 0.25s backoff (waited {waited}s)"
        );
    }

    /// The control for the test above: without the header the same script
    /// waits the ordinary backoff. Without this, "waited 7 s" could be
    /// satisfied by a client that always waits 7 s.
    #[test]
    fn a_429_without_the_header_keeps_the_ordinary_backoff() {
        let mock = Mock::start(vec![Reply::new(429, "slow down"), Reply::new(200, "{}")]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let mut waits: Vec<std::time::Duration> = Vec::new();
        send_with_retry_sleeping(
            "test",
            LLM_MAX_RETRIES,
            || client.get(&url).send(),
            |d| waits.push(d),
        )
        .expect("the retry succeeded");

        assert_eq!(waits, vec![retry_backoff(0)]);
        assert_eq!(mock.count(), 2);
    }

    #[test]
    fn a_transient_status_is_retried_until_one_succeeds() {
        let mock = Mock::start(vec![Reply::new(503, "upstream is restarting"), ok("{}")]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let response = send_with_retry("test", LLM_MAX_RETRIES, || client.get(&url).send())
            .expect("the second attempt succeeded");
        assert_eq!(response.status(), 200);
        assert_eq!(mock.count(), 2, "one failed attempt plus one retry");
    }

    #[test]
    fn a_deterministic_status_is_not_retried() {
        let mock = Mock::start(vec![Reply::new(400, "bad request")]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let response = send_with_retry("test", LLM_MAX_RETRIES, || client.get(&url).send())
            .expect("a 400 is a reply, not a transport failure");
        assert_eq!(response.status(), 400);
        assert_eq!(mock.count(), 1, "a 400 must not be retried");
    }

    #[test]
    fn a_permanent_5xx_is_not_retried() {
        let mock = Mock::start(vec![Reply::new(501, "not implemented")]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let response = send_with_retry("test", LLM_MAX_RETRIES, || client.get(&url).send())
            .expect("a 501 is a reply");
        assert_eq!(response.status(), 501);
        assert_eq!(mock.count(), 1, "a 501 will 501 again — do not retry");
    }

    /// When the retry budget runs out the *last* `Response` is handed back
    /// rather than an error, so the caller reports the real status (and,
    /// for DeepSeek, the body snippet) instead of a generic "request
    /// failed".
    #[test]
    fn an_exhausted_budget_returns_the_last_response() {
        let mock = Mock::start(vec![
            Reply::new(503, "still down"),
            Reply::new(503, "still down"),
        ]);
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/chat", mock.base_url);

        let response = send_with_retry("test", 1, || client.get(&url).send())
            .expect("exhaustion returns the response, not an error");
        assert_eq!(response.status(), 503);
        assert_eq!(mock.count(), 2, "1 + 1 retry");
    }

    /// A closed port gives a real transport-level `reqwest::Error`
    /// (connection refused) with no server code needed.
    #[test]
    fn a_transport_error_is_retried_then_reported_with_the_attempt_count() {
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = probe.local_addr().expect("addr").port();
            drop(probe); // nothing listens there now
            port
        };
        let client = reqwest::blocking::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/chat");

        let err = send_with_retry("test", 1, || client.get(&url).send())
            .expect_err("nothing is listening");
        assert!(err.contains("after 2 attempts"), "got {err}");
        // `reqwest`'s `Display` for a connection error is generic, so the
        // URL is the part that proves the cause was carried, not replaced.
        assert!(
            err.contains(&port.to_string()),
            "the cause must survive the wrap: {err}",
        );
    }

    #[test]
    fn with_no_retries_a_single_attempt_failure_is_reported_plainly() {
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = probe.local_addr().expect("addr").port();
            drop(probe);
            port
        };
        let client = reqwest::blocking::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/chat");

        let err = send_with_retry("test", 0, || client.get(&url).send())
            .expect_err("nothing is listening");
        assert!(err.starts_with("Request failed: "), "got {err}");
        assert!(!err.contains("after"), "no apology for one attempt: {err}");
    }

    /// The probe is the run's *first* network call and its answer is
    /// load-bearing — `false` aborts the run with "backend unreachable",
    /// or freezes the CLI into simulated answers — so it has to survive
    /// the same blip a chat call does. Un-retried, the chat-path retry
    /// could never help: the run never got that far.
    #[test]
    fn the_ollama_probe_survives_a_transient_503() {
        let mock = Mock::start(vec![
            Reply::new(503, "{\"error\":\"starting up\"}"),
            ok("{\"models\":[]}"),
        ]);
        let c = OllamaClient::new(&mock.base_url, "qwen2.5:3b");

        assert!(c.check_connection(), "the retry rescued the probe");
        assert_eq!(mock.count(), 2, "one failed probe plus one retry");
    }

    #[test]
    fn the_deepseek_probe_survives_a_transient_503() {
        let mock = Mock::start(vec![
            Reply::new(503, "{\"error\":\"overloaded\"}"),
            ok("{\"data\":[]}"),
        ]);
        let c = DeepSeekClient::with_endpoint(&mock.base_url, "deepseek-chat", "sk-x");

        assert!(c.check_connection(), "the retry rescued the probe");
        assert_eq!(mock.count(), 2, "one failed probe plus one retry");
    }

    /// Retrying the probe must not make "down" mean "keep trying": a
    /// server that is genuinely unavailable still reports not-connected,
    /// after a bounded number of attempts.
    #[test]
    fn a_probe_that_never_recovers_is_still_reported_not_connected() {
        let mock = Mock::start(vec![Reply::new(503, "down")]);
        let c = OllamaClient::new(&mock.base_url, "qwen2.5:3b");

        assert!(!c.check_connection());
        assert_eq!(
            mock.count(),
            LLM_MAX_RETRIES as usize + 1,
            "the probe must spend its budget and stop, not retry forever",
        );
    }

    /// The end-to-end case that motivated the whole thing: Ollama answers a
    /// `503` and then the real reply. The retry must be invisible to
    /// `chat_with_messages` — same parsed `content`, one request more.
    #[test]
    fn ollama_chat_recovers_from_a_transient_503() {
        let mock = Mock::start(vec![
            Reply::new(503, "{\"error\":\"model is loading\"}"),
            ok("{\"message\":{\"role\":\"assistant\",\"content\":\"hello\"}}"),
        ]);

        let mut c = OllamaClient::new(&mock.base_url, "qwen2.5:3b");
        let reply = c
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect("the retry should have rescued this call");

        assert_eq!(reply, "hello");
        assert_eq!(mock.count(), 2);
    }

    /// The same for DeepSeek, plus the property the `body.clone()` in the
    /// retry closure exists to guarantee: the second attempt carries the
    /// *identical* body and the `Authorization` header, even though the
    /// first attempt consumed the original.
    #[test]
    fn deepseek_chat_retries_with_the_same_body_and_auth_header() {
        let mock = Mock::start(vec![
            Reply::new(503, "{\"error\":\"overloaded\"}"),
            ok("{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hello\"}}]}"),
        ]);

        let mut c = DeepSeekClient::with_endpoint(&mock.base_url, "deepseek-chat", "sk-secret");
        let reply = c
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect("the retry should have rescued this call");
        assert_eq!(reply, "hello");

        let seen = mock.requests();
        assert_eq!(seen.len(), 2, "one failed attempt plus one retry");
        assert_eq!(
            seen[0].body, seen[1].body,
            "the retry must re-send the same body",
        );
        assert!(seen[0].body.contains("\"messages\""), "{}", seen[0].body);
        assert_eq!(
            seen[0].header("authorization"),
            Some("Bearer sk-secret"),
            "the first attempt lost its auth header",
        );
        assert_eq!(
            seen[1].header("authorization"),
            Some("Bearer sk-secret"),
            "the retry lost its auth header — this is the bug body.clone() prevents",
        );
    }
}

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

    /// See [`ollama_client_build_failure_is_reported_not_swallowed`].
    #[test]
    fn deepseek_client_build_failure_is_reported_not_swallowed() {
        let mut c = DeepSeekClient::with_endpoint("https://api.example/v1", "m", "sk-x");
        c.client = Err("could not build the HTTP client: no TLS backend".to_string());

        let err = c
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect_err("a client that failed to build must not look usable");
        assert!(err.contains("no TLS backend"), "got {err}");

        assert!(!c.check_connection(), "probe must report not-connected");
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

    /// The `LlmBackend` impl forwards each method to the inherent one.
    /// Driving them through a `&mut dyn LlmBackend` (the shape the runner
    /// holds) exercises the delegating bodies without touching the
    /// network: a client that failed to build reports the build error from
    /// the delegated `chat_with_messages`, and the accessors mirror the
    /// inherent getters.
    #[test]
    fn llm_backend_delegations_forward_to_the_inherent_methods() {
        let mut c = DeepSeekClient::with_endpoint("https://api.example/v1", "deepseek-chat", "sk-x");
        c.client = Err("could not build the HTTP client: no TLS backend".to_string());
        let backend: &mut dyn LlmBackend = &mut c;

        assert!(!backend.check_connection(), "delegated probe");
        let err = backend
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect_err("delegated chat must forward the build error");
        assert!(err.contains("no TLS backend"), "got {err}");
        assert_eq!(backend.provider(), LlmProvider::DeepSeek);
        assert_eq!(backend.model(), "deepseek-chat");
        assert_eq!(backend.base_url(), "https://api.example/v1");
    }

    /// `Redacted<String>` derefs to the inner `String`; both the shared and
    /// mutable deref paths must forward. Building a client and running its
    /// model accessor implicitly derefs the redacted `api_key` field's
    /// sibling — exercise the wrapper directly to pin both impls.
    #[test]
    fn redacted_wrapper_derefs_to_the_inner_value() {
        let mut r = Redacted(String::from("hello"));
        // `Deref`.
        assert_eq!(r.len(), 5);
        assert_eq!(&*r, "hello");
        // `DerefMut`.
        r.push_str(" world");
        assert_eq!(&*r, "hello world");
        // `Debug` still redacts.
        assert_eq!(format!("{r:?}"), "\"<redacted>\"");
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
        assert_eq!(parsed["messages"][1]["content"], "Read the temperature");
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
        assert!(DeepSeekClient::try_with_endpoint("https://x/v1", "m", "").is_none());
        assert!(DeepSeekClient::try_with_endpoint("https://x/v1", "m", "\t\n").is_none());
        assert!(DeepSeekClient::try_with_endpoint("https://x/v1", "m", "sk-abc").is_some());
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
        let c = DeepSeekClient::with_endpoint("http://127.0.0.1:1", "deepseek-chat", "");
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
            arguments: serde_json::from_value(serde_json::json!({"sensor": "temperature"}))
                .unwrap(),
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
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert_eq!(parsed["messages"][0]["role"], "assistant");
        assert!(parsed["messages"][0]["content"].is_null());
        let tool_calls = parsed["messages"][0]["tool_calls"]
            .as_array()
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_42");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "read_sensor");
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
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
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
            .write_chat_body_for_test(&[Message::user("hi")], SamplingParams::default())
            .expect("write body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
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
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
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
        let raw = DeepSeekClient::with_endpoint("https://x/v1", "m", "  sk-abc  ");
        assert_eq!(raw.api_key(), "  sk-abc  ");
        let trimmed = DeepSeekClient::try_with_endpoint("https://x/v1", "m", "  sk-abc  ")
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

    /// A client that could not be built used to be swallowed by
    /// `unwrap_or_else(|_| Client::new())` — which is not a fallback: it
    /// reruns the same TLS initialisation (`Client::new` *panics* on
    /// failure) and silently drops the 60-second timeout, the one setting
    /// that keeps a stalled connection from hanging the agent. The error
    /// is now carried and reported like any other transport failure, and
    /// the two best-effort probes still answer "not connected".
    #[test]
    fn ollama_client_build_failure_is_reported_not_swallowed() {
        let mut c = OllamaClient::new("http://127.0.0.1:1", "llama3.2");
        c.client = Err("could not build the HTTP client: no TLS backend".to_string());

        let err = c
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect_err("a client that failed to build must not look usable");
        assert!(err.contains("no TLS backend"), "got {err}");

        assert!(!c.check_connection(), "probe must report not-connected");
        assert!(c.get_models().is_empty(), "probe must return no models");
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
        let parsed: serde_json::Value = serde_json::from_slice(&buf).expect("parse JSON");
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
        let config = RunnerConfig {
            probe_ollama_on_run: false,
            compression: policy,
            ..Default::default()
        };
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
        runner
            .messages
            .push(Message::tool("c1", &"x".repeat(5_000)));
        for i in 0..50 {
            runner
                .messages
                .push(Message::assistant_text(&format!("a{}", i)));
        }
        let stats = runner.compress_now();
        assert_eq!(stats.dropped, 0, "disabled policy must not drop");
        assert_eq!(
            stats.tool_results_truncated, 0,
            "disabled policy must not truncate"
        );
        assert_eq!(runner.messages().len(), 52);
    }

    #[test]
    fn policy_truncates_long_tool_results() {
        let mut runner = runner_with_policy(CompressionPolicy {
            max_messages: 0, // disabled
            tool_content_max_chars: 100,
        });
        runner
            .messages
            .push(Message::tool("c1", &"y".repeat(2_000)));
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
            runner
                .messages
                .push(Message::assistant_text(&format!("a{}", i)));
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
    fn compress_now_bounds_byte_footprint() {
        // REQ-SCHED-001 / mem-3: a conversation whose *byte* footprint exceeds
        // MAX_DYNAMIC_CONTEXT_BYTES must be GC'd down below it, while the most
        // recent turn is preserved. Use a disabled message-count policy so this
        // isolates the byte-GC step.
        //
        // The history is sized *from the budget under test*, not hard-coded:
        // the ceiling is 512 KiB on the C61 but 2 MiB under `s3-8mb-psram`
        // (the ESP32-S3 profile), so the old fixed 6 × 200 KiB ≈ 1.2 MiB
        // fixture sat *below* the S3 ceiling — the precondition below then
        // failed and the S3 byte-GC bound was never actually exercised.
        const PER_MESSAGE: usize = 200 * 1024;
        let budget = crate::MAX_DYNAMIC_CONTEXT_BYTES;
        let count = budget / PER_MESSAGE + 2;
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        for _ in 0..count {
            runner
                .messages
                .push(Message::assistant_text(&"x".repeat(PER_MESSAGE)));
        }
        assert!(
            crate::conversation::estimate_bytes(&runner.messages) > budget,
            "fixture must exceed the budget under test"
        );
        let stats = runner.compress_now();
        assert!(stats.dropped > 0);
        assert!(
            crate::conversation::estimate_bytes(&runner.messages)
                <= crate::MAX_DYNAMIC_CONTEXT_BYTES
        );
        // The most recent turn is never dropped.
        assert_eq!(
            runner.messages.last().map(|m| m.content.len()),
            Some(PER_MESSAGE)
        );
    }

    /// The raised ceiling the S3 profile buys, exercised directly.
    ///
    /// The `s3-8mb-psram` profile lifts the byte budget from 512 KiB (C61) to
    /// 2 MiB. That is only meaningful if a history in between is then *kept*
    /// instead of GC'd — which is exactly what the byte-GC does, and what no
    /// test checked before (the profile's tests were not in the sweep at all).
    #[cfg(feature = "s3-8mb-psram")]
    #[test]
    fn s3_profile_keeps_a_history_the_c61_would_drop() {
        assert_eq!(crate::MAX_DYNAMIC_CONTEXT_BYTES, 2 * 1024 * 1024);
        let mut runner = runner_with_policy(CompressionPolicy::disabled());
        // ~1.2 MiB: over the C61's 512 KiB ceiling, under the S3's 2 MiB.
        for _ in 0..6 {
            runner
                .messages
                .push(Message::assistant_text(&"x".repeat(200 * 1024)));
        }
        let before = runner.messages.len();
        let stats = runner.compress_now();
        assert_eq!(stats.dropped, 0, "S3 budget must not GC a 1.2 MiB history");
        assert_eq!(runner.messages.len(), before);
        assert!(
            crate::conversation::estimate_bytes(&runner.messages)
                <= crate::MAX_DYNAMIC_CONTEXT_BYTES
        );
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
    use std::sync::{Arc, Mutex};

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
        // `Rc<RefCell<_>>` would make the closure non-`Send`, so the
        // boxed `FnSink` couldn't be installed behind `BoxedTraceSink =
        // Box<dyn TraceSink + Send + Sync>`. Tests use `Arc<Mutex<_>>`
        // even though they're single-threaded, so the typed sink can
        // be shared across threads (e.g. inside the CLI's `Arc`).
        let captured: Arc<Mutex<Vec<TraceEvent>>> = Default::default();
        let captured_clone = captured.clone();
        let mut sink = FnSink::new(move |event| {
            captured_clone.lock().unwrap().push(event);
        });
        sink.event(TraceEvent::FinalResult {
            body: "done".to_string(),
        });
        assert_eq!(captured.lock().unwrap().len(), 1);
        assert_eq!(
            captured.lock().unwrap()[0],
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
        let captured: Arc<Mutex<Vec<TraceEvent>>> = Default::default();
        let captured_c = captured.clone();
        shared.install(Box::new(FnSink::new(move |event| {
            captured_c.lock().unwrap().push(event);
        })));
        shared.with_sinks(|sinks| {
            for s in sinks.iter_mut() {
                s.event(TraceEvent::RunStart {
                    task: "t".to_string(),
                });
            }
        });
        assert_eq!(captured.lock().unwrap().len(), 1);
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
        let captured_a: Arc<Mutex<usize>> = Default::default();
        let captured_b: Arc<Mutex<usize>> = Default::default();
        let ca = captured_a.clone();
        let cb = captured_b.clone();
        shared.install(Box::new(FnSink::new(move |_| {
            *ca.lock().unwrap() += 1;
        })));
        shared.install(Box::new(FnSink::new(move |_| {
            *cb.lock().unwrap() += 1;
        })));
        shared.with_sinks(|sinks| {
            for s in sinks.iter_mut() {
                s.event(TraceEvent::Observing);
            }
        });
        assert_eq!(*captured_a.lock().unwrap(), 1, "first sink got the event");
        assert_eq!(
            *captured_b.lock().unwrap(),
            1,
            "second sink got the event too"
        );
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
        let cfg = RunnerConfig {
            probe_ollama_on_run: false,
            compression: policy,
            ..Default::default()
        };
        let mut exec = crate::real_tools::SimulatorExecutor::new();
        exec.connect_ble();
        RealAgentRunner::with_config(exec, cfg)
    }

    #[test]
    fn runner_with_sink_runs_events_through_it() {
        let shared = Arc::new(SharedTraceSink::new());
        let captured: Arc<Mutex<Vec<TraceEvent>>> = Default::default();
        let captured_c = captured.clone();
        shared.install(Box::new(FnSink::new(move |event| {
            captured_c.lock().unwrap().push(event);
        })));

        let mut runner = runner_with_sink_and_policy(CompressionPolicy::disabled());
        runner.set_trace_sink(Some(shared));
        runner.set_verbose(false); // ensure fallback path is silent
                                   // Drive the runner through the simulated path so no
                                   // network probe happens.
        let result = runner.run("noop");
        assert!(result.is_ok(), "simulated run must succeed");
        let events = captured.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TraceEvent::RunStart { .. })),
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

#[cfg(test)]
mod parse_tolerance_tests {
    use super::*;
    use crate::real_tools::SimulatorExecutor;

    fn runner() -> RealAgentRunner<SimulatorExecutor> {
        RealAgentRunner::new(SimulatorExecutor::new())
    }

    #[test]
    fn parse_tool_call_json_object() {
        let r = runner();
        let (name, args) = r
            .parse_tool_call(r#"{"tool":"read_sensor","args":{"sensor":"temp"}}"#)
            .expect("parse tool call");
        assert_eq!(name, "read_sensor");
        assert_eq!(args.get("sensor").and_then(|v| v.as_str()), Some("temp"));
    }

    #[test]
    fn parse_tool_call_wrapped_in_fence_and_prose() {
        // A model wrapped the tool call in a fenced code block and prose.
        let r = runner();
        let resp = "Here is the call:\n```json\n{\"tool\":\"get_balance\",\"args\":{\"address\":\"0xabc\"}}\n```\n";
        let (name, args) = r.parse_tool_call(resp).expect("recover wrapped call");
        assert_eq!(name, "get_balance");
        assert!(args.contains_key("address"));
    }

    #[test]
    fn parse_tool_call_anthropic_format() {
        let r = runner();
        let resp = r#"<antml:invoke name="fetch_url"><antml:parameter name="url">https://x</antml:parameter></antml:invoke>"#;
        let (name, args) = r.parse_tool_call(resp).expect("anthropic call");
        assert_eq!(name, "fetch_url");
        assert_eq!(args.get("url").and_then(|v| v.as_str()), Some("https://x"));
    }

    #[test]
    fn parse_tool_call_returns_none_for_plain_answer() {
        let r = runner();
        assert!(r
            .parse_tool_call("The temperature is 25 degrees.")
            .is_none());
    }

    /// The generic `{"<key>": {...}}` fallback used to read any object-valued
    /// key as a tool name, so an answer plus a data object was executed as a
    /// call to a tool named `data` — and tool parsing ran before result
    /// parsing, discarding the real answer.
    #[test]
    fn parse_tool_call_does_not_mistake_an_answer_for_a_tool_call() {
        let r = runner();
        let resp = r#"{"result":"The temperature is 25.5 C","data":{"sensor":"temperature"}}"#;
        assert!(r.parse_tool_calls(resp).is_empty(), "must not parse `data`");
        assert_eq!(
            r.parse_result(resp).as_deref(),
            Some("The temperature is 25.5 C")
        );
    }

    /// A bare object keyed by an unknown name with no `args` is data, not a
    /// tool call.
    #[test]
    fn parse_tool_call_ignores_unknown_bare_object_keys() {
        let r = runner();
        assert!(r
            .parse_tool_calls(r#"{"data":{"sensor":"temperature"}}"#)
            .is_empty());
    }

    /// OpenAI's single-call shape without the `tool_calls` array. The old
    /// fallback picked the alphabetically first key (`arguments`) as the tool
    /// name.
    #[test]
    fn parse_tool_call_single_openai_object() {
        let r = runner();
        let calls = r.parse_tool_calls(
            r#"{"name":"read_sensor","arguments":{"sensor":"temperature"}}"#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_sensor");
        assert_eq!(
            calls[0].1.get("sensor").and_then(|v| v.as_str()),
            Some("temperature")
        );
    }

    #[test]
    fn parse_result_json_envelope() {
        let r = runner();
        assert_eq!(r.parse_result(r#"{"result":"42"}"#).as_deref(), Some("42"));
        assert_eq!(r.parse_result(r#"{"result":42}"#).as_deref(), Some("42"));
        assert_eq!(
            r.parse_result(r#"{"result":true}"#).as_deref(),
            Some("true")
        );
    }

    #[test]
    fn parse_result_fenced_block() {
        let r = runner();
        let out = r.parse_result("```\nlet x = 1;\n```").unwrap();
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn parse_result_embedded_json_in_prose() {
        let r = runner();
        let out = r
            .parse_result("The answer is {\"result\":\"7\"} if that helps.")
            .unwrap();
        assert_eq!(out, "7");
    }

    #[test]
    fn parse_result_plain_prose_answer() {
        let r = runner();
        let out = r
            .parse_result("The final temperature reading is 25.5 degrees Celsius.")
            .unwrap();
        assert!(out.contains("25.5"));
    }

    #[test]
    fn parse_result_rejects_short_chatter() {
        let r = runner();
        assert!(r.parse_result("ok").is_none());
        assert!(r.parse_result("Let me check").is_none());
        assert!(r.parse_result("").is_none());
        // Chatter prefixes are filtered even when long enough.
        assert!(r
            .parse_result("Now I will compute the result for you.")
            .is_none());
    }

    #[test]
    fn find_json_object_skips_braces_in_strings() {
        // A `}` inside a JSON string value must not terminate the object.
        let s = r#"prefix {"tool":"a","note":"has } brace"}"#;
        let obj = find_json_object(s).expect("balanced object");
        assert!(obj.contains("\"note\":\"has } brace\""));
    }

    #[test]
    fn ensure_system_prompt_noop_when_matching() {
        let mut r = runner();
        r.messages.push(Message::system("SYS"));
        r.ensure_system_prompt("SYS");
        assert_eq!(r.messages().len(), 1);
        assert_eq!(r.messages()[0].content, "SYS");
    }

    #[test]
    fn ensure_system_prompt_replaces_stale_prompt() {
        let mut r = runner();
        r.messages.push(Message::system("old"));
        r.messages.push(Message::user("task"));
        r.ensure_system_prompt("new prompt");
        assert_eq!(r.messages().len(), 2, "must replace in place, not append");
        assert_eq!(r.messages()[0].content, "new prompt");
        assert_eq!(r.messages()[1].role, Role::User);
    }

    #[test]
    fn ensure_system_prompt_inserts_when_missing() {
        let mut r = runner();
        r.messages.push(Message::user("task"));
        r.ensure_system_prompt("SYS");
        assert_eq!(r.messages()[0].role, Role::System);
        assert_eq!(r.messages()[0].content, "SYS");
        assert_eq!(r.messages()[1].role, Role::User);
    }

    #[test]
    fn current_turn_task_returns_the_last_user_message() {
        // The scaffold's keyword scripting is keyed off this, and a REPL
        // session reuses one history — "the first user message" would keep
        // answering turn one's question for the rest of the session.
        let mut r = runner();
        r.messages.push(Message::system("SYS"));
        r.messages.push(Message::user("first"));
        r.messages.push(Message::assistant_text("hi"));
        r.messages.push(Message::user("second"));
        assert_eq!(r.current_turn_task(), "second");
        // No user message at all → empty string.
        let mut r2 = runner();
        r2.messages.push(Message::system("SYS"));
        assert_eq!(r2.current_turn_task(), "");
    }

    #[test]
    fn message_constructors_set_roles_and_ids() {
        let call = ToolCall {
            name: "read_sensor".to_string(),
            arguments: HashMap::new(),
        };

        let user = Message::user("hi");
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content, "hi");
        assert!(user.tool_call.is_none());

        let sys = Message::system("prompt");
        assert_eq!(sys.role, Role::System);

        let asst = Message::assistant_text("answer");
        assert_eq!(asst.role, Role::Assistant);
        assert!(asst.tool_call.is_none());

        let tc = Message::assistant_tool_call(call.clone());
        assert_eq!(tc.role, Role::Assistant);
        assert_eq!(tc.tool_call.as_ref().unwrap().name, "read_sensor");
        assert!(tc.tool_call_id.is_none());

        // The variant that carries a matching tool_call_id.
        let tc_id = Message::assistant_tool_call_id(call.clone(), "call_1");
        assert_eq!(tc_id.tool_call_id.as_deref(), Some("call_1"));

        // The follow-up tool-result message links back by id.
        let t = Message::tool("call_1", "result");
        assert_eq!(t.role, Role::Tool);
        assert_eq!(t.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(t.content, "result");
    }

    #[test]
    fn tool_result_success_and_error() {
        let ok = ToolResult::success("read_sensor", "25.5");
        assert!(ok.success);
        assert_eq!(ok.tool_name, "read_sensor");
        assert_eq!(ok.content, "25.5");

        let err = ToolResult::error("read_sensor", "sensor offline");
        assert!(!err.success);
        assert_eq!(err.content, "sensor offline");
    }
}

// ============================================================================
// Full ReAct loop integration tests (mock LLM backend)
// ============================================================================
//
// These drive `RealAgentRunner::run()` end-to-end through a canned
// [`LlmBackend`] that returns scripted tool-call / result / chatter
// responses. They exercise the entire ReAct loop — system-prompt seed,
// backend chat, tool execution, result parsing, and iteration budgeting —
// without touching the network.

#[cfg(test)]
mod run_react_loop_tests {
    use super::*;
    use crate::real_tools::SimulatorExecutor;

    /// A canned [`LlmBackend`] that hands back a scripted list of responses,
    /// one per `chat_with_messages` call, while counting calls.
    struct MockLlm {
        responses: Vec<String>,
        idx: usize,
        calls: usize,
    }

    impl MockLlm {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses,
                idx: 0,
                calls: 0,
            }
        }
    }

    impl LlmBackend for MockLlm {
        fn check_connection(&self) -> bool {
            true
        }

        fn chat_with_messages(
            &mut self,
            _messages: &[Message],
            _sampling: SamplingParams,
        ) -> std::result::Result<String, String> {
            self.calls += 1;
            let r = self
                .responses
                .get(self.idx)
                .cloned()
                .unwrap_or_else(|| r#"{"result":"done"}"#.to_string());
            self.idx += 1;
            Ok(r)
        }

        fn provider(&self) -> LlmProvider {
            LlmProvider::Ollama
        }

        fn model(&self) -> &str {
            "mock"
        }

        fn base_url(&self) -> &str {
            "mock://llm"
        }
    }

    /// Build a runner wired to a `MockLlm` and force the backend on, so
    /// `run()` routes every chat through the mock (no auto-probe, no
    /// simulated fallback).
    fn runner_with_mock(responses: Vec<String>) -> RealAgentRunner<SimulatorExecutor> {
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(MockLlm::new(responses));
        runner.force_enable_backend();
        runner
    }

    /// A backend that is never reachable and always errors if used.
    struct DeadLlm;

    impl LlmBackend for DeadLlm {
        fn check_connection(&self) -> bool {
            false
        }

        fn chat_with_messages(
            &mut self,
            _messages: &[Message],
            _sampling: SamplingParams,
        ) -> std::result::Result<String, String> {
            Err("connection refused".to_string())
        }

        fn provider(&self) -> LlmProvider {
            LlmProvider::Ollama
        }

        fn model(&self) -> &str {
            "dead"
        }

        fn base_url(&self) -> &str {
            "dead://llm"
        }
    }

    #[test]
    fn unreachable_backend_fails_instead_of_simulating() {
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(DeadLlm);
        runner.config_mut().probe_ollama_on_run = true;

        let err = runner
            .run("read the temperature")
            .expect_err("an unreachable configured backend must not simulate");
        assert!(err.contains("unreachable"), "unexpected error: {err}");
    }

    #[test]
    fn force_disable_backend_simulates_rather_than_erroring() {
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(DeadLlm);
        runner.config_mut().probe_ollama_on_run = true;
        runner.force_disable_backend();

        // An explicit disable is a decision, not a probe failure: the run must
        // still succeed against the scripted responder. Regression guard — the
        // CLI's "fall back to simulated responses" paths depend on this.
        let out = runner
            .run("read the temperature")
            .expect("explicitly disabled backend must simulate");
        assert!(!out.is_empty());
    }

    #[test]
    fn run_continuing_keeps_history_while_run_resets_it() {
        // `--repl` documents "maintains conversation context across
        // multiple turns"; `run` clears the history at the top of every
        // call, so the REPL needs `run_continuing`.
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.force_disable_backend();
        runner.config_mut().compression = crate::conversation::CompressionPolicy::disabled();

        runner.run("first task").unwrap();
        let after_one = runner.messages().len();
        assert!(after_one > 0);

        runner.run_continuing("second task").unwrap();
        let after_two = runner.messages().len();
        assert!(
            after_two > after_one,
            "run_continuing dropped the earlier turn ({after_one} -> {after_two})"
        );
        // Exactly one system message, however many turns we run.
        assert_eq!(
            runner
                .messages()
                .iter()
                .filter(|m| m.role == Role::System)
                .count(),
            1,
            "run_continuing accumulated duplicate system prompts"
        );

        // A plain `run` still starts clean.
        runner.run("third task").unwrap();
        assert_eq!(
            runner.messages().len(),
            after_one,
            "run() should have discarded the accumulated history"
        );
    }

    #[test]
    fn mid_run_backend_error_propagates() {
        // Probe off so the run gets past the start-of-run check and fails
        // inside `think()` — the mid-run path.
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(DeadLlm);
        runner.config_mut().probe_ollama_on_run = false;
        runner.force_enable_backend();

        let err = runner
            .run("read the temperature")
            .expect_err("a mid-run backend error must propagate");
        assert!(err.contains("LLM backend error"), "unexpected error: {err}");
    }

    /// A backend whose reachability the test flips between runs, so it
    /// can prove the runner re-probes rather than latching the first
    /// verdict for the life of the runner.
    struct FlakyLlm {
        reachable: std::sync::Arc<std::sync::atomic::AtomicBool>,
        probes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LlmBackend for FlakyLlm {
        fn check_connection(&self) -> bool {
            self.probes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.reachable.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn chat_with_messages(
            &mut self,
            _messages: &[Message],
            _sampling: SamplingParams,
        ) -> std::result::Result<String, String> {
            Ok(r#"{"result":"LIVE_BACKEND"}"#.to_string())
        }

        fn provider(&self) -> LlmProvider {
            LlmProvider::Ollama
        }

        fn model(&self) -> &str {
            "flaky"
        }

        fn base_url(&self) -> &str {
            "flaky://llm"
        }
    }

    /// Every chat request's message list, in order.
    type CapturedRequests = std::sync::Arc<std::sync::Mutex<Vec<Vec<Message>>>>;

    /// Records the message list of every chat request, so a test can
    /// assert what the model was actually shown.
    struct CapturingLlm {
        seen: CapturedRequests,
    }

    impl LlmBackend for CapturingLlm {
        fn check_connection(&self) -> bool {
            true
        }

        fn chat_with_messages(
            &mut self,
            messages: &[Message],
            _sampling: SamplingParams,
        ) -> std::result::Result<String, String> {
            self.seen.lock().unwrap().push(messages.to_vec());
            Ok(r#"{"result":"seen"}"#.to_string())
        }

        fn provider(&self) -> LlmProvider {
            LlmProvider::Ollama
        }

        fn model(&self) -> &str {
            "capture"
        }

        fn base_url(&self) -> &str {
            "capture://llm"
        }
    }

    fn capturing_runner() -> (RealAgentRunner<SimulatorExecutor>, CapturedRequests) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(CapturingLlm { seen: seen.clone() });
        runner.force_enable_backend();
        (runner, seen)
    }

    /// The note must be the *second* message: after the system prompt and
    /// ahead of the task. Index 0 is checked by content, not role, because
    /// the bug this guards against put the note *at* index 0.
    fn assert_note_builtin_second(msgs: &[Message], note: &str) -> bool {
        msgs.len() >= 3
            && msgs[0].role == Role::System
            && !msgs[0].content.contains(note)
            && msgs[1].content.contains(note)
    }

    #[test]
    fn a_seeded_system_context_reaches_the_model_on_a_plain_run() {
        let (mut runner, seen) = capturing_runner();
        runner.seed_system_context("PRIOR_RUN_NOTE");

        runner.run("do the thing").unwrap();

        let reqs = seen.lock().unwrap();
        let first = &reqs[0];
        assert!(
            assert_note_builtin_second(first, "PRIOR_RUN_NOTE"),
            "the seed did not land after the system prompt: {:?}",
            first.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_seeded_system_context_survives_continuing_runs_and_is_not_repeated() {
        let (mut runner, seen) = capturing_runner();
        runner.seed_system_context("PRIOR_RUN_NOTE");

        runner.run_continuing("first task").unwrap();
        runner.run_continuing("second task").unwrap();

        let reqs = seen.lock().unwrap();
        let first = &reqs[0];
        assert!(
            assert_note_builtin_second(first, "PRIOR_RUN_NOTE"),
            "the seed was dropped on the first continuing run: {:?}",
            first.iter().map(|m| &m.content).collect::<Vec<_>>()
        );

        // The note is a one-shot seed: after two turns it is in the history
        // exactly once, not re-prepended each turn.
        let last = reqs.last().unwrap();
        assert_eq!(
            last.iter()
                .filter(|m| m.content.contains("PRIOR_RUN_NOTE"))
                .count(),
            1,
            "the seed was re-inserted on the following turn"
        );
    }

    #[test]
    fn insert_after_system_prompt_on_an_empty_conversation_is_the_trap() {
        // Documents *why* `seed_system_context` exists rather than the
        // caller doing this: on an empty conversation the note lands at
        // index 0, and the next `run()` overwrites it with the prompt.
        let (mut runner, seen) = capturing_runner();
        runner.insert_after_system_prompt(Message::system("PRIOR_RUN_NOTE"));

        runner.run("do the thing").unwrap();

        let reqs = seen.lock().unwrap();
        let first = &reqs[0];
        assert!(
            !first.iter().any(|m| m.content.contains("PRIOR_RUN_NOTE")),
            "the trap no longer exists — `insert_after_system_prompt` \
             unexpectedly preserved the note: {:?}",
            first.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reset_backend_probe_makes_the_next_run_probe_again() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let reachable = std::sync::Arc::new(AtomicBool::new(true));
        let probes = std::sync::Arc::new(AtomicUsize::new(0));
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(FlakyLlm {
            reachable: reachable.clone(),
            probes: probes.clone(),
        });
        runner.config_mut().probe_ollama_on_run = true;
        runner.config_mut().compression = crate::conversation::CompressionPolicy::disabled();

        runner.run("first").unwrap();
        assert_eq!(probes.load(Ordering::SeqCst), 1, "the first run must probe");

        // The verdict is cached: a second run on the same runner reuses it.
        runner.run("second").unwrap();
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "the cached verdict should be reused"
        );

        // The REPL resets the verdict between turns, so a backend that
        // dropped mid-session is noticed instead of being used anyway.
        runner.reset_backend_probe();
        reachable.store(false, Ordering::SeqCst);
        let err = runner
            .run("third")
            .expect_err("a dead backend must not be reused silently");
        assert!(err.contains("unreachable"), "unexpected error: {err}");
        assert_eq!(
            probes.load(Ordering::SeqCst),
            2,
            "the reset run must have re-probed"
        );
    }

    #[test]
    fn reset_backend_probe_is_a_noop_without_a_wired_backend() {
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new()).without_backend();
        runner.config_mut().probe_ollama_on_run = true;

        // No backend to probe: the deliberate simulation path must survive.
        runner.reset_backend_probe();
        let out = runner
            .run("read the temperature")
            .expect("no-backend simulation must still run");
        assert!(!out.is_empty());
    }

    #[test]
    fn reset_backend_probe_will_not_downgrade_a_probe_less_runner() {
        // `force_disable_backend` clears `probe_ollama_on_run`, so a
        // reset must be a no-op — otherwise it would clear
        // `backend_enabled` and `run()` would never re-enable it,
        // silently turning an explicitly-live backend into the
        // simulated responder.
        let mut runner = runner_with_mock(vec![r#"{"result":"MOCK_FINAL_ANSWER_42"}"#.to_string()]);
        runner.config_mut().probe_ollama_on_run = false;

        runner.reset_backend_probe();
        let result = runner.run("what is the temperature?").unwrap();
        assert_eq!(
            result, "MOCK_FINAL_ANSWER_42",
            "the wired backend must still be used after a reset"
        );
    }

    #[test]
    fn run_completes_full_react_loop_with_mock_backend() {
        let mut runner = runner_with_mock(vec![
            r#"{"tool":"read_sensor","args":{"sensor":"temperature"}}"#.to_string(),
            r#"{"result":"MOCK_FINAL_ANSWER_42"}"#.to_string(),
        ]);

        let result = runner.run("what is the temperature?").unwrap();
        // The mock's terminal result is surfaced verbatim — proof the mock
        // backend (not the simulated fallback) produced it.
        assert_eq!(result, "MOCK_FINAL_ANSWER_42");
        assert_eq!(runner.tool_call_count, 1);
        assert!(runner.iteration >= 2);

        // The tool was actually executed by SimulatorExecutor.
        let msgs = runner.messages();
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::Tool && m.content.contains("Temperature")),
            "expected a read_sensor tool result in the conversation"
        );
        // The final result was recorded as an assistant message.
        assert!(msgs
            .iter()
            .any(|m| m.role == Role::Assistant && m.content.contains("MOCK_FINAL_ANSWER_42")));
    }

    #[test]
    fn run_recovers_from_tool_failure() {
        // First the mock asks for a sensor the executor can't resolve
        // (arg string without a recognised sensor keyword → tool error);
        // then it returns a terminal result. The ReAct loop must survive
        // the tool failure and keep going.
        let mut runner = runner_with_mock(vec![
            r#"{"tool":"read_sensor","args":{"sensor":"bogus"}}"#.to_string(),
            r#"{"result":"MOCK_RECOVERED"}"#.to_string(),
        ]);

        let result = runner.run("check the sensor").unwrap();
        assert_eq!(result, "MOCK_RECOVERED");

        // The failed tool surfaced as an Error tool message.
        let msgs = runner.messages();
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::Tool && m.content.contains("Error")),
            "expected the failing tool result to be surfaced as an error"
        );
    }

    #[test]
    fn run_exhausts_iteration_budget_on_chatter_and_reports_the_dead_end() {
        // If the mock never returns a tool call or a terminal result, the
        // loop treats each reply as chatter and iterates until the budget
        // runs out. "ok" is not an answer, so the run must report the dead
        // end: it used to return the chatter, which the CLI printed under its
        // `RESULT` banner as if the agent had produced it.
        let mut runner = runner_with_mock(vec!["ok".to_string(); 20]);

        let err = runner
            .run("keep going forever")
            .expect_err("mid-plan chatter is not a final answer");
        // Budget is exhausted after max_iterations (default 10).
        assert_eq!(runner.iteration, 10);
        assert_eq!(runner.tool_call_count, 0);
        assert!(err.contains("without a final answer"), "got {err:?}");
        // The chatter is quoted, so nothing the model said is lost.
        assert!(err.contains("\"ok\""), "got {err:?}");
    }

    #[test]
    fn a_terse_answer_below_the_prose_threshold_is_still_returned() {
        // `parse_result` accepts plain prose only past
        // `MIN_PROSE_RESULT_LEN`, so a short interjection can't end the loop
        // early. A model that answers `22.5` without the `{"result": …}`
        // envelope is being sloppy, not silent, so the leftover-text
        // fallback still hands the answer over rather than erroring.
        let mut runner = runner_with_mock(vec!["22.5".to_string(); 20]);
        runner.config_mut().max_iterations = 2;

        assert_eq!(runner.run("what is the temperature?").unwrap(), "22.5");
        // The rescue happens *after* the loop broke on its budget, so nothing
        // set a terminal state along the way. `Ok` from `run` and `Finished`
        // from `state` must still be the same fact, or the CLI's JSON
        // envelope would report this completed run as `Observing`.
        assert_eq!(runner.state(), AgentState::Finished);
    }

    #[test]
    fn a_second_repl_turn_is_answered_by_the_second_task_not_the_first() {
        // The scaffold keys its scripted response off the task. Turn two asks
        // about the LED; if the script still reads turn one's question it
        // replies with that script — the wrong answer to the question asked,
        // which is what `magent run --repl --mock` did.
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.force_disable_backend();

        // Turn one must use a route that *completes* here. The old script
        // paired this with "check the system health", whose last step is a
        // `ble_send` — with no peer reachable that step fails, and the runner
        // now reports the failure instead of the canned "Health check
        // complete - all systems operational". A temperature read has no
        // such dependency, so it still answers with its own script.
        let first = runner
            .run_continuing("check the temperature")
            .expect("turn one");
        assert!(first.contains("Temperature"), "got {first:?}");

        let second = runner.run_continuing("turn the led on").expect("turn two");
        assert!(
            second.contains("LED"),
            "turn two must answer its own question, got {second:?}"
        );
    }

    #[test]
    fn a_result_the_loop_accepted_is_not_re_judged_on_the_way_out() {
        // `think` accepts a well-formed `{"result": …}` envelope and sets
        // `Finished`; `get_result` then used to feed the *extracted* text back
        // through the classifier, which is a weaker test than the one that
        // accepted it. An answer that starts with an innocent word the
        // chatter heuristic claims — "first", "next", "ok", "now " — or that
        // is merely short would be rejected *after* being accepted, failing a
        // run that had answered. The loop's verdict is authoritative.
        for answer in ["first place is Alice", "ok, done", "next step is done"] {
            let mut runner = runner_with_mock(vec![format!(r#"{{"result":"{answer}"}}"#)]);
            assert_eq!(
                runner.run("who won?").unwrap(),
                answer,
                "an accepted envelope result must survive get_result"
            );
            assert_eq!(runner.state(), AgentState::Finished);
        }
    }

    #[test]
    fn a_turn_with_no_answer_of_its_own_does_not_reuse_the_previous_turns() {
        // `--repl` turns share one history, and `get_result` walks the whole
        // of it backwards. Turn one answers; turn two only ever emits tool
        // calls, so the tool budget runs out with no assistant text of its
        // own. The scan must not walk past the start of turn two and hand back
        // turn one's answer — printing a stale answer under the CLI's `RESULT`
        // banner is the same fabricated success the chatter split exists to
        // prevent, just across a turn boundary instead of within one.
        let call = r#"{"tool":"read_sensor","args":{"sensor":"temp"}}"#.to_string();
        // A long, non-chatter answer: nothing about it would be rejected by
        // the fallback's classifier, so if the scan reaches it the run reports
        // it as turn two's result and this test fails on the returned value.
        let first = "the temperature in the lab is 22.5 degrees";
        let mut responses = vec![format!(r#"{{"result":"{first}"}}"#)];
        responses.extend(vec![call; 20]);
        let mut runner = runner_with_mock(responses);
        runner.config_mut().max_tool_calls = 2;

        assert_eq!(runner.run_continuing("first").unwrap(), first);
        let err = runner
            .run_continuing("second")
            .expect_err("turn two never produced an answer of its own");
        // The specific diagnosis matters: "no assistant text was produced"
        // means the scan stayed inside turn two, while the chatter wording
        // would mean it walked into turn one and only tripped the classifier
        // by luck.
        assert!(
            err.contains("no assistant text was produced"),
            "turn two must report its own dead end, not a verdict about turn one: {err:?}"
        );
    }

    #[test]
    fn a_json_shaped_leftover_is_handed_over_rather_than_called_chatter() {
        // The third kind of text `get_result` can land on: neither a tool
        // call, nor a `{"result": …}` envelope, nor chatter — a bare JSON
        // object the classifier refuses for lacking a `result` key. It is
        // not a plan statement, so it is treated as the model's (sloppy)
        // answer and passed through, mirroring the terse-answer rescue. This
        // pins that choice: it is deliberate, not an oversight of the
        // chatter check.
        let mut runner = runner_with_mock(vec![r#"{"temperature":22.5}"#.to_string(); 20]);
        runner.config_mut().max_iterations = 2;

        assert_eq!(
            runner.run("what is the temperature?").unwrap(),
            r#"{"temperature":22.5}"#
        );
        assert_eq!(runner.state(), AgentState::Finished);
    }

    #[test]
    fn a_failed_run_leaves_the_runner_in_the_error_state() {
        // `RunReport.state` is how the CLI's JSON envelope reports the
        // outcome, so "returned `Err`" and "state is `Error`" must be the
        // same fact — including on the budget-exhaustion path, which is the
        // one that used to leave the state at `Thinking`.
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(DeadLlm);
        runner.config_mut().probe_ollama_on_run = true;
        runner.run("read the temperature").expect_err("unreachable");
        assert_eq!(runner.state(), AgentState::Error, "probe failure");

        let mut runner = runner_with_mock(vec!["ok".to_string(); 20]);
        runner.config_mut().max_iterations = 2;
        runner.run("chatter forever").expect_err("no answer");
        assert_eq!(runner.state(), AgentState::Error, "budget exhausted");

        // A run that did converge reports `Finished`, not `Error`.
        let mut runner = runner_with_mock(vec![r#"{"result":"done"}"#.to_string()]);
        runner.run("finish").unwrap();
        assert_eq!(runner.state(), AgentState::Finished);
    }

    #[test]
    fn run_tool_only_exhaustion_errors_instead_of_faking_success() {
        // Every reply is a tool call, so no assistant message ever carries
        // text. When the tool-call budget runs out, `get_result` used to fall
        // through to a canned "Task completed" — reporting success for a run
        // that produced no answer at all. It must report the dead end.
        let call = r#"{"tool":"read_sensor","args":{"sensor":"temp"}}"#.to_string();
        let mut runner = runner_with_mock(vec![call; 20]);
        runner.config_mut().max_tool_calls = 2;

        let err = runner
            .run("poll the sensor")
            .expect_err("a tool-only run that exhausts its budget produced no answer");
        assert_eq!(runner.tool_call_count, 2);
        assert!(err.contains("without a final answer"), "got {err:?}");
    }

    /// One assistant message asking for two independent tools. `parse_tool_call`
    /// returned a single `Option`, and its OpenAI-shape arm read `tool_calls[0]`
    /// and stopped — so the second call was never executed and never mentioned
    /// again. The model's next turn saw a conversation in which it had asked for
    /// only the first, and a reading it explicitly requested silently never
    /// happened.
    #[test]
    fn two_tool_calls_in_one_response_are_both_executed() {
        let two_calls = concat!(
            r#"{"tool_calls":["#,
            r#"{"id":"call_a","type":"function","function":{"name":"read_sensor","#,
            r#""arguments":"{\"sensor\":\"temperature\"}"}},"#,
            r#"{"id":"call_b","type":"function","function":{"name":"read_sensor","#,
            r#""arguments":"{\"sensor\":\"humidity\"}"}}]}"#
        );
        let mut runner = runner_with_mock(vec![
            two_calls.to_string(),
            r#"{"result":"done"}"#.to_string(),
        ]);

        runner.run("monitor the environment").unwrap();

        assert_eq!(
            runner.tool_call_count(),
            2,
            "the second tool call in the same response was dropped"
        );
        let bodied = |needle: &str| runner.messages().iter().any(|m| m.content.contains(needle));
        assert!(
            bodied("Temperature:"),
            "the first call's result is missing: {:?}",
            runner.messages()
        );
        assert!(
            bodied("Humidity:"),
            "the second call never ran: {:?}",
            runner.messages()
        );
    }

    /// Every tool-result message must name a call id that an assistant message
    /// announced. The parallel-call path allocates ids in a loop, so this is the
    /// invariant that would break first if the pairing drifted.
    #[test]
    fn every_tool_result_is_paired_with_its_assistant_call() {
        let two_calls = concat!(
            r#"{"tool_calls":["#,
            r#"{"function":{"name":"read_sensor","arguments":"{\"sensor\":\"temperature\"}"}},"#,
            r#"{"function":{"name":"read_sensor","arguments":"{\"sensor\":\"humidity\"}"}}]}"#
        );
        let mut runner = runner_with_mock(vec![
            two_calls.to_string(),
            r#"{"result":"done"}"#.to_string(),
        ]);
        runner.run("monitor").unwrap();

        let announced: Vec<&String> = runner
            .messages()
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .filter_map(|m| m.tool_call_id.as_ref())
            .collect();
        let answered: Vec<&String> = runner
            .messages()
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.as_ref())
            .collect();
        assert_eq!(announced.len(), 2, "two calls must be announced");
        assert_eq!(answered.len(), 2, "two results must be posted");
        for id in &answered {
            assert!(announced.contains(id), "result {id} answers no call");
        }
    }

    /// A REPL keeps the history but restarts the per-run budget counter, and
    /// the call id was allocated from that counter — so turn two's first call
    /// was `call_1` again while turn one's `call_1` was still in the history.
    /// The request then carried two different calls under one id, and which
    /// result belonged to which call was a matter of position, not of the
    /// `tool_call_id` the API is supposed to pair on.
    #[test]
    fn repl_turns_do_not_reissue_a_tool_call_id() {
        let call = r#"{"tool":"read_sensor","args":{"sensor":"temp"}}"#.to_string();
        let mut runner = runner_with_mock(vec![
            call.clone(),
            r#"{"result":"one"}"#.to_string(),
            call.clone(),
            r#"{"result":"two"}"#.to_string(),
        ]);

        runner.run("first turn").unwrap();
        runner.run_continuing("second turn").unwrap();

        let ids: Vec<&str> = runner
            .messages()
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids.len(), 2, "each turn made one call: {ids:?}");
        assert_ne!(
            ids[0], ids[1],
            "the second turn reused the first turn's call id: {ids:?}"
        );
    }

    /// A wall-clock budget of zero must stop the loop *before* the first LLM
    /// call, report the wall clock (not the untouched iteration counters) as
    /// the reason, and leave the backend unused.
    #[test]
    fn a_zero_wall_clock_budget_stops_before_the_first_call() {
        use std::sync::{Arc, Mutex};

        let mut runner = runner_with_mock(vec![r#"{"result":"never reached"}"#.to_string()]);
        runner.config_mut().max_run_duration = Some(std::time::Duration::ZERO);

        let captured: Arc<Mutex<Vec<TraceEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(SharedTraceSink::new());
        let sink_captured = captured.clone();
        shared.install(Box::new(FnSink::new(move |event| {
            sink_captured.lock().unwrap().push(event);
        })));
        runner.config_mut().trace_sink = Some(shared);

        let err = runner
            .run("do the thing")
            .expect_err("a zero wall-clock budget must expire before the first step");
        assert!(
            err.contains("wall-clock budget of 0s exhausted"),
            "the error must name the wall clock, got: {err}"
        );
        assert!(
            err.contains("iterations: 0/10"),
            "no iteration should have run, got: {err}"
        );

        let events = captured.lock().unwrap();
        assert!(
            events.iter().any(|event| matches!(
                event,
                TraceEvent::BudgetExhausted {
                    kind: "wall_clock_ms",
                    limit: 0
                }
            )),
            "the budget-exhausted trace was not emitted: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TraceEvent::LlmResponse { .. })),
            "the LLM must not have been called: {events:?}"
        );
    }

    /// A slow reply must be *allowed to finish*, then stop the loop at the
    /// next head check — the budget is not a request timeout (each client has
    /// its own), it bounds the time between steps.
    #[test]
    fn a_wall_clock_budget_stops_between_iterations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct SlowLlm {
            calls: Arc<AtomicUsize>,
            delay: std::time::Duration,
        }

        impl LlmBackend for SlowLlm {
            fn check_connection(&self) -> bool {
                true
            }

            fn chat_with_messages(
                &mut self,
                _messages: &[Message],
                _sampling: SamplingParams,
            ) -> std::result::Result<String, String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(self.delay);
                // Not a result and not a tool call: the loop goes back to
                // Thinking, which is where the wall-clock check runs.
                Ok("Let me check the sensor.".to_string())
            }

            fn provider(&self) -> LlmProvider {
                LlmProvider::Ollama
            }

            fn model(&self) -> &str {
                "slow"
            }

            fn base_url(&self) -> &str {
                "slow://llm"
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
        runner.set_backend(SlowLlm {
            calls: calls.clone(),
            delay: std::time::Duration::from_millis(1_200),
        });
        runner.force_enable_backend();
        runner.config_mut().max_run_duration = Some(std::time::Duration::from_secs(1));

        let err = runner
            .run("do the thing")
            .expect_err("the budget must expire after the first slow reply");
        assert!(
            err.contains("wall-clock budget of 1s exhausted"),
            "the error must name the wall clock, got: {err}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the in-flight reply must finish; the budget is checked between steps"
        );
    }

    /// A budget that is not yet exhausted must not change the normal path.
    #[test]
    fn a_generous_wall_clock_budget_leaves_a_run_alone() {
        let mut runner = runner_with_mock(vec![r#"{"result":"done"}"#.to_string()]);
        runner.config_mut().max_run_duration = Some(std::time::Duration::from_secs(60));

        let answer = runner.run("do the thing").unwrap();
        assert_eq!(answer, "done");
        assert!(runner.wall_clock_note().is_empty());
    }
}

#[cfg(test)]
mod simulated_planner_tests {
    use super::*;
    use crate::real_tools::SimulatorExecutor;

    fn runner() -> RealAgentRunner<SimulatorExecutor> {
        RealAgentRunner::new(SimulatorExecutor::new())
    }

    fn plan(task: &str) -> String {
        let mut r = runner();
        r.messages.push(Message::system("SYS"));
        r.messages.push(Message::user(task));
        r.generate_simulated_response()
    }

    fn planned_tool(task: &str) -> Option<String> {
        let out = plan(task);
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        v.get("tool").and_then(|t| t.as_str()).map(str::to_string)
    }

    /// The hospital route picks the specialty from the task, and an
    /// unspecialised request asks for the generic ER rather than guessing one.
    #[test]
    fn the_hospital_route_asks_for_the_specialty_the_task_names() {
        let args = |task: &str| {
            let v: serde_json::Value = serde_json::from_str(&plan(task)).unwrap();
            v.get("args")
                .and_then(|a| a.get("specialty"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        };
        assert_eq!(args("find the nearest hospital").as_deref(), Some("er"));
        assert_eq!(
            args("find a hospital with a cardiology department").as_deref(),
            Some("cardiology")
        );
        assert_eq!(
            args("nearest hospital for diabetes care").as_deref(),
            Some("endocrinology")
        );
    }

    /// The offline planner (`--mock`) had no route for any of the tools the
    /// system prompt advertises, so every health task fell through to the
    /// default — a `read_sensor temperature` call it was never asked for.
    #[test]
    fn health_tasks_route_to_the_tools_the_prompt_advertises() {
        for (task, expected) in [
            ("how did I sleep last night", "get_sleep_quality"),
            ("what is my heart rate", "read_heart_rate"),
            ("am I stressed", "check_stress_level"),
            ("check my blood sugar", "read_glucose"),
            ("read my ecg", "read_ecg"),
            ("start a running session", "start_exercise"),
            ("start cycling", "start_exercise"),
            ("begin a yoga session", "start_exercise"),
            ("end my exercise session", "end_exercise"),
            ("what is my exercise goal", "get_exercise_goal"),
            ("adjust my exercise goal", "adjust_exercise_goal"),
            ("send a health alert", "send_health_alert"),
            ("guide me through a meditation", "send_meditation"),
            ("set do not disturb", "set_dnd"),
            ("notify my emergency contact", "notify_emergency_contact"),
            ("find the nearest hospital", "find_nearest_hospital"),
        ] {
            assert_eq!(
                planned_tool(task).as_deref(),
                Some(expected),
                "task {task:?} did not route to {expected}"
            );
        }
    }

    /// "end my run" and "adjust my exercise goal" both contain "run"/"exercise";
    /// the start route must not win over them.
    #[test]
    fn ending_and_adjusting_are_not_shadowed_by_starting() {
        assert_eq!(planned_tool("end my run").as_deref(), Some("end_exercise"));
        assert_eq!(
            planned_tool("stop the workout").as_deref(),
            Some("end_exercise")
        );
        assert_eq!(
            planned_tool("change my exercise intensity").as_deref(),
            Some("adjust_exercise_goal")
        );
        // And "health alert" must beat the generic `["health", ...]` branch.
        assert_eq!(
            planned_tool("send a health alert").as_deref(),
            Some("send_health_alert")
        );
    }

    /// `battery` and `memory` are advertised by the prompt and resolved by
    /// the executor, so the offline planner has to reach them — and the
    /// `memory` route must not swallow the flash phrasings it shares a word
    /// with.
    #[test]
    fn device_status_sensors_route_without_hijacking_flash() {
        let sensor = |task: &str| {
            let v: serde_json::Value = serde_json::from_str(&plan(task)).unwrap();
            assert_eq!(v["tool"].as_str(), Some("read_sensor"), "task {task:?}");
            v["args"]["sensor"].as_str().map(str::to_string)
        };
        assert_eq!(
            sensor("what is the battery level").as_deref(),
            Some("battery")
        );
        assert_eq!(sensor("电池还剩多少").as_deref(), Some("battery"));
        assert_eq!(
            sensor("how much free heap is left").as_deref(),
            Some("memory")
        );
        assert_eq!(sensor("free memory").as_deref(), Some("memory"));
        // "read from flash memory" mentions `memory` but is a flash read.
        assert_eq!(
            planned_tool("read from flash memory").as_deref(),
            Some("flash_read")
        );
    }

    /// The activity named in the task is the one the planner starts. A task
    /// naming no type still needs one to call the tool at all.
    #[test]
    fn start_exercise_uses_the_named_activity() {
        for (task, kind) in [
            ("start a running session", "running"),
            ("start cycling", "cycling"),
            ("go for a walk", "walking"),
            ("start swimming", "swimming"),
            ("start yoga", "yoga"),
            ("start a hiit workout", "hiit"),
        ] {
            let out = plan(task);
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(
                v["args"]["type"].as_str(),
                Some(kind),
                "task {task:?} planned {out}"
            );
        }
    }

    /// An unmatched task must not execute a tool it never asked for. The old
    /// default read the temperature and then reported success.
    #[test]
    fn an_unmatched_task_does_not_run_an_unrelated_tool() {
        let out = plan("say hi");
        assert!(
            !out.contains("read_sensor"),
            "the default planned a sensor read: {out}"
        );
        assert!(
            !out.contains("temperature"),
            "the default planned a temperature read: {out}"
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(v.get("result").is_some(), "must terminate with a result");
    }

    /// A tool that failed must not be reported as done. The failure text carries
    /// quotes (`field: "sleep"`), so the result has to be escaped or it is not
    /// JSON at all — and an unparseable result is a failure reported as silence.
    #[test]
    fn a_failed_tool_is_reported_as_a_failure_not_as_done() {
        let mut r = runner();
        r.messages.push(Message::system("SYS"));
        r.messages.push(Message::user("how did I sleep last night"));
        r.messages.push(Message::tool(
            "call_1",
            r#"Error: ConfigurationError { field: "sleep", reason: MissingField }"#,
        ));
        r.tool_call_count = 1;

        let out = r.generate_simulated_response();
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("invalid JSON {out:?}: {e}"));
        let text = v["result"].as_str().expect("a result string");
        assert!(text.contains("did not complete"), "got {text:?}");
        assert!(v.get("tool").is_none(), "must not plan another tool");
    }
}

/// Coverage for the small, pure, and previously-untested surfaces of this
/// module: the `log`-backed sink, the diagnostic `Display`/`Debug` impls, the
/// `LlmBackend` trait forwarders, the legacy string-array `OllamaClient::chat`
/// translation, and `OllamaClient::generate`. None of these are reached by the
/// ReAct-loop tests, yet all of them ship to hosts (`magent doctor`, `--repl`)
/// or firmware.
#[cfg(test)]
mod surface_tests {
    use super::*;

    // ---- LogSink: every arm of the `log` facade mapping ----

    #[test]
    fn log_sink_handles_every_event_variant() {
        // The sink is a pure `match` over `TraceEvent`; driving one of each
        // variant exercises every arm (the actual `log::*` calls are no-ops
        // with no logger installed, which is exactly the embedded contract).
        let mut sink = LogSink;
        sink.event(TraceEvent::RunStart {
            task: "do the thing".to_string(),
        });
        sink.event(TraceEvent::BackendReady {
            provider: "ollama".to_string(),
            using_real_llm: true,
        });
        sink.event(TraceEvent::BackendReady {
            provider: "sim".to_string(),
            using_real_llm: false,
        });
        sink.event(TraceEvent::BudgetExhausted {
            kind: "iterations",
            limit: 10,
        });
        sink.event(TraceEvent::ThinkingStart {
            iteration: 1,
            tool_calls: 0,
        });
        sink.event(TraceEvent::CompressionApplied {
            kept: 8,
            dropped: 2,
            tool_results_truncated: 1,
            bytes_saved: 512,
        });
        sink.event(TraceEvent::LlmResponse {
            body: "hello".to_string(),
        });
        sink.event(TraceEvent::ToolCallStart {
            name: "read_sensor".to_string(),
            arguments: "{\"sensor\":\"temperature\"}".to_string(),
        });
        sink.event(TraceEvent::ToolCallEnd {
            name: "read_sensor".to_string(),
            result: "25.5".to_string(),
            success: true,
        });
        sink.event(TraceEvent::ToolCallEnd {
            name: "read_sensor".to_string(),
            result: "sensor offline".to_string(),
            success: false,
        });
        sink.event(TraceEvent::FinalResult {
            body: "done".to_string(),
        });
        sink.event(TraceEvent::ObservingNoAction);
        sink.event(TraceEvent::Observing);
    }

    // ---- Diagnostics: Display / Debug ----

    #[test]
    fn agent_state_display_covers_every_state() {
        assert_eq!(AgentState::Idle.to_string(), "Idle");
        assert_eq!(AgentState::Thinking.to_string(), "Thinking");
        assert_eq!(AgentState::Executing.to_string(), "Executing");
        assert_eq!(AgentState::Observing.to_string(), "Observing");
        assert_eq!(AgentState::Finished.to_string(), "Finished");
        assert_eq!(AgentState::Error.to_string(), "Error");
    }

    #[test]
    fn llm_provider_display_covers_every_provider() {
        assert_eq!(LlmProvider::Ollama.to_string(), "ollama");
        assert_eq!(LlmProvider::DeepSeek.to_string(), "deepseek");
        assert_eq!(LlmProvider::Custom.to_string(), "custom");
    }

    #[test]
    fn ollama_client_debug_names_url_and_model() {
        let c = OllamaClient::new("http://gpu:11434/", "qwen2.5:3b");
        let dbg = format!("{c:?}");
        assert!(dbg.contains("http://gpu:11434"), "got {dbg}");
        assert!(dbg.contains("qwen2.5:3b"), "got {dbg}");
        // The HTTP client is deliberately elided (`finish_non_exhaustive`).
        assert!(dbg.contains(".."), "expected non-exhaustive marker: {dbg}");
    }

    #[test]
    fn shared_trace_sink_debug_reports_installed_count() {
        let shared = SharedTraceSink::new();
        assert_eq!(format!("{shared:?}"), "SharedTraceSink { installed: 0 }");
        shared.install(Box::new(NullSink));
        assert_eq!(format!("{shared:?}"), "SharedTraceSink { installed: 1 }");
    }

    // ---- RunnerConfig::tool_prompt / set_tool_descriptions ----

    #[test]
    fn tool_prompt_renders_bullets_and_skips_blank_descriptions() {
        let mut cfg = RunnerConfig::default();
        cfg.set_tool_descriptions(vec![
            ("read_sensor".to_string(), "Read a sensor".to_string()),
            ("blank".to_string(), "   ".to_string()),
            ("write_gpio".to_string(), "Set a pin".to_string()),
        ]);
        // The blank-description entry is skipped, and the survivors are joined
        // with newlines (which is the branch that `is_empty()`/`push('\n')`
        // guards).
        assert_eq!(
            cfg.tool_prompt(),
            "- read_sensor: Read a sensor\n- write_gpio: Set a pin"
        );
    }

    #[test]
    fn tool_prompt_is_empty_for_no_tools() {
        let cfg = RunnerConfig::default();
        assert_eq!(cfg.tool_prompt(), "");
    }

    // ---- OllamaClient: trait forwarders, chat(), generate() ----

    #[test]
    fn ollama_trait_forwarders_match_the_inherent_methods() {
        // `LlmBackend` is what `RealAgentRunner::set_backend` stores, so its
        // forwarders must report exactly the inherent values.
        let c = OllamaClient::new("http://host:11434/", "llama3.2");
        assert_eq!(LlmBackend::provider(&c), LlmProvider::Ollama);
        assert_eq!(LlmBackend::model(&c), "llama3.2");
        assert_eq!(LlmBackend::base_url(&c), "http://host:11434");
    }

    #[test]
    fn ollama_set_model_replaces_the_model_name() {
        let mut c = OllamaClient::new("http://host:11434", "llama3.2");
        c.set_model("qwen2.5:3b");
        assert_eq!(c.model(), "qwen2.5:3b");
    }

    #[test]
    fn ollama_trait_check_connection_delegates_to_the_probe() {
        // The trait's `check_connection` forwarder must return the same
        // answer the inherent probe does — the runner calls it through
        // `&mut dyn LlmBackend`.
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"models\":[]}",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        assert!(LlmBackend::check_connection(&c));
    }

    #[test]
    fn ollama_trait_chat_with_messages_delegates_to_the_inherent_method() {
        // Drive a completion through the trait object the runner holds and
        // assert the parsed content round-trips.
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"message\":{\"role\":\"assistant\",\"content\":\"hi there\"}}",
        )]);
        let mut c = OllamaClient::new(&mock.base_url, "llama3.2");
        let backend: &mut dyn LlmBackend = &mut c;
        let reply = backend
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect("mock answers 200");
        assert_eq!(reply, "hi there");
    }

    #[test]
    fn ollama_chat_translates_every_legacy_prefix() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}",
        )]);
        let mut c = OllamaClient::new(&mock.base_url, "llama3.2");
        let reply = c
            .chat(
                &[
                    "[User] what's the temperature".to_string(),
                    "[Assistant] checking".to_string(),
                    "[Tool] read_sensor: 25.5".to_string(),
                    "[Tool] baretoolresult".to_string(),
                    "[System] Task: log vials".to_string(),
                    "[System] plain system text".to_string(),
                    "unprefixed line".to_string(),
                ],
                "SYS",
            )
            .expect("the mock answers 200");
        assert_eq!(reply, "ok");

        // The generated request body must carry the translated roles, proving
        // each `strip_prefix` arm ran.
        let seen = mock.requests();
        assert_eq!(seen.len(), 1);
        let body = &seen[0].body;
        assert!(body.contains("\"role\":\"system\""), "{body}");
        assert!(body.contains("\"role\":\"user\""), "{body}");
        assert!(body.contains("\"role\":\"assistant\""), "{body}");
        assert!(body.contains("what's the temperature"), "{body}");
        assert!(body.contains("log vials"), "{body}");
        assert!(body.contains("unprefixed line"), "{body}");
    }

    #[test]
    fn ollama_generate_returns_the_response_field() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"response\":\"generated text\"}",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        assert_eq!(c.generate("hi").expect("200 with a response"), "generated text");
    }

    #[test]
    fn ollama_generate_reports_a_non_success_status() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            500,
            "boom",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        let err = c.generate("hi").expect_err("500 is an error");
        assert!(err.contains("HTTP error"), "got {err}");
    }

    #[test]
    fn ollama_generate_reports_a_missing_response_field() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"other\":1}",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        let err = c.generate("hi").expect_err("no `response` field");
        assert!(err.contains("No content in response"), "got {err}");
    }

    #[test]
    fn ollama_chat_surfaces_a_non_success_status() {
        // The status branch of `chat_with_messages` is separate from the retry
        // machinery: a permanent 400 must come back as "HTTP error".
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            400,
            "bad",
        )]);
        let mut c = OllamaClient::new(&mock.base_url, "llama3.2");
        let err = c
            .chat_with_messages(&[Message::user("hi")], SamplingParams::default())
            .expect_err("400 is an error");
        assert!(err.contains("HTTP error"), "got {err}");
    }

    #[test]
    fn ollama_get_models_parses_the_names_array() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"models\":[{\"name\":\"llama3.2\"},{\"name\":\"qwen2.5\"}]}",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        assert_eq!(c.get_models(), vec!["llama3.2", "qwen2.5"]);
    }

    #[test]
    fn ollama_get_models_is_empty_on_a_missing_array() {
        let mock = super::http_mock::Mock::start(vec![super::http_mock::Reply::new(
            200,
            "{\"unexpected\":true}",
        )]);
        let c = OllamaClient::new(&mock.base_url, "llama3.2");
        assert!(c.get_models().is_empty());
    }

    // ---- advertised_tools / advertised_sensors skip branches ----

    #[test]
    fn advertised_tools_parses_each_prompt_line() {
        // The parser runs over the real prompt; a tool line is the only thing
        // with a `- name(` shape, so the count is the prompt's contract.
        let tools = advertised_tools();
        assert!(tools.iter().any(|t| t.name == "read_sensor"));
        assert!(tools.iter().any(|t| t.name == "write_gpio"));
        assert!(tools.iter().any(|t| t.name == "web_search"));
        // Every parsed entry carries its signature and a description.
        for t in &tools {
            assert!(t.signature.starts_with(t.name), "bad sig for {}", t.name);
            assert!(!t.description.is_empty(), "no description for {}", t.name);
        }
    }

    #[test]
    fn advertised_sensors_parses_the_read_sensor_line() {
        let sensors = advertised_sensors();
        assert!(sensors.contains(&"temperature"));
        assert!(sensors.contains(&"heart_rate"));
        assert!(sensors.contains(&"battery"));
    }

    // ---- JSON escapers: the control-character and unicode arms ----

    #[test]
    fn escape_json_covers_every_special_case() {
        // `\r`/`\t` and the generic `< 0x20` -> `\uXXXX` arm were unexercised;
        // a control byte reaching the body unescaped produces invalid JSON.
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("a\\b"), "a\\\\b");
        assert_eq!(escape_json("a\nb"), "a\\nb");
        assert_eq!(escape_json("a\rb"), "a\\rb");
        assert_eq!(escape_json("a\tb"), "a\\tb");
        assert_eq!(escape_json("a\u{0007}b"), "a\\u0007b");
        // Multi-byte characters pass through unchanged.
        assert_eq!(escape_json("温度"), "温度");
    }

    #[test]
    fn write_json_string_matches_escape_json_for_the_same_input() {
        let mut buf = Vec::new();
        write_json_string(&mut buf, "x\r\t\u{0001}\"\\\n温度").expect("writes");
        let as_str = String::from_utf8(buf).expect("utf8");
        assert_eq!(as_str, format!("\"{}\"", escape_json("x\r\t\u{0001}\"\\\n温度")));
    }
}
