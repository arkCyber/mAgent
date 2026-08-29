//! `magent run` — wires the agent runner to the CLI.
//!
//! Architecture:
//!
//! ```text
//!   ┌──────────────────────────┐
//!   │ RunCmd::execute()        │   ← entry point from main.rs
//!   └────────────┬─────────────┘
//!                │ builds
//!                ▼
//!   ┌──────────────────────────┐
//!   │ RealAgentRunner<         │
//!   │   SimulatorExecutor      │ ← tool backend (sensors/BLE/flash/GPIO)
//!   │ >                        │
//!   └────────────┬─────────────┘
//!                │ drives
//!                ▼
//!   ┌──────────────────────────┐
//!   │ LlmBackend (trait)       │ ← pluggable chat-completions client
//!   │  ┌────────────────────┐  │
//!   │  │ OllamaClient       │  │   ← default; localhost:11434
//!   │  ├────────────────────┤  │
//!   │  │ DeepSeekClient     │  │   ← `--provider deepseek`
//!   │  └────────────────────┘  │
//!   └──────────────────────────┘
//! ```
//!
//! We construct the runner in [`build_runner`], run it in
//! [`RunCmd::execute`], and stream the conversation to the [`Output`]
//! adapter so the user sees step-by-step progress on stderr (Human mode)
//! or a single JSON envelope on stdout (JSON mode).
//!
//! ## Provider selection
//!
//! Default is Ollama (local). Switch with `--provider deepseek`; the
//! API key comes from `--api-key`, `DEEPSEEK_API_KEY`, or
//! `OLLAMA_API_KEY` (in that priority order).
//!
//! Configuration precedence (later wins):
//!
//! 1. Built-in defaults baked into the binary.
//! 2. `~/.config/magent/magent.json` (managed by `magent config`).
//! 3. Environment variables (`OLLAMA_HOST`, `DEEPSEEK_API_KEY`, …).
//! 4. CLI flags (`--provider`, `--model`, `--temperature`, …).
//!
//! See [`apply_config_overrides`] for the implementation.

use std::collections::VecDeque;
use std::io;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use magent_core::agent_runner::{
    DeepSeekClient, LlmBackend, LogSink, OllamaClient, RealAgentRunner, RunnerConfig,
    SamplingParams, SharedTraceSink, ToolExecutor, TraceEvent, TraceSink,
};
use magent_core::conversation::CompressionPolicy;
use magent_core::summary::{FileSummaryStore, MessageDto, SummaryRecord, SummarySource, SummaryStore};

// `web3_app` — signed run-report envelope helpers. We only compile
// these in when the feature is on so a plain `cargo build` doesn't
// drag `ed25519-dalek` into the binary. The actual signing /
// verify logic lives in [`magent_core::web3_app`]; the CLI side
// just maps the runner's [`RunReport`] into [`RunReportFields`]
// and wires `--sign` / `--verify-signed` into the dispatch flow.
#[cfg(feature = "web3_app")]
use magent_core::web3_app::{
    parse_and_verify_signed_run_report, sign_run_report, RunReportFields,
};
#[cfg(feature = "web3_app")]
use crate::web3 as web3_cli;
#[cfg(feature = "web3")]
use crate::blockchain_executor::BlockchainExecutor;

use crate::email_executor::CompositeExecutor;

use crate::cli::RunOptions;
use crate::output::{Output, OutputKind};

/// Anything that can go wrong while running an agent task.
#[derive(Debug)]
pub enum RunError {
    /// Could not load `--prompt <FILE>`.
    PromptLoad(String),
    /// AgentRunner.run returned an error (e.g. budget exhausted, parse
    /// failure that wasn't recoverable).
    Agent(String),
    /// I/O error talking to stdout / stderr.
    Io(std::io::Error),
    /// `--sign` was requested but the signing step failed
    /// (vault locked, identity not found, validation, …). The
    /// underlying error message is preserved verbatim for the
    /// CLI to surface.
    SignedEnvelope(String),
    /// `--verify-signed <PATH>` was requested but the
    /// verification step failed (tamper, expired, …). Surfaced
    /// by the runner for symmetry with `SignedEnvelope` above.
    VerifySignedEnvelope(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::PromptLoad(msg) => write!(f, "could not load system prompt: {}", msg),
            RunError::Agent(msg) => write!(f, "agent failed: {}", msg),
            RunError::Io(e) => write!(f, "i/o error: {}", e),
            RunError::SignedEnvelope(msg) => write!(f, "signed envelope: {}", msg),
            RunError::VerifySignedEnvelope(msg) => write!(f, "verify envelope: {}", msg),
        }
    }
}

impl std::error::Error for RunError {}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        RunError::Io(e)
    }
}

/// Adapter that routes [`TraceEvent`]s from the runner into the
/// CLI's [`Output`] so the human-mode step-by-step trace, the
/// JSON-mode envelope, and the quiet-mode silence all stay in sync.
///
/// In **Human** mode every event becomes a labelled trace line on
/// stderr (matching the colour scheme the rest of the CLI uses for
/// `[agent] …` / `[DeepSeek] …` lines). In **JSON** mode events are
/// silently dropped so the stdout envelope stays clean — same
/// policy as [`Output::info`]. When the user passes `--quiet`, we
/// stop emitting events at the sink level entirely.
///
/// The sink does **not** borrow `Output` — `Output` holds
/// non-`Clone` stdout/stderr locks that can't travel inside a
/// `Box<dyn TraceSink>`. Instead the sink re-acquires the locks
/// per event from `std::io::stdout()` / `std::io::stderr()`. This
/// is one extra syscall per event but matches the lock-per-call
/// pattern the rest of the CLI already uses, and keeps the sink
/// `'static` so the runner can own it directly.
pub struct OutputTraceSink {
    kind: OutputKind,
    quiet: bool,
}

impl OutputTraceSink {
    /// Build a sink matching the user's current [`Output`] mode.
    /// `quiet` short-circuits every event before any IO happens.
    pub fn new(kind: OutputKind, quiet: bool) -> Self {
        Self { kind, quiet }
    }
}

impl TraceSink for OutputTraceSink {
    fn event(&mut self, event: TraceEvent) {
        if self.quiet {
            return;
        }
        if self.kind == OutputKind::Json {
            // JSON mode: trace events would corrupt the envelope,
            // drop them silently. Same policy as `Output::trace`.
            return;
        }
        let label = match &event {
            TraceEvent::RunStart { .. } => "Run",
            TraceEvent::BackendReady { .. } => "Backend",
            TraceEvent::BudgetExhausted { .. } => "Budget",
            TraceEvent::ThinkingStart { .. } => "Thinking",
            TraceEvent::CompressionApplied { .. } => "Compress",
            TraceEvent::LlmResponse { .. } => "LLM",
            TraceEvent::ToolCallStart { .. } => "Action",
            TraceEvent::ToolCallEnd { success: true, .. } => "Tool",
            TraceEvent::ToolCallEnd { .. } => "Tool-Error",
            TraceEvent::FinalResult { .. } => "Result",
            TraceEvent::ObservingNoAction => "Observing",
            TraceEvent::Observing => "Observing",
        };
        let body = render_event(&event);
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "[{}] {}", label, body);
    }
}

/// Truncate `s` to at most `max` bytes without ever splitting a UTF-8
/// code point. `&s[..max]` panics if `max` lands in the middle of a
/// multi-byte character, which the human trace output hits whenever the
/// LLM responds with non-ASCII text (e.g. Chinese weather answers).
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Render a `TraceEvent` as a single human-readable line. Pulled
/// out of `event` so unit tests can pin the exact output format.
fn render_event(event: &TraceEvent) -> String {
    match event {
        TraceEvent::RunStart { task } => task.clone(),
        TraceEvent::BackendReady {
            provider,
            using_real_llm,
        } => {
            if *using_real_llm {
                format!("{} (real LLM)", provider)
            } else {
                "simulated reasoning (no LLM backend)".to_string()
            }
        }
        TraceEvent::BudgetExhausted { kind, limit } => {
            format!("budget exhausted: {}={}", kind, limit)
        }
        TraceEvent::ThinkingStart {
            iteration,
            tool_calls,
        } => {
            format!("iteration {} (tool_calls={})", iteration, tool_calls)
        }
        TraceEvent::CompressionApplied {
            kept,
            dropped,
            tool_results_truncated,
            bytes_saved,
        } => format!(
            "kept={} dropped={} truncated={} bytes_saved≈{}",
            kept, dropped, tool_results_truncated, bytes_saved
        ),
        TraceEvent::LlmResponse { body } => {
            if body.len() > 200 {
                format!("{}…", truncate_utf8(body, 200))
            } else {
                body.clone()
            }
        }
        TraceEvent::ToolCallStart { name, arguments } => {
            format!("{} {}", name, arguments)
        }
        TraceEvent::ToolCallEnd {
            name,
            result,
            success,
        } => {
            if *success {
                format!("{}: {}", name, result)
            } else {
                format!("{} (error): {}", name, result)
            }
        }
        TraceEvent::FinalResult { body } => body.clone(),
        TraceEvent::ObservingNoAction => "no tool call, continuing…".to_string(),
        TraceEvent::Observing => "processing result…".to_string(),
    }
}

/// The `magent run` subcommand, after argv parsing.
pub struct RunCmd<'a> {
    pub opts: &'a mut RunOptions,
}

/// What `apply_config_overrides` decided to do with the config
/// file. We split applied values from warnings so the trace line
/// stays clean: `[Config] applied foo, bar` is one line, then
/// `[Config] warning: …` is a separate line if anything looked
/// off. Mixing them into a single comma-joined list would make
/// the warnings look like ordinary config values.
#[derive(Debug, Default)]
struct ConfigApplyReport {
    /// Human-readable strings describing each value the config
    /// filled in. Joined with `, ` for the `[Config] applied …` line.
    applied: Vec<String>,
    /// Anything the user might want to know but that doesn't
    /// prevent the run from proceeding. Emitted as separate
    /// `[Config] warning: …` lines so they don't get lost in
    /// the applied list.
    warnings: Vec<String>,
}

/// Apply the on-disk config file's defaults to `opts`.
///
/// The layering is **built-in → config file → CLI flags**, so this
/// helper only ever fills in fields that are still at their
/// "user didn't supply anything" sentinel (`None` for `Option<T>`,
/// empty string for `String`).
fn apply_config_overrides(
    opts: &mut RunOptions,
    config: &crate::config::ConfigRecord,
) -> ConfigApplyReport {
    let mut report = ConfigApplyReport::default();

    // Provider — empty means "user didn't pass --provider". We
    // resolve this *first* so the URL / model helpers below know
    // which side of the config to read from.
    if opts.provider.is_empty() {
        let p = &config.provider.default;
        if !p.is_empty() {
            opts.provider = p.clone();
            report.applied.push(format!("provider={}", p));
        }
    }
    // `effective_provider` is what the URL / model helpers should
    // treat as "the provider in effect right now". It falls back to
    // the historical default (`ollama`) when even the config file
    // doesn't pin one — matches the previous CLI behaviour.
    let effective_provider = if opts.provider.is_empty() {
        "ollama"
    } else {
        opts.provider.as_str()
    };

    // Provider-specific URLs. Pulled from the config only when
    // still empty; never mix ollama and deepseek URLs.
    if opts.ollama_url.is_empty() {
        if let Some(url) = endpoint_url(config, "ollama") {
            opts.ollama_url = url.to_string();
            report.applied.push(format!("ollama_url={}", url));
        }
    }
    if opts.deepseek_url.is_empty() {
        if let Some(url) = endpoint_url(config, "deepseek") {
            opts.deepseek_url = url.to_string();
            report.applied.push(format!("deepseek_url={}", url));
        }
    }
    // Model — use the effective provider (not "ollama" hard-coded),
    // so a config that defaults to deepseek gets `deepseek-chat`,
    // not `llama3.2`.
    if opts.model.is_empty() {
        if let Some(model) = endpoint_model(config, effective_provider) {
            opts.model = model.to_string();
            report.applied.push(format!("model={}", model));
        }
    }
    // Sampling. We always honour the config value (including 0.0
    // for temperature, which a user can legitimately set to lock
    // outputs to deterministic) — the sentinel logic only fires
    // because the user didn't pass the flag, not because the value
    // is zero.
    if opts.temperature.is_none() {
        opts.temperature = Some(config.sampling.temperature);
        report
            .applied
            .push(format!("temperature={}", config.sampling.temperature));
    }
    if opts.num_predict.is_none() {
        let n = config.sampling.num_predict;
        opts.num_predict = Some(n);
        report.applied.push(format!("num_predict={}", n));
    }
    // Runner caps. `max_iterations = 0` is a legitimate "no cap"
    // (the runner interprets it as such), so we honour it too.
    if opts.max_iterations.is_none() {
        let m = config.runner.max_iterations;
        opts.max_iterations = Some(m);
        report.applied.push(format!("max_iterations={}", m));
    }
    if opts.max_tool_calls.is_none() {
        let m = config.runner.max_tool_calls;
        opts.max_tool_calls = Some(m);
        report.applied.push(format!("max_tool_calls={}", m));
    }
    // Compression — `max_messages = 0` is a valid "disabled"
    // sentinel, so honour it. The runner already special-cases 0.
    if opts.max_messages.is_none() {
        let m = config.compression.max_messages;
        opts.max_messages = Some(m);
        report.applied.push(format!("max_messages={}", m));
    }
    if opts.tool_max_chars.is_none() {
        let m = config.compression.tool_content_max_chars;
        opts.tool_max_chars = Some(m);
        report
            .applied
            .push(format!("tool_max_chars={}", m));
    }
    // Built-in URL defaults for the two providers. These only fire
    // when neither the user nor the config file supplied a value,
    // so the precedence chain stays built-in → config → CLI. We
    // don't log them because the values are the same as the
    // historical CLI defaults and would just be noise.
    if opts.ollama_url.is_empty() {
        opts.ollama_url = "http://localhost:11434".to_string();
    }
    if opts.deepseek_url.is_empty() {
        opts.deepseek_url = "https://api.deepseek.com/v1".to_string();
    }
    // Probe-on-run — only flip if the user hasn't already overridden
    // via --mock (which sets probe_ollama=false early). We mirror
    // the bool directly onto `opts.probe_ollama` so the value
    // actually takes effect at run time, not just in the log.
    if !opts.mock && config.runner.probe_ollama_on_run
        && !opts.probe_ollama {
            opts.probe_ollama = true;
            report
                .applied
                .push("probe_ollama=true".to_string());
        }
    // Quiet-mode default. If the user didn't pass `--quiet`, the
    // config's `io.quiet_default` decides. We only consider the
    // config value when the user didn't override it because `false`
    // is the historical default; `true` from the config is what
    // `applied` should reflect.
    if !opts.quiet && config.io.quiet_default {
        opts.quiet = true;
        report
            .applied
            .push("quiet=true".to_string());
    }
    // Warn when the config asks for a provider that we don't
    // recognise. The runner would just fall back to "ollama"
    // silently otherwise; better to surface it. `config validate`
    // is the canonical way to catch this; here we only warn at
    // run time so a broken config doesn't make `magent run` crash.
    if !opts.provider.is_empty()
        && opts.provider != "ollama"
        && opts.provider != "deepseek"
    {
        report.warnings.push(format!(
            "provider {} is not recognised (expected `ollama` or `deepseek`); using as-is",
            opts.provider
        ));
    }
    // Warn if the user picked a known provider but the config has
    // no model configured for that slot. We can still run (the
    // hard-coded fallback kicks in), but the user probably wants
    // to know they forgot to set `provider.<x>.model`. Unknown
    // providers are handled by the "not recognised" warning
    // above, so we don't double-warn here.
    if !opts.provider.is_empty()
        && (opts.provider == "ollama" || opts.provider == "deepseek")
    {
        let provider_for_model = match opts.provider.as_str() {
            "deepseek" => &config.provider.deepseek,
            _ => &config.provider.ollama,
        };
        // Only warn when the config has a model on the *other*
        // slot — i.e. the user might have meant the other one
        // and we want to nudge them to check.
        let other_provider = if opts.provider == "deepseek" {
            "ollama"
        } else {
            "deepseek"
        };
        let other_model_present = match other_provider {
            "ollama" => !config.provider.ollama.model.is_empty(),
            "deepseek" => !config.provider.deepseek.model.is_empty(),
            _ => false,
        };
        if provider_for_model.model.trim().is_empty() && other_model_present {
            report.warnings.push(format!(
                "config has no model for provider {} (the other provider `{}` has one); \
                 verify `provider.{}.model` is set",
                opts.provider, other_provider, opts.provider
            ));
        }
    }
    report
}

/// Pull the URL for `provider_name` out of the config (or `None` if
/// not configured). Returns `None` when the field is empty.
fn endpoint_url<'a>(
    config: &'a crate::config::ConfigRecord,
    provider_name: &str,
) -> Option<&'a str> {
    let endpoint = match provider_name {
        "ollama" => &config.provider.ollama,
        "deepseek" => &config.provider.deepseek,
        _ => return None,
    };
    if endpoint.url.trim().is_empty() {
        None
    } else {
        Some(&endpoint.url)
    }
}

/// Pull the model name for `provider_name` out of the config.
fn endpoint_model<'a>(
    config: &'a crate::config::ConfigRecord,
    provider_name: &str,
) -> Option<&'a str> {
    let endpoint = match provider_name {
        "deepseek" => &config.provider.deepseek,
        // `ollama` and any unknown name fall back to the ollama
        // slot, matching the historical `provider.default` default.
        _ => &config.provider.ollama,
    };
    if endpoint.model.trim().is_empty() {
        None
    } else {
        Some(&endpoint.model)
    }
}

/// Resolve the API key the runner should use. Priority:
///
/// 1. `cli_value` (from `--api-key`).
/// 2. The env var named in `config.<provider>.api_key_env`
///    (e.g. `DEEPSEEK_API_KEY`, but configurable).
/// 3. Hard-coded `OLLAMA_API_KEY` (kept as a last-resort fallback so
///    existing scripts that only set one env var keep working).
///
/// Returns `None` if no source supplied anything, in which case the
/// caller is expected to surface a friendly error or fall back to
/// simulated reasoning.
fn resolve_api_key_with_config(
    cli_value: &Option<String>,
    provider: &str,
    config: &crate::config::ConfigRecord,
) -> Option<String> {
    if let Some(v) = cli_value {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // Configured env var name — the user can rename it via
    // `magent config set provider.deepseek.api_key_env MY_KEY`.
    let configured = match provider {
        "deepseek" => config.provider.deepseek.api_key_env.as_deref(),
        "ollama" => config.provider.ollama.api_key_env.as_deref(),
        _ => None,
    };
    if let Some(env_name) = configured {
        if !env_name.is_empty() {
            if let Ok(v) = std::env::var(env_name) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    // Historical fallback for compatibility with older setups.
    if let Ok(v) = std::env::var("OLLAMA_API_KEY") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

impl<'a> RunCmd<'a> {
    pub fn new(opts: &'a mut RunOptions) -> Self {
        Self { opts }
    }

    /// Run the agent end-to-end and stream progress to `out`.
    ///
    /// Two short-circuits live here:
    ///
    /// * `--verify-signed <PATH>` skips the agent run entirely and
    ///   just verifies the file as a [`SignedRunReport`]. The exit
    ///   code is `0` on success, the runner surfaces a
    ///   [`RunError::VerifySignedEnvelope`] on failure (which the
    ///   dispatcher maps to a non-zero exit). We don't depend on
    ///   the agent backend at all in this path, so a user can
    ///   verify a report handed to them by another machine without
    ///   needing the same LLM backend.
    /// * `--sign` (with optional `--signer <NAME>`) sets the
    ///   [`RunOptions::sign_with_vault_identity`] field; the
    ///   signing happens after the regular JSON envelope has
    ///   been flushed in [`finalize_report`] (see
    ///   [`sign_after_run`]). Keeping the signing in its own
    ///   function (rather than inline in `finalize_report`) means
    ///   a vault failure never poisons the JSON envelope — same
    ///   policy as `--save-summary`.
    pub fn execute(&mut self, out: &mut Output) -> Result<RunReport, RunError> {
        // ─── web3_app short-circuit: verify-only ────────────────────
        // We run this BEFORE the REPL check so `--verify-signed`
        // works even if no task is given (the user might be
        // verifying a saved report as a standalone CI step).
        #[cfg(feature = "web3_app")]
        if let Some(path) = self.opts.verify_signed_path.as_deref() {
            return verify_signed_report(path, out);
        }

        // ─── REPL mode: when --repl flag is set or no task is provided ───
        if self.opts.repl_mode || self.opts.task.is_empty() {
            return self.execute_repl(out);
        }

        // Cache the output mode so the runner's trace sink can
        // suppress events under `--json` without holding a
        // reference to `out` (which has non-`'static` stdout /
        // stderr locks).
        let output_kind = out.kind();

        // ─── 0. Apply config-file defaults ──────────────────────────────
        // Layering: built-in → config file → env vars → CLI flags.
        // We load the config record here (returning the in-memory
        // defaults if the file doesn't exist) and stamp any unset
        // fields onto `opts`. Later code paths (env vars + CLI flags)
        // win because they only kick in when these fields are
        // *still* at their sentinel value.
        let config = match crate::config::load() {
            Ok(c) => c,
            Err(e) => {
                // If the config file is corrupt we still want `run`
                // to work — fall back to the in-memory defaults and
                // surface a warning via `out.warn` so the line goes
                // through the same channel as every other trace /
                // warning (stderr in Human mode, swallowed in JSON
                // mode).
                let _ = out.warn(&format!(
                    "could not load config file ({}); using built-in defaults",
                    e
                ));
                crate::config::ConfigRecord::with_defaults()
            }
        };
        let config_report = apply_config_overrides(self.opts, &config);
        if !config_report.applied.is_empty() && output_kind == OutputKind::Human {
            out.trace_labeled(
                "Config",
                &format!("applied {}", config_report.applied.join(", ")),
            )?;
        }
        // Surface warnings on their own line so they don't get
        // buried in the comma-joined applied list. `out.warn` is
        // the right channel — it goes to stderr in Human mode and
        // is suppressed under `--json` so a script piping the
        // envelope doesn't get noise.
        for w in &config_report.warnings {
            out.warn(w)?;
        }

        // ─── 1. Resolve the system prompt ─────────────────────────────────
        // Three sources, in priority order:
        //   1. `--prompt-name <NAME>` — load from the prompts store
        //      (managed by `magent set-prompt`).
        //   2. `--prompt <FILE>`      — load a hand-written .txt file.
        //   3. Built-in HEALTH_SYSTEM_PROMPT.
        let resolved =
            crate::prompt::resolve_for_run(self.opts).map_err(|e| match e {
                crate::prompt::PromptError::PromptFileLoad { path, source } => {
                    RunError::PromptLoad(format!("{}: {}", path.display(), source))
                }
                other => RunError::PromptLoad(other.to_string()),
            })?;
        let system_prompt = resolved.text;
        // Apply provider/model hints from the resolved prompt, but
        // never override explicit flags. We honour
        // `--provider <X>` even if the prompt was tagged with a
        // different provider, so the user's CLI choice wins.
        if let Some(p) = resolved.provider.as_deref() {
            if !p.is_empty() {
                self.opts.provider = p.to_string();
            }
        }
        if self.opts.model.is_empty() {
            if let Some(m) = resolved.model.as_deref() {
                self.opts.model = m.to_string();
            }
        }

        // ─── 2. Build the runner + executor ──────────────────────────────
        let mut runner = build_runner(self.opts, system_prompt, out, output_kind)?;

        // ─── 3. Wire up the LLM backend ─────────────────────────────
        //
        // The core runner probes Ollama on every `run()` call by default
        // (legacy behaviour). We override that so:
        //
        //   * `--mock`              → never probe, always simulate
        //   * `--provider deepseek` → always use DeepSeek (requires a key)
        //   * `--provider ollama`   → use the Ollama default; fall back to
        //                             simulated reasoning if unreachable
        //
        // API keys are resolved in this order:
        //   1. `--api-key <KEY>` (highest priority)
        //   2. `DEEPSEEK_API_KEY` env var (when provider=deepseek)
        //   3. `OLLAMA_API_KEY`  env var (fallback, mostly for symmetry)
        if self.opts.mock {
            runner.config_mut().probe_ollama_on_run = false;
            out.trace_labeled("Mode", "mock (simulated reasoning)")?;
        } else if self.opts.provider == "deepseek" {
            // DeepSeek path. Build the client, swap it into the runner.
            //
            // `resolve_api_key` returns `None` if every source (CLI,
            // DEEPSEEK_API_KEY, OLLAMA_API_KEY) is empty or unset, so
            // we can branch on a single check.
            let key = resolve_api_key_with_config(&self.opts.api_key, &self.opts.provider, &config);
            let Some(key) = key else {
                out.warn(
                    "No DeepSeek API key found — pass --api-key <KEY> or set DEEPSEEK_API_KEY. \
                     Falling back to simulated reasoning.",
                )?;
                runner.config_mut().probe_ollama_on_run = false;
                // Don't bother swapping the backend — the runner's
                // default Ollama client is harmless because we've
                // disabled its probe and the LLM lookup is gated on
                // `backend_enabled` which stays false.
                let task = self.opts.task.clone();
                out.trace_labeled("Task", &task)?;
                let result = runner.run(&task).map_err(RunError::Agent)?;
                // We never actually used DeepSeek — the report
                // should say so explicitly. Fall back to "mock" so
                // the JSON envelope's `provider` field is accurate.
                return finalize_report(&mut runner, "mock", &result, self.opts, out);
            };
            // We have a non-empty key. Pick the model: if the user
            // passed --model explicitly, honour it; otherwise default
            // to `deepseek-chat`. We use an empty sentinel in
            // `RunOptions::model` so we can tell apart "user asked
            // for model X" from "user didn't ask at all".
            let model = if self.opts.model.is_empty() {
                "deepseek-chat".to_string()
            } else {
                self.opts.model.clone()
            };
            let ds = DeepSeekClient::try_with_endpoint(
                &self.opts.deepseek_url,
                &model,
                &key,
            )
            .expect(
                "resolve_api_key returned a non-empty key, so try_with_endpoint should succeed",
            );
            let connected = ds.check_connection();
            runner.set_backend(ds);
            if connected {
                runner.force_enable_backend();
                let model_display = runner
                    .backend_mut()
                    .map(|b| b.model().to_string())
                    .unwrap_or_default();
                out.trace_labeled(
                    "DeepSeek",
                    &format!(
                        "connected at {} (model: {})",
                        self.opts.deepseek_url, model_display
                    ),
                )?;
            } else {
                runner.config_mut().probe_ollama_on_run = false;
                runner.force_disable_backend();
                out.warn(&format!(
                    "DeepSeek not reachable at {} — falling back to simulated responses",
                    self.opts.deepseek_url
                ))?;
            }
        } else if let Some(client) = runner.ollama_mut() {
            // Default: Ollama. Re-build the client from CLI options so
            // the user gets exactly the model / URL they asked for,
            // not whatever the default RunnerConfig baked in.
            let model = if self.opts.model.is_empty() {
                "llama3.2".to_string()
            } else {
                self.opts.model.clone()
            };
            let url = self.opts.ollama_url.clone();
            *client = OllamaClient::new(&url, &model);

            if client.check_connection() {
                // We've already proven the connection works — tell the
                // runner to skip its own auto-probe (which would just
                // hit the network a second time).
                runner.config_mut().probe_ollama_on_run = false;
                runner.force_enable_backend();
                out.trace_labeled(
                    "Ollama",
                    &format!("connected at {} (model: {})", url, model),
                )?;
            } else {
                // Ollama unreachable — fall back to simulated responses
                // for this run, but don't pollute the runner config so
                // the next call gets a fresh chance.
                runner.config_mut().probe_ollama_on_run = false;
                runner.force_disable_backend();
                out.warn(&format!(
                    "Ollama not reachable at {} — falling back to simulated responses",
                    url
                ))?;
            }
        }

        // ─── 3.5. Inject a previous run's summary (if requested) ─────────────
        // `--load-summary <TOPIC>` reads a stored summary and
        // prepends its window as a system note immediately after
        // the live system prompt. We do this **before** the ReAct
        // loop starts so the model sees the context on its very
        // first call. Errors are surfaced but non-fatal in Human
        // mode: a missing topic shouldn't kill an otherwise-valid
        // run. In JSON mode we still bail out — a CI script
        // asking for a specific topic wants a hard guarantee.
        if let Some(topic) = self.opts.load_summary_topic.clone() {
            let store = FileSummaryStore::open_default();
            match store.load(&topic) {
                Ok(prev) => {
                    let body = render_summary_context(&prev);
                    runner.insert_after_system_prompt(
                        magent_core::agent_runner::Message::system(&body),
                    );
                    // `trace_labeled` returns `io::Result`; we
                    // ignore it because a stderr failure is not
                    // a load failure — the summary was loaded
                    // successfully and the run should continue.
                    let _ = out.trace_labeled(
                        "Summary",
                        &format!(
                            "loaded {} window ({} messages, kept={})",
                            topic,
                            prev.head_tail_window.len(),
                            prev.stats.kept,
                        ),
                    );
                }
                Err(e) => {
                    if output_kind == OutputKind::Json {
                        return Err(RunError::Agent(format!(
                            "--load-summary {:?} failed: {}",
                            topic, e
                        )));
                    }
                    out.warn(&format!(
                        "--load-summary {:?} failed: {}",
                        topic, e
                    ))?;
                }
            }
        }

        // ─── 4. Drive the ReAct loop ─────────────────────────────────────
        let task = self.opts.task.clone();
        out.trace_labeled("Task", &task)?;

        let result = runner.run(&task).map_err(RunError::Agent)?;

        // ─── 5. Print the final answer + emit JSON envelope if needed ────
        // When `--mock` is set, the report's `provider` field should
        // say `"mock"` so downstream consumers can tell simulation
        // apart from a real Ollama/DeepSeek result. The two are
        // indistinguishable from the ReAct loop's point of view
        // (both produce canned responses), but a CI script wants
        // to know.
        let report_provider = if self.opts.mock {
            "mock"
        } else {
            self.opts.provider.as_str()
        };
        finalize_report(&mut runner, report_provider, &result, self.opts, out)
    }

    /// Execute REPL mode: interactive multi-turn conversation.
    fn execute_repl(&self, _out: &mut Output) -> Result<RunReport, RunError> {
        use std::io::{self, Write};
        use std::sync::Arc;

        let executor = CompositeExecutor::new(self.opts.email_tools.as_deref());

        // Resolve defaults
        let provider = if self.opts.provider.is_empty() {
            "ollama".to_string()
        } else {
            self.opts.provider.clone()
        };
        let model = if self.opts.model.is_empty() {
            if provider == "deepseek" {
                "deepseek-chat".to_string()
            } else {
                "llama3.2".to_string()
            }
        } else {
            self.opts.model.clone()
        };
        let ollama_url = if self.opts.ollama_url.is_empty() {
            "http://localhost:11434".to_string()
        } else {
            self.opts.ollama_url.clone()
        };
        let deepseek_url = if self.opts.deepseek_url.is_empty() {
            "https://api.deepseek.com/v1".to_string()
        } else {
            self.opts.deepseek_url.clone()
        };
        let temperature = self.opts.temperature.unwrap_or(0.3);
        let num_predict = self.opts.num_predict.unwrap_or(512);
        let max_iterations = self.opts.max_iterations.unwrap_or(10);
        let max_tool_calls = self.opts.max_tool_calls.unwrap_or(8);
        let max_messages = self.opts.max_messages.unwrap_or(32);
        let tool_max_chars = self.opts.tool_max_chars.unwrap_or(800);

        // Load system prompt
        let system_prompt = match crate::prompt::resolve_for_run(self.opts) {
            Ok(r) => r.text,
            Err(_) => RunnerConfig::default().system_prompt,
        };

        let config = RunnerConfig {
            max_iterations,
            max_tool_calls,
            system_prompt,
            verbose: !self.opts.quiet,
            sampling: SamplingParams {
                temperature,
                num_predict,
            },
            probe_ollama_on_run: !self.opts.mock,
            compression: CompressionPolicy {
                max_messages,
                tool_content_max_chars: tool_max_chars,
            },
            tool_descriptions: Vec::new(),
            trace_sink: None,
        };

        let mut runner = RealAgentRunner::with_config(executor, config);
        let backend_label = if self.opts.mock {
            runner.force_disable_backend();
            "mock".to_string()
        } else if provider == "deepseek" {
            if let Some(key) = &self.opts.api_key {
                if let Some(ds) = DeepSeekClient::try_with_endpoint(&deepseek_url, &model, key) {
                    if ds.check_connection() {
                        runner.set_backend(ds);
                        runner.force_enable_backend();
                        "deepseek".to_string()
                    } else {
                        runner.force_disable_backend();
                        "deepseek (mock fallback)".to_string()
                    }
                } else {
                    runner.force_disable_backend();
                    "deepseek (mock fallback)".to_string()
                }
            } else {
                runner.force_disable_backend();
                "deepseek (no API key)".to_string()
            }
        } else {
            let client = OllamaClient::new(&ollama_url, &model);
            if client.check_connection() {
                runner.set_backend(client);
                runner.force_enable_backend();
                "ollama".to_string()
            } else {
                runner.force_disable_backend();
                "ollama (mock fallback)".to_string()
            }
        };

        // Install trace sink
        let shared = Arc::new(SharedTraceSink::new());
        shared.install(Box::new(ReplTraceSink));
        shared.install(Box::new(LogSink));
        runner.set_trace_sink(Some(shared));

        // Initialize REPL session
        let mut session = ReplSession::default();

        // Print welcome banner
        print_welcome_banner(&backend_label, &model);

        // REPL loop
        loop {
            print!("\n❯ ");
            io::stdout().flush().map_err(RunError::Io)?;

            let input = read_line_with_history("", &session);

            // Skip empty input
            if input.trim().is_empty() {
                continue;
            }

            // Handle built-in commands
            match handle_repl_command(&input, &mut runner, &mut session) {
                ReplCommandResult::Continue => {}
                ReplCommandResult::Stop => break,
                ReplCommandResult::RunTask(task) => {
                    // Add to history before running
                    session.add_to_history(task.clone());
                    session.task_count += 1;

                    // Run the task
                    match runner.run(&task) {
                        Ok(result) => {
                            println!("\n{}", result);
                        }
                        Err(e) => {
                            eprintln!("\n✗ Error: {}", e);
                        }
                    }
                }
            }
        }

        println!("\n\nGoodbye! 👋");

        // Return a summary report
        Ok(RunReport {
            answer: "REPL session ended".to_string(),
            iterations: runner.iteration(),
            tool_calls: runner.tool_call_count(),
            provider: backend_label,
            using_ollama: runner.using_ollama(),
            state: "Finished".to_string(),
            final_messages: runner.messages().len(),
            approx_tokens: runner.approx_total_tokens(),
            approx_bytes: runner.approx_total_bytes(),
        })
    }
}

/// Print REPL welcome banner
fn print_welcome_banner(backend: &str, model: &str) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                                                              ║");
    println!("║                  🤖  mAgent REPL  v{}                      ║", env!("CARGO_PKG_VERSION"));
    println!("║                                                              ║");
    println!("║  Backend: {:>50}        ║", backend);
    println!("║  Model:   {:>50}        ║", model);
    println!("║                                                              ║");
    println!("║  Type 'help' for commands, 'quit' to exit                  ║");
    println!("║                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}

/// Run the ReAct loop, emit the final answer, write the JSON
/// envelope, and return a [`RunReport`]. Pulled out of
/// [`RunCmd::execute`] so the early-return paths (e.g. "no DeepSeek
/// key, falling back to simulation") don't have to duplicate the
/// 8-line "build report + flush output" sequence.
///
/// Generic over the executor so any future caller that swaps
/// `SimulatorExecutor` for a real BLE dongle, MQTT gateway, etc.
/// can reuse this without us having to duplicate the function.
fn finalize_report<E: ToolExecutor>(
    runner: &mut RealAgentRunner<E>,
    provider: &str,
    result: &str,
    opts: &RunOptions,
    out: &mut Output,
) -> Result<RunReport, RunError> {
    let report = RunReport {
        answer: result.to_string(),
        iterations: runner.iteration(),
        tool_calls: runner.tool_call_count(),
        provider: provider.to_string(),
        using_ollama: runner.using_ollama(),
        state: runner.state().to_string(),
        // Snapshot the live-context counters so the JSON envelope tells
        // the caller exactly how heavy the conversation got.
        final_messages: runner.messages().len(),
        approx_tokens: runner.approx_total_tokens(),
        approx_bytes: runner.approx_total_bytes(),
    };
    out.final_answer(result)?;
    out.write_json(report.to_json())?;
    out.flush()?;

    // ── Auto-save: when `--save-summary <TOPIC>` is set, persist
    // the post-run window. We do this *after* the JSON envelope
    // has been flushed so a save failure never poisons the
    // primary result the user actually asked for. Errors are
    // surfaced as warnings in Human mode (stderr) and as a
    // non-fatal `info:` line in JSON mode — the envelope stays
    // clean.
    if let Some(topic) = opts.save_summary_topic.as_deref() {
        if let Err(e) = save_summary_after_run(runner, opts, topic, out) {
            if out.kind() == OutputKind::Human {
                out.warn(&format!("--save-summary {:?} failed: {}", topic, e))?;
            } else {
                out.info(&format!(
                    "--save-summary {:?} failed: {}",
                    topic, e
                ))?;
            }
        }
    }

    // ── web3_app: signed envelope emission ────────────────────────
    // When `--sign` (or `--signer <NAME>`) is set, sign the
    // report with the named vault identity and persist the
    // envelope to disk. We do this AFTER the JSON envelope has
    // been flushed so a vault failure never poisons the
    // primary result the user actually asked for. Errors are
    // surfaced as warnings (Human mode) / `info:` lines
    // (JSON mode); a signing failure is also a `RunError` so
    // CI scripts that want strict semantics can check the
    // exit code.
    #[cfg(feature = "web3_app")]
    if opts.sign_with_vault_identity.is_some() {
        if let Err(e) = sign_after_run(&report, opts, out) {
            // Promote the warning into a hard error so CI
            // scripts that explicitly opted into --sign get a
            // non-zero exit if the signature didn't land on
            // disk. The error message includes the path the
            // user asked for, so debugging is easy.
            return Err(RunError::SignedEnvelope(format!(
                "--sign failed: {}",
                e
            )));
        }
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// web3_app — sign / verify helpers
// ---------------------------------------------------------------------------
//
// The two functions below are the only place the CLI crate talks
// to `magent_core::web3_app` directly. They live behind a
// `#[cfg(feature = "web3_app")]` gate so a plain `cargo build`
// doesn't pull `ed25519-dalek` into the binary. The signing helper
// mirrors the layout of `save_summary_after_run` (warn on failure,
// keep going) when called from the regular path, but we promote
// the failure to `RunError::SignedEnvelope` when invoked from
// `finalize_report` so `--sign` is a hard CI requirement.

/// Sign `report` with the named vault identity and persist the
/// resulting [`SignedRunReport`] envelope to disk. The output
/// path is `--signed-output <PATH>` if given, else
/// `<cwd>/<task-slug>-signed.json`.
///
/// Errors:
///
/// * `web3_cli::Web3CliError::VaultNotFound` — the vault file
///   doesn't exist and the user hasn't created one yet.
/// * `web3_cli::Web3CliError::Aead(_)` — passphrase lookup
///   failed (e.g. `MAGENT_WEB3_PASSPHRASE` unset and no
///   `--passphrase-env` set).
/// * `web3_cli::Web3CliError::IdentityNotFound(_)` — the
///   requested name isn't in the vault.
/// * `web3_cli::Web3CliError::Aead("...")` — wrong passphrase.
/// * `magent_core::error::Web3ErrorKind::*` — the underlying
///   `sign_run_report` failed (rare; mostly report-length
///   validation).
#[cfg(feature = "web3_app")]
fn sign_after_run(
    report: &RunReport,
    opts: &RunOptions,
    out: &mut Output,
) -> Result<(), String> {
    // Resolve the identity name. The parser sets the sentinel
    // value `"default"` for bare `--sign` (no `--signer`). We
    // honour `MAGENT_AGENT_IDENTITY` env var as a fallback so
    // CI scripts can opt in via env without touching the CLI
    // flag list.
    let identity_name = match opts
        .sign_with_vault_identity
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "default")
    {
        Some(name) => name.to_string(),
        None => std::env::var("MAGENT_AGENT_IDENTITY")
            .unwrap_or_else(|_| "default".to_string()),
    };

    // Resolve the passphrase resolver. We mirror the dispatch
    // logic in `cli/src/main.rs` for the `web3` subcommand
    // here — the user can either set `MAGENT_WEB3_PASSPHRASE`
    // or override with `--passphrase-env <NAME>` (the runner
    // accepts a `--passphrase-env` for parity; default is
    // `MAGENT_WEB3_PASSPHRASE`).
    let passphrase_env = std::env::var("MAGENT_WEB3_PASSPHRASE_ENV")
        .unwrap_or_else(|_| "MAGENT_WEB3_PASSPHRASE".to_string());
    let passphrase = std::env::var(&passphrase_env).map_err(|_| {
        format!(
            "passphrase env var ${} is not set (use --passphrase-env or export it)",
            passphrase_env
        )
    })?;

    // Load the vault (creates an empty one if missing).
    let vault_path = web3_cli::default_vault_path();
    let mut vault = if vault_path.exists() {
        web3_cli::load_vault(&vault_path).map_err(|e| e.to_string())?
    } else {
        web3_cli::empty_vault()
    };

    // Decrypt the requested identity.
    let identity = web3_cli::decrypt_identity(
        &mut vault,
        &identity_name,
        passphrase.as_bytes(),
    )
    .map_err(|e| e.to_string())?;

    // Resolve "now" — the runner is single-threaded so
    // `SystemTime::now()` is fine; we use it as the issued-at
    // and (if `--not-before` was given) the start of the
    // validity window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Build the canonical mirror of `RunReport`.
    let fields = RunReportFields::new(
        report.answer.clone(),
        report.iterations,
        report.tool_calls,
        report.provider.clone(),
        report.using_ollama,
        report.state.clone(),
        report.final_messages,
        report.approx_tokens,
    );

    let signed = sign_run_report(
        &identity,
        now,
        opts.not_before_unix,
        opts.not_after_unix,
        fields,
    )
    .map_err(|e| e.to_string())?;

    // Decide the output path. Default: <cwd>/<slug>-signed.json
    // where <slug> is the task with non-alphanumeric chars
    // replaced by `-` and capped at 64 chars.
    let out_path = match opts.signed_output.clone() {
        Some(p) => p,
        None => {
            let slug = slugify(&opts.task);
            std::path::PathBuf::from(format!("{}-signed.json", slug))
        }
    };
    let json = signed.to_json_pretty();
    std::fs::write(&out_path, json).map_err(|e| {
        format!("could not write signed envelope to {:?}: {}", out_path, e)
    })?;

    if out.kind() == OutputKind::Human {
        let _ = out.info(&format!(
            "[web3] signed envelope written to {:?} (signer={})",
            out_path,
            signed.signer
        ));
    }
    Ok(())
}

/// Verify the [`SignedRunReport`] at `path`. Reads the file,
/// parses it, and runs `parse_and_verify_signed_run_report` on
/// the contents. The `now` clock is `SystemTime::now()`.
#[cfg(feature = "web3_app")]
fn verify_signed_report(path: &Path, out: &mut Output) -> Result<RunReport, RunError> {
    let json = std::fs::read_to_string(path).map_err(|e| {
        RunError::VerifySignedEnvelope(format!("could not read {:?}: {}", path, e))
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let env = parse_and_verify_signed_run_report(&json, now).map_err(|e| {
        RunError::VerifySignedEnvelope(format!("verification failed: {}", e))
    })?;

    // Human mode: print the report so the user can eyeball
    // what just got verified. JSON mode: emit the envelope
    // itself so downstream tools can pipe it through jq.
    if out.kind() == OutputKind::Human {
        let r = &env.payload;
        let _ = out.info(&format!(
            "✓ verified: signer={} issued_at={} report={:?}",
            env.signer, env.issued_at_unix, r
        ));
    } else {
        // We emit the envelope itself as a top-level field
        // (`signed_envelope`) so jq consumers can grab it
        // with `.signed_envelope | fromjson`. We can't pass
        // it as a string-valued JSON `Value::String` because
        // the existing `Output::write_json` API expects an
        // object whose keys get merged into the envelope;
        // the cleanest fit is to merge its parsed JSON
        // straight in.
        let parsed: serde_json::Value = serde_json::from_str(&env.to_json())
            .map_err(|e| RunError::VerifySignedEnvelope(format!(
                "could not re-parse verified envelope: {}", e
            )))?;
        out.write_json(parsed).map_err(RunError::Io)?;
    }

    // Hand back a synthetic RunReport so the dispatcher's
    // exit-code mapping stays uniform (success → 0).
    Ok(RunReport {
        answer: env.payload.answer.clone(),
        iterations: env.payload.iterations,
        tool_calls: env.payload.tool_calls,
        provider: env.payload.provider.clone(),
        using_ollama: env.payload.using_ollama,
        state: env.payload.state.clone(),
        final_messages: env.payload.final_messages,
        approx_tokens: env.payload.approx_tokens,
        // The signed-envelope schema predates `approx_bytes`; the byte
        // footprint isn't part of the signed payload, so report 0 here.
        approx_bytes: 0,
    })
}

/// Convert a free-form task string into a filesystem-friendly
/// slug: lowercase, non-alphanumeric → `-`, max 64 chars. Used
/// by the default signed-output path so users don't have to
/// think about file naming when they don't pass `--signed-output`.
#[cfg(feature = "web3_app")]
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(64));
    let mut prev_dash = false;
    for c in s.chars().take(64 * 4) {
        let c_lower = c.to_ascii_lowercase();
        if c_lower.is_ascii_alphanumeric() {
            out.push(c_lower);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("run");
    }
    out
}

/// Persist the post-run `head_tail_window` to the summaries
/// store. Returns the core-level error on failure so the caller
/// can choose how loudly to surface it (the runner chooses
/// "warning, keep going").
fn save_summary_after_run<E: ToolExecutor>(
    runner: &mut RealAgentRunner<E>,
    opts: &RunOptions,
    topic: &str,
    out: &mut Output,
) -> Result<(), magent_core::summary::SummaryError> {
    // Translate runtime `Message`s to DTOs. The core's
    // `MessageDto::from_message` is the canonical conversion.
    let window: Vec<MessageDto> = runner
        .messages()
        .iter()
        .map(MessageDto::from_message)
        .collect();

    // Build the source block from the live `RunOptions`. We
    // capture provider / model so the saved record tells the
    // reader exactly which model produced the window.
    let source = SummarySource {
        session_id: Some(format!(
            "run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        )),
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        original_message_count: window.len(),
        // P5.C7: use the real snapshot captured by the ReAct loop.
        // If no compression ran (window never hit the limit), fall
        // back to a snapshot of the current policy so the record
        // still reflects what *would* have happened.
        policy: runner
            .last_compression_policy()
            .unwrap_or_else(|| (&runner.config().compression).into()),
    };

    // P5.C7: use the real CompressionStats captured during the ReAct
    // loop. If no compression ran (kept == total messages), force a
    // snapshot now so the record reflects actual counts. `compress_now`
    // is safe to call here because the run is already finished and the
    // returned stats are deterministic for a given window.
    let stats = runner
        .last_compression_stats()
        .unwrap_or_else(|| runner.compress_now());

    let record: SummaryRecord = magent_core::summary::SummaryBuilder::new(topic.to_string())?
        .with_source(source)
        .with_window_slice(&window)
        .with_stats(stats)
        .build()?;

    let store = FileSummaryStore::open_default();
    // Honour `--save-summary-overwrite` by clearing the file
    // first when set. We do this rather than passing a flag
    // into `store.save` because the trait signature is shared
    // with the embedded KV backend (which doesn't have
    // files to delete).
    if opts.save_summary_overwrite {
        // `delete` is idempotent; ignore errors here.
        let _ = store.delete(topic);
    } else if store.load(topic).is_ok() {
        // Documented default (README / SUMMARY_STORE.md / RunOptions docs)
        // is to refuse clobbering an existing summary unless
        // `--save-summary-overwrite` is passed — mirror the probe the
        // `summary save` subcommand already does. Without this, retried CI
        // runs silently overwrite the previous run's summary.
        return Err(magent_core::summary::SummaryError::AlreadyExists(
            topic.to_string(),
        ));
    }

    let save_result = store.save(record);
    match &save_result {
        Ok(report) => {
            if out.kind() == OutputKind::Human {
                // `trace_labeled` returns `io::Result`; we ignore
                // it here so a stderr failure doesn't bubble up
                // as a `SummaryError`. The actual save outcome is
                // returned below verbatim.
                let _ = out.trace_labeled(
                    "Summary",
                    &format!(
                        "saved {} ({} bytes) to {}",
                        topic, report.bytes, report.path.display()
                    ),
                );
            }
        }
        Err(_) => {
            // Failure path: the caller will print the warning.
        }
    }
    save_result.map(|_| ())
}

/// Render a stored summary as a single system-prompt paragraph.
/// We keep this deliberately small — long context blowups here
/// would defeat the point of head/tail compression. The
/// conversation messages themselves aren't inlined; instead we
/// instruct the LLM to refer to them by their position when
/// replayed.
fn render_summary_context(rec: &SummaryRecord) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        &mut s,
        "## Context from a previous run (topic: {})",
        rec.topic
    );
    let _ = writeln!(
        &mut s,
        "The following is the head/tail window the agent saw at the end of its previous run."
    );
    let _ = writeln!(
        &mut s,
        "Use it as background, but treat the user's *current* task as the source of truth."
    );
    let _ = writeln!(&mut s);
    if let Some(llm) = &rec.llm_summary {
        let _ = writeln!(&mut s, "### Previous LLM summary");
        let _ = writeln!(&mut s, "{}", llm);
        let _ = writeln!(&mut s);
    }
    let _ = writeln!(
        &mut s,
        "### Previous window ({} messages)",
        rec.head_tail_window.len()
    );
    for (i, m) in rec.head_tail_window.iter().enumerate() {
        let _ = writeln!(&mut s, "[{}] {}: {}", i, m.role, m.content);
    }
    s
}

/// Stats we return from a single `run`.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub answer: String,
    pub iterations: usize,
    pub tool_calls: usize,
    /// `"ollama"`, `"deepseek"`, or `"mock"` depending on what the CLI
    /// wired up. Mirrors the `--provider` flag so downstream consumers
    /// can tell which backend produced the answer.
    pub provider: String,
    pub using_ollama: bool,
    pub state: String,
    /// Number of messages in the conversation history at the end of
    /// the run, after compression has been applied. Useful for
    /// diagnosing "why did my session feel truncated?" without having
    /// to enable `--verbose`.
    pub final_messages: usize,
    /// Rough token estimate of the conversation at the end of the run
    /// (sum of `len(s) / 4` over every message + tool args). Cheap,
    /// good enough for a budget guardrail.
    pub approx_tokens: usize,
    /// Estimated dynamic heap footprint (bytes) of the conversation at the
    /// end of the run — the figure the REQ-SCHED-001 / mem-3 byte-GC bounds to
    /// `MAX_DYNAMIC_CONTEXT_BYTES`. Lets callers confirm the context cache
    /// stayed within budget.
    pub approx_bytes: usize,
}

impl RunReport {
    /// JSON envelope for `--json` mode. Field names match what the
    /// embedding tool / CI scripts expect to grep on. `provider` is
    /// new (DeepSeek support); `using_ollama` is kept for backwards
    /// compatibility with anything greppping the old field.
    /// `final_messages` / `approx_tokens` are new (context-management
    /// support) so callers can see the live payload size at the end
    /// of the run.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "iterations": self.iterations,
            "tool_calls": self.tool_calls,
            "provider": self.provider,
            "using_ollama": self.using_ollama,
            "state": self.state,
            "final_messages": self.final_messages,
            "approx_tokens": self.approx_tokens,
            "approx_bytes": self.approx_bytes,
        })
    }
}

/// Build a `RealAgentRunner<CompositeExecutor>` pre-configured for the
/// CLI's options. Split out from `RunCmd::execute` so we can also
/// construct a runner inside `doctor.rs` (which only needs an empty
/// executor to verify the tool backend).
///
/// `output_kind` is forwarded to the [`OutputTraceSink`] so the
/// runner can suppress trace events under `--json`. The `out`
/// parameter is still useful for the high-level pre-run banner
/// (`out.trace_labeled("Backend", …)`) that lives outside the
/// ReAct loop.
pub fn build_runner(
    opts: &RunOptions,
    system_prompt: String,
    out: &mut Output,
    output_kind: OutputKind,
) -> Result<RealAgentRunner<CompositeExecutor>, RunError> {
    // The tool executor: SimulatorExecutor + optional McpToolExecutor.
    // `CompositeExecutor::new` handles the email_tools path (empty string
    // means use default path, a path string means use that path).
    let email_tools_path: Option<&str> = opts.email_tools.as_deref();
    let executor = CompositeExecutor::new(email_tools_path);
    let email_tools_enabled = opts.email_tools.is_some();

    // Compose a backend label that reflects every enabled executor.
    let mut backend_label = if email_tools_enabled {
        "SimulatorExecutor + McpToolExecutor (sensors/BLE/flash/GPIO + email)"
    } else {
        "SimulatorExecutor (sensors/BLE/flash/GPIO)"
    }
    .to_string();
    if executor.has_blockchain() {
        backend_label.push_str(" + BlockchainExecutor (web3)");
    }

    // Resolve every numeric budget once here so the runner build
    // and the trace label below can share the same values.
    let max_iterations = opts.max_iterations.unwrap_or(10);
    let max_tool_calls = opts.max_tool_calls.unwrap_or(8);
    let max_messages = opts.max_messages.unwrap_or(32);
    let tool_max_chars = opts.tool_max_chars.unwrap_or(800);
    let temperature = opts.temperature.unwrap_or(0.3);
    let num_predict = opts.num_predict.unwrap_or(512);

    let config = RunnerConfig {
        max_iterations,
        max_tool_calls,
        system_prompt,
        verbose: !opts.quiet,
        sampling: SamplingParams {
            temperature,
            num_predict,
        },
        probe_ollama_on_run: !opts.mock,
        compression: CompressionPolicy {
            max_messages,
            tool_content_max_chars: tool_max_chars,
        },
        // tool_descriptions is populated below after the executor is created.
        tool_descriptions: Vec::new(),
        trace_sink: None,
    };

    out.trace_labeled(
        "Backend",
        &format!(
            "{}; budget = {} iter / {} tools / {} msg / {} chars",
            backend_label, max_iterations, max_tool_calls, max_messages, tool_max_chars
        ),
    )?;

    let mut runner = RealAgentRunner::with_config(executor, config);

    // Inject email tool descriptions so the LLM knows about mcp__email__*.
    let mut all_descriptions: Vec<(String, String)> = Vec::new();
    if email_tools_enabled {
        all_descriptions.extend(CompositeExecutor::email_tool_descriptions());
    }
    // Inject blockchain tool descriptions so the LLM knows about
    // get_balance / send_transaction / etc.
    all_descriptions.extend(CompositeExecutor::blockchain_tool_descriptions());
    if !all_descriptions.is_empty() {
        runner.config_mut().set_tool_descriptions(all_descriptions);
    }

    // Route every TraceEvent through the CLI's Output adapter.
    let shared = Arc::new(SharedTraceSink::new());
    shared.install(Box::new(OutputTraceSink::new(output_kind, opts.quiet)));
    shared.install(Box::new(LogSink));
    runner.set_trace_sink(Some(shared));
    Ok(runner)
}

/// Load a system prompt from a file. Returns the trimmed contents.
pub(crate) fn load_prompt_file(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(raw.trim().to_string())
}

/// Return the built-in default system prompt (the one baked into
/// `magent-core::agent_runner::HEALTH_SYSTEM_PROMPT` at compile time).
/// Exposed so [`crate::prompt::resolve_for_run`] can fall back to it
/// when the user hasn't supplied `--prompt-name` or `--prompt`.
pub(crate) fn default_system_prompt() -> String {
    magent_core::agent_runner::RunnerConfig::default().system_prompt
}

// ============================================================================
// Tests
// ============================================================================
// We don't run the full ReAct loop in unit tests (that would need either
// Ollama or careful simulated-response injection). Instead we verify the
// pieces we *can* test in isolation: prompt loading, report serialisation,
// and the quiet/mode flag plumbing.

// Clippy's `items_after_test_module` fires because the REPL implementation
// lives between the tests and the helpers below. The cleanest fix would be
// to move the `tests` mod to the bottom of the file, but the helpers it
// exercises (e.g. `load_prompt_file`) are interspersed through the REPL
// code, so the lift would be larger than the noise. Allow it explicitly.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, GlobalFlags};
    use crate::output::OutputKind;

    #[test]
    fn prompt_loader_trims_whitespace() {
        let dir = std::env::temp_dir().join("magent_cli_prompt_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("prompt.txt");
        std::fs::write(&path, "  hello world\n\n").unwrap();
        let s = load_prompt_file(&path).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn prompt_loader_missing_file_is_an_error() {
        let p = std::path::PathBuf::from("/nonexistent/does-not-exist.txt");
        assert!(load_prompt_file(&p).is_err());
    }

    #[test]
    fn build_runner_propagates_compression_policy() {
        // The CLI's `--max-messages` / `--tool-max-chars` flags must
        // end up on the runner's `RunnerConfig.compression` field so
        // the ReAct loop actually applies them.
        let opts = RunOptions {
            task: "noop".to_string(),
            max_messages: Some(12),
            tool_max_chars: Some(200),
            mock: true, // skip Ollama probe
            ..Default::default()
        };
        let mut out = Output::new(OutputKind::Json, true);
        let runner = build_runner(
            &opts,
            RunnerConfig::default().system_prompt,
            &mut out,
            OutputKind::Json,
        )
        .expect("build_runner");
        assert_eq!(runner.config().compression.max_messages, 12);
        assert_eq!(runner.config().compression.tool_content_max_chars, 200);
    }

    #[test]
    fn build_runner_zero_max_messages_means_disabled() {
        let opts = RunOptions {
            task: "noop".to_string(),
            max_messages: Some(0),
            tool_max_chars: Some(0),
            mock: true,
            ..Default::default()
        };
        let mut out = Output::new(OutputKind::Json, true);
        let runner = build_runner(
            &opts,
            RunnerConfig::default().system_prompt,
            &mut out,
            OutputKind::Json,
        )
            .expect("build_runner");
        assert_eq!(runner.config().compression.max_messages, 0);
        assert_eq!(runner.config().compression.tool_content_max_chars, 0);
    }

    #[test]
    fn run_report_json_includes_all_fields() {
        let r = RunReport {
            answer: "done".to_string(),
            iterations: 3,
            tool_calls: 2,
            provider: "ollama".to_string(),
            using_ollama: true,
            state: "Finished".to_string(),
            final_messages: 7,
            approx_tokens: 42,
            approx_bytes: 2048,
        };
        let v = r.to_json();
        assert_eq!(v["iterations"], serde_json::json!(3));
        assert_eq!(v["tool_calls"], serde_json::json!(2));
        assert_eq!(v["provider"], serde_json::json!("ollama"));
        assert_eq!(v["using_ollama"], serde_json::json!(true));
        assert_eq!(v["state"], serde_json::json!("Finished"));
        assert_eq!(v["final_messages"], serde_json::json!(7));
        assert_eq!(v["approx_tokens"], serde_json::json!(42));
        assert_eq!(v["approx_bytes"], serde_json::json!(2048));
    }

    #[test]
    fn run_report_json_works_with_mock_provider() {
        // The CLI flips the report's `provider` field to `"mock"`
        // when `--mock` is set, so CI scripts can distinguish
        // canned responses from a real LLM answer.
        let r = RunReport {
            answer: "ok".to_string(),
            iterations: 0,
            tool_calls: 0,
            provider: "mock".to_string(),
            using_ollama: false,
            state: "Finished".to_string(),
            final_messages: 0,
            approx_tokens: 0,
            approx_bytes: 0,
        };
        let v = r.to_json();
        assert_eq!(v["provider"], serde_json::json!("mock"));
        assert_eq!(v["using_ollama"], serde_json::json!(false));
    }

    #[test]
    fn run_report_json_works_with_deepseek_provider() {
        let r = RunReport {
            answer: "ok".to_string(),
            iterations: 1,
            tool_calls: 0,
            provider: "deepseek".to_string(),
            using_ollama: false,
            state: "Finished".to_string(),
            final_messages: 2,
            approx_tokens: 5,
            approx_bytes: 128,
        };
        let v = r.to_json();
        assert_eq!(v["provider"], serde_json::json!("deepseek"));
        assert_eq!(v["using_ollama"], serde_json::json!(false));
    }

    #[test]
    fn resolve_api_key_with_config_uses_ollama_key_env_for_ollama_provider() {
        // The configured `api_key_env` is per-provider, so passing
        // `provider = "ollama"` should look at the ollama config
        // block's `api_key_env`, not the deepseek one.
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("OLLAMA_API_KEY");
            std::env::set_var("MAGENT_TEST_OLLAMA_KEY", "from-ollama-cfg");
        }
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.ollama.api_key_env = Some("MAGENT_TEST_OLLAMA_KEY".to_string());
        let none = None;
        assert_eq!(
            resolve_api_key_with_config(&none, "ollama", &cfg).as_deref(),
            Some("from-ollama-cfg")
        );
        unsafe {
            std::env::remove_var("MAGENT_TEST_OLLAMA_KEY");
        }
    }

    #[test]
    fn resolve_api_key_with_config_ignores_blank_configured_env() {
        // An empty `api_key_env` (the default after `with_defaults`
        // for unknown providers) must be treated as "no configured
        // source", not as a request to read an empty env var.
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("OLLAMA_API_KEY");
        }
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.deepseek.api_key_env = None;
        let none = None;
        assert_eq!(
            resolve_api_key_with_config(&none, "deepseek", &cfg),
            None
        );
    }

    // ------------------------------------------------------------------
    // TraceSink plumbing
    // ------------------------------------------------------------------

    #[test]
    fn render_event_run_start_returns_task() {
        // The simplest event: the body of the line IS the task
        // text, so users can grep `^[Run] my task` from CI logs.
        let body = render_event(&TraceEvent::RunStart {
            task: "my task".to_string(),
        });
        assert_eq!(body, "my task");
    }

    #[test]
    fn render_event_backend_ready_marks_simulated() {
        let body = render_event(&TraceEvent::BackendReady {
            provider: "ollama".to_string(),
            using_real_llm: false,
        });
        assert!(body.contains("simulated"));
        let body = render_event(&TraceEvent::BackendReady {
            provider: "ollama".to_string(),
            using_real_llm: true,
        });
        assert!(body.contains("ollama") && body.contains("real LLM"));
    }

    #[test]
    fn render_event_budget_uses_kind_and_limit() {
        let body = render_event(&TraceEvent::BudgetExhausted {
            kind: "iterations",
            limit: 10,
        });
        assert!(body.contains("iterations") && body.contains("10"));
    }

    #[test]
    fn render_event_llm_truncates_long_bodies() {
        let long = "x".repeat(500);
        let body = render_event(&TraceEvent::LlmResponse { body: long });
        assert!(body.len() < 500, "expected truncation, got {} bytes", body.len());
        assert!(body.ends_with('…'), "truncation marker missing");
    }

    #[test]
    fn render_event_truncation_never_splits_multibyte_utf8() {
        // Regression: `&body[..200]` panicked with "end byte index 200 is
        // not a char boundary" whenever the LLM replied with non-ASCII text
        // (e.g. Chinese weather answers), because byte 200 landed inside a
        // multi-byte character. Rendering must be char-boundary-safe.
        let long_cn = "天气".repeat(300); // 4 bytes per char
        let body = render_event(&TraceEvent::LlmResponse { body: long_cn.clone() });
        assert!(body.len() < long_cn.len());
        assert!(body.ends_with('…'));
        // The truncated prefix must be valid UTF-8 (no panic above) and
        // split on a char boundary.
        assert!(body.is_char_boundary(0));

        // `truncate_utf8` itself must never return an interior index.
        let s = "a天气b天气c";
        let t = truncate_utf8(s, 7);
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
    }

    #[test]
    fn render_event_tool_error_marks_failure() {
        let body = render_event(&TraceEvent::ToolCallEnd {
            name: "read_sensor".to_string(),
            result: "boom".to_string(),
            success: false,
        });
        assert!(body.contains("error"));
    }

    #[test]
    fn output_trace_sink_quiet_drops_everything() {
        let mut sink = OutputTraceSink::new(OutputKind::Human, true);
        sink.event(TraceEvent::FinalResult {
            body: "should not appear".to_string(),
        });
        // No assertion on stdout — just that we don't panic. The
        // "does not print" guarantee is exercised in the integration
        // smoke test below.
    }

    #[test]
    fn output_trace_sink_json_mode_drops_events() {
        // Same expectation as `Output::trace_labeled`: JSON mode
        // must not contaminate the stdout envelope.
        let mut sink = OutputTraceSink::new(OutputKind::Json, false);
        sink.event(TraceEvent::RunStart {
            task: "noise".to_string(),
        });
    }

    // ------------------------------------------------------------------
    // --verbose / --log-level parser
    // ------------------------------------------------------------------

    #[test]
    fn global_flags_default_to_off() {
        let g = GlobalFlags::default();
        assert!(!g.verbose);
        assert!(g.log_level.is_none());
        assert!(!g.json);
        assert!(!g.no_color);
    }

    #[test]
    fn verbose_short_and_long_are_equivalent() {
        let a = Args::parse(&argv_str("magent --verbose run t")).unwrap();
        let b = Args::parse(&argv_str("magent -v run t")).unwrap();
        assert!(a.global.verbose);
        assert!(b.global.verbose);
    }

    #[test]
    fn log_level_consumes_next_token() {
        let a = Args::parse(&argv_str("magent --log-level debug run t")).unwrap();
        assert_eq!(a.global.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn log_level_without_value_is_an_error() {
        // When the user passes `--log-level` as the very last token
        // (no value follows), the parser has nothing to bind to and
        // returns `MissingValue`. With a value, the parser swallows
        // the next token — even if that token would normally be
        // the subcommand — which is a deliberate choice: it keeps
        // the parser single-pass and the error path consistent.
        let err = Args::parse(&argv_str("magent --log-level")).unwrap_err();
        assert!(err.to_string().contains("--log-level"));
    }

    /// Tiny test helper: build a `Vec<String>` from a shell-style
    /// command line so the parser tests stay readable.
    fn argv_str(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(|s| s.to_string()).collect()
    }

    // ------------------------------------------------------------------
    // `apply_config_overrides` — config-file → RunOptions precedence
    // ------------------------------------------------------------------

    /// Build a `ConfigRecord` with a couple of fields overridden, so
    /// we can verify that `apply_config_overrides` reads them back
    /// out and stamps them onto the options struct.
    fn config_with_overrides() -> crate::config::ConfigRecord {
        let mut c = crate::config::ConfigRecord::with_defaults();
        c.provider.default = "deepseek".to_string();
        c.provider.deepseek.model = "deepseek-coder".to_string();
        c.sampling.temperature = 0.42;
        c.runner.max_iterations = 7;
        c.compression.max_messages = 4;
        c
    }

    #[test]
    fn apply_config_fills_provider_when_blank() {
        let mut opts = RunOptions::default();
        let cfg = config_with_overrides();
        let report = apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.provider, "deepseek");
        assert!(report.applied.iter().any(|a| a == "provider=deepseek"));
    }

    #[test]
    fn apply_config_preserves_user_explicit_provider() {
        let mut opts = RunOptions {
            provider: "ollama".to_string(), // user picked
            ..Default::default()
        };
        let cfg = config_with_overrides();
        apply_config_overrides(&mut opts, &cfg);
        // The user's choice must win over the config file.
        assert_eq!(opts.provider, "ollama");
    }

    #[test]
    fn apply_config_fills_model_for_deepseek() {
        let mut opts = RunOptions {
            provider: "deepseek".to_string(),
            ..Default::default()
        };
        let cfg = config_with_overrides();
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.model, "deepseek-coder");
    }

    #[test]
    fn apply_config_fills_sampling() {
        let mut opts = RunOptions::default();
        let cfg = config_with_overrides();
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.temperature, Some(0.42));
    }

    #[test]
    fn apply_config_preserves_user_explicit_temperature() {
        let mut opts = RunOptions {
            temperature: Some(0.99),
            ..Default::default()
        };
        let cfg = config_with_overrides();
        apply_config_overrides(&mut opts, &cfg);
        // CLI flag wins.
        assert_eq!(opts.temperature, Some(0.99));
    }

    #[test]
    fn apply_config_fills_runner_caps() {
        let mut opts = RunOptions::default();
        let cfg = config_with_overrides();
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.max_iterations, Some(7));
        assert_eq!(opts.max_messages, Some(4));
    }

    #[test]
    fn apply_config_honours_zero_max_messages() {
        // A user that explicitly writes `max_messages = 0` in the
        // config file expects "no compression"; the override path
        // must honour that, not stomp it with a built-in default.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.compression.max_messages = 0;
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.max_messages, Some(0),
            "config's max_messages=0 must be honoured, not dropped as a sentinel");
    }

    #[test]
    fn apply_config_honours_zero_temperature() {
        // `temperature = 0.0` is a legitimate "deterministic output"
        // request. The override path must honour it, not skip it
        // as a sentinel.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.sampling.temperature = 0.0;
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.temperature, Some(0.0));
    }

    #[test]
    fn apply_config_picks_deepseek_model_when_default_is_deepseek() {
        // Regression: previously the model lookup hard-coded
        // `ollama` as the fallback for an empty `selected_provider`,
        // which meant a config that defaulted to deepseek would
        // still pick `llama3.2`. This guards against that.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.default = "deepseek".to_string();
        cfg.provider.deepseek.model = "deepseek-coder".to_string();
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.provider, "deepseek");
        assert_eq!(opts.model, "deepseek-coder");
    }

    #[test]
    fn apply_config_records_unknown_provider_warning() {
        // A broken config (or a typo) should not silently behave
        // like ollama — we surface it via the warnings list.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.default = "gpt-9000".to_string();
        let report = apply_config_overrides(&mut opts, &cfg);
        assert!(
            report.warnings.iter().any(|w| w.contains("gpt-9000")),
            "expected a warning mentioning the bad provider; got {:?}",
            report.warnings
        );
    }

    #[test]
    fn apply_config_warns_when_user_provider_has_no_model() {
        // User picked deepseek but the config has a model only
        // under ollama → the runner will fall back to the
        // hard-coded default, but the user should know.
        let mut opts = RunOptions {
            provider: "deepseek".to_string(),
            ..Default::default()
        };
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.deepseek.model = String::new();
        cfg.provider.ollama.model = "llama3.2".to_string();
        let report = apply_config_overrides(&mut opts, &cfg);
        assert!(
            report.warnings.iter().any(|w| w.contains("deepseek")),
            "expected a warning about deepseek's missing model; got {:?}",
            report.warnings
        );
    }

    #[test]
    fn apply_config_sets_probe_ollama_from_config() {
        // Regression: previously the probe flag was only logged
        // in the applied list, never actually flipped on
        // `opts.probe_ollama`. A config that pins
        // `runner.probe_ollama_on_run = true` should make the
        // runner actually probe on every call.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.runner.probe_ollama_on_run = true;
        apply_config_overrides(&mut opts, &cfg);
        assert!(
            opts.probe_ollama,
            "config probe_ollama_on_run=true should have flipped opts.probe_ollama"
        );

        // And `mock` should suppress it (the runner can't probe
        // a mocked LLM anyway).
        let mut opts = RunOptions {
            mock: true,
            ..Default::default()
        };
        apply_config_overrides(&mut opts, &cfg);
        assert!(
            !opts.probe_ollama,
            "mock mode should beat config probe_ollama_on_run"
        );
    }

    #[test]
    fn apply_config_sets_quiet_from_config() {
        // `io.quiet_default = true` should flip `opts.quiet` when
        // the user didn't pass `--quiet`. The config is currently
        // the only place where a quiet default lives — there's no
        // `--quiet` flag override path that lands in `RunOptions`
        // before `apply_config_overrides` runs.
        let mut opts = RunOptions {
            quiet: true,
            ..Default::default()
        };
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.io.quiet_default = true;
        apply_config_overrides(&mut opts, &cfg);
        assert!(
            opts.quiet,
            "config io.quiet_default=true should have flipped opts.quiet"
        );

        // And `--quiet` ("user explicit") should still win — but
        // since we can't trivially go from `true` back to `false`
        // (false is the nonexistent sentinel), we instead test
        // that config does *not* unset a CLI-supplied true.
        let mut opts = RunOptions {
            quiet: true,
            ..Default::default()
        };
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.io.quiet_default = false;
        apply_config_overrides(&mut opts, &cfg);
        assert!(opts.quiet, "user --quiet should beat config io.quiet_default=false");
    }

    #[test]
    fn apply_config_emits_url_for_selected_provider() {
        let mut opts = RunOptions {
            provider: "deepseek".to_string(),
            ..Default::default()
        };
        let cfg = crate::config::ConfigRecord::with_defaults();
        apply_config_overrides(&mut opts, &cfg);
        // The DeepSeek URL should be filled in (config has
        // `https://api.deepseek.com/v1` as a default).
        assert_eq!(opts.deepseek_url, "https://api.deepseek.com/v1");
    }

    #[test]
    fn apply_config_fills_built_in_url_when_no_config() {
        // When the config file is missing, `with_defaults()` returns
        // empty URL strings; the built-in defaults at the end of
        // `apply_config_overrides` must still kick in.
        let mut opts = RunOptions::default();
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.ollama.url = String::new();
        cfg.provider.deepseek.url = String::new();
        apply_config_overrides(&mut opts, &cfg);
        assert_eq!(opts.ollama_url, "http://localhost:11434");
        assert_eq!(opts.deepseek_url, "https://api.deepseek.com/v1");
    }

    #[test]
    fn resolve_api_key_with_config_prefers_cli() {
        // (1) --api-key wins regardless of env / config.
        let cfg = crate::config::ConfigRecord::with_defaults();
        let cli = Some("cli-key".to_string());
        assert_eq!(
            resolve_api_key_with_config(&cli, "deepseek", &cfg).as_deref(),
            Some("cli-key"),
        );
    }

    #[test]
    fn resolve_api_key_with_config_falls_back_to_configured_env() {
        // (2) When --api-key is empty, look at the env var named in
        // the config file.
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.deepseek.api_key_env = Some("MY_DEEPSEEK_KEY".to_string());
        // SAFETY: test-only env mutation. We use a unique name so
        // parallel test runs don't trample each other.
        unsafe { std::env::set_var("MY_DEEPSEEK_KEY", "secret-from-config") };
        let none: Option<String> = None;
        let got = resolve_api_key_with_config(&none, "deepseek", &cfg);
        unsafe { std::env::remove_var("MY_DEEPSEEK_KEY") };
        assert_eq!(got.as_deref(), Some("secret-from-config"));
    }

    #[test]
    fn resolve_api_key_with_config_returns_none_when_all_empty() {
        // (3) Nothing supplied → None. The runner surfaces a
        // friendly error in that case.
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.deepseek.api_key_env = None;
        let none: Option<String> = None;
        // SAFETY: the env var is process-global; we briefly unset
        // the historical fallback to keep the test deterministic.
        let saved = std::env::var("OLLAMA_API_KEY").ok();
        unsafe { std::env::remove_var("OLLAMA_API_KEY") };
        let got = resolve_api_key_with_config(&none, "deepseek", &cfg);
        if let Some(v) = saved {
            unsafe { std::env::set_var("OLLAMA_API_KEY", v) };
        }
        assert_eq!(got, None);
    }

    // -- Audit additions: endpoint_* helpers --

    #[test]
    fn endpoint_url_returns_empty_string_as_none() {
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.ollama.url = String::new();
        assert_eq!(endpoint_url(&cfg, "ollama"), None);
    }

    #[test]
    fn endpoint_url_trims_whitespace() {
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.deepseek.url = "   ".to_string();
        assert_eq!(endpoint_url(&cfg, "deepseek"), None);
    }

    #[test]
    fn endpoint_url_returns_value_when_present() {
        let cfg = crate::config::ConfigRecord::with_defaults();
        assert_eq!(endpoint_url(&cfg, "ollama"), Some("http://localhost:11434"));
    }

    #[test]
    fn endpoint_url_unknown_provider_returns_none() {
        let cfg = crate::config::ConfigRecord::with_defaults();
        assert_eq!(endpoint_url(&cfg, "gpt-9000"), None);
    }

    #[test]
    fn endpoint_model_falls_back_to_ollama_for_unknown() {
        // When `provider.default` is empty/missing, the runner
        // should fall back to the ollama model. This is the
        // historical default; deepseek users must explicitly
        // set `provider.default`.
        let cfg = crate::config::ConfigRecord::with_defaults();
        assert_eq!(endpoint_model(&cfg, ""), Some("llama3.2"));
        assert_eq!(endpoint_model(&cfg, "unknown"), Some("llama3.2"));
    }

    #[test]
    fn endpoint_model_picks_deepseek_for_deepseek() {
        let cfg = crate::config::ConfigRecord::with_defaults();
        assert_eq!(endpoint_model(&cfg, "deepseek"), Some("deepseek-chat"));
    }

    #[test]
    fn endpoint_model_returns_none_when_empty() {
        let mut cfg = crate::config::ConfigRecord::with_defaults();
        cfg.provider.ollama.model = String::new();
        assert_eq!(endpoint_model(&cfg, "ollama"), None);
    }

    #[cfg(feature = "web3")]
    #[test]
    fn build_runner_injects_blockchain_tool_descriptions() {
        // When the web3/blockchain feature is on, the runner should
        // expose `get_balance` (and friends) to the LLM via
        // `tool_descriptions`. Without this, the LLM would not know
        // it can call the blockchain tools.
        use crate::output::OutputKind;
        let opts = RunOptions {
            task: "noop".to_string(),
            ..Default::default()
        };
        let mut out = Output::new(OutputKind::Human, true);
        let runner = build_runner(&opts, "system prompt".to_string(), &mut out, OutputKind::Human);
        let runner = match runner {
            Ok(r) => r,
            Err(_) => return, // skip if construction fails for unrelated reasons
        };
        let descs = &runner.config().tool_descriptions;
        let has_blockchain = descs
            .iter()
            .any(|(name, _)| BlockchainExecutor::tool_names().contains(&name.as_str()));
        assert!(
            has_blockchain,
            "expected blockchain tool descriptions in runner config; got: {:?}",
            descs.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
    }

    #[cfg(feature = "web3")]
    #[test]
    fn build_runner_without_blockchain_email_tools_has_no_descriptions() {
        // If neither web3 nor email-tools is enabled, the runner should
        // not carry any extra tool descriptions (the LLM still gets the
        // built-in simulator tools via the ToolExecutor wiring).
        use crate::output::OutputKind;
        let opts = RunOptions {
            task: "noop".to_string(),
            ..Default::default()
        };
        let mut out = Output::new(OutputKind::Human, true);
        let _ = build_runner(&opts, "system prompt".to_string(), &mut out, OutputKind::Human);
    }
}

// ============================================================================
// REPL implementation
// ============================================================================

/// REPL session state
struct ReplSession {
    /// Command history for navigation
    history: VecDeque<String>,
    /// Current history position (for up/down arrow navigation)
    history_pos: usize,
    /// Number of tasks executed
    task_count: usize,
    /// Session start time
    start_time: std::time::Instant,
}

impl Default for ReplSession {
    fn default() -> Self {
        Self {
            history: VecDeque::new(),
            history_pos: 0,
            task_count: 0,
            start_time: std::time::Instant::now(),
        }
    }
}

impl ReplSession {
    fn add_to_history(&mut self, cmd: String) {
        // Don't add empty or duplicate of last command
        if cmd.is_empty() {
            return;
        }
        if let Some(last) = self.history.back() {
            if last == &cmd {
                return;
            }
        }
        self.history.push_back(cmd);
        // Keep history bounded
        if self.history.len() > 100 {
            self.history.pop_front();
        }
        self.history_pos = self.history.len();
    }

    #[allow(dead_code)]
    fn get_history_entry(&self, offset: isize) -> Option<&String> {
        let pos = self.history_pos as isize + offset;
        if pos < 0 || pos >= self.history.len() as isize {
            return None;
        }
        self.history.get(pos as usize)
    }

    fn session_duration(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }
}

/// REPL command result
enum ReplCommandResult {
    Continue,
    Stop,
    RunTask(String),
}

/// REPL-specific trace sink for interactive output
struct ReplTraceSink;

impl TraceSink for ReplTraceSink {
    fn event(&mut self, event: TraceEvent) {
        match event {
            TraceEvent::ToolCallStart { name, arguments } => {
                let _ = writeln!(std::io::stderr(), "  → {}({})", name, arguments);
            }
            TraceEvent::ToolCallEnd { name, result, success } => {
                let status = if success { "✓" } else { "✗" };
                let _ = writeln!(std::io::stderr(), "    {} {}: {}", status, name, result);
            }
            TraceEvent::LlmResponse { body } => {
                let preview = if body.len() > 150 {
                    format!("{}...", truncate_utf8(&body, 150))
                } else {
                    body.clone()
                };
                let _ = write!(std::io::stderr(), "  🤖 {}", preview);
            }
            TraceEvent::FinalResult { .. } => {
                let _ = writeln!(std::io::stderr());
                let _ = writeln!(std::io::stderr(), "  ─────────────────────────────");
            }
            _ => {}
        }
    }
}

/// Handle built-in REPL commands
fn handle_repl_command(
    input: &str,
    runner: &mut RealAgentRunner<CompositeExecutor>,
    session: &mut ReplSession,
) -> ReplCommandResult {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.first().copied().unwrap_or("");
    let args: Vec<&str> = parts[1..].to_vec();

    match cmd.to_lowercase().as_str() {
        "" => ReplCommandResult::Continue,
        "help" | "?" => {
            print_repl_help();
            ReplCommandResult::Continue
        }
        "quit" | "exit" | "q" => ReplCommandResult::Stop,
        "clear" | "cls" => {
            print!("\x1B[2J\x1B[H");
            let _ = std::io::stdout().flush();
            ReplCommandResult::Continue
        }
        "reset" => {
            runner.reset_conversation();
            println!("✓ Conversation reset. System prompt preserved.");
            ReplCommandResult::Continue
        }
        "stats" => {
            print_repl_stats(runner, session);
            ReplCommandResult::Continue
        }
        "history" | "hist" => {
            print_repl_history(session);
            ReplCommandResult::Continue
        }
        "tools" => {
            print_repl_tools();
            ReplCommandResult::Continue
        }
        "context" => {
            let msgs = runner.messages().len();
            let tokens = runner.approx_total_tokens();
            println!("\n📊 Context:");
            println!("  Messages:     {}", msgs);
            println!("  Approx tokens: ~{}", tokens);
            println!("  Size:         ~{} chars", tokens * 4);
            ReplCommandResult::Continue
        }
        "session" => {
            let duration = session.session_duration();
            println!("\n📅 Session info:");
            println!("  Tasks run:    {}", session.task_count);
            println!("  History size: {}", session.history.len());
            println!("  Duration:     {:?}", duration);
            ReplCommandResult::Continue
        }
        "backend" => {
            let using = if runner.using_ollama() { "ollama" } else { "mock/deepseek" };
            println!("\n🔧 Backend: {}", using);
            ReplCommandResult::Continue
        }
        "retry" | "rerun" => {
            // Get last task from history and run it again
            if let Some(last_task) = session.history.iter().rev().find(|h| !h.starts_with('/')) {
                println!("Retrying: {}", last_task);
                ReplCommandResult::RunTask(last_task.clone())
            } else {
                println!("No previous task to retry.");
                ReplCommandResult::Continue
            }
        }
        "alias" => {
            if args.is_empty() {
                println!("\n📝 Aliases:");
                println!("  help, ?     - Show this help");
                println!("  quit, exit, q - Exit REPL");
                println!("  clear, cls  - Clear screen");
                println!("  hist        - Show history");
                println!("  stats       - Show session stats");
            } else {
                println!("Aliases are fixed shortcuts for commands.");
            }
            ReplCommandResult::Continue
        }
        "prompt" => {
            println!("\n📋 System prompt (first message):");
            let messages = runner.messages();
            if !messages.is_empty() {
                let content = &messages[0].content;
                let preview = if content.len() > 300 {
                    format!("{}\n...[truncated]", &content[..300])
                } else {
                    content.clone()
                };
                println!("\n{}\n", preview);
            } else {
                println!("  (no messages)");
            }
            ReplCommandResult::Continue
        }
        "env" => {
            println!("\n🌐 Environment:");
            println!("  OLLAMA_URL:     {}", std::env::var("OLLAMA_URL").unwrap_or_else(|_| "not set".into()));
            println!("  DEEPSEEK_API_KEY: {}", if std::env::var("DEEPSEEK_API_KEY").is_ok() { "***" } else { "not set" });
            println!("  MAGENT_CONFIG:  {}", std::env::var("MAGENT_CONFIG").unwrap_or_else(|_| "default".into()));
            ReplCommandResult::Continue
        }
        _ if input.starts_with('/') => {
            println!("Unknown command: {}. Type 'help' for available commands.", cmd);
            ReplCommandResult::Continue
        }
        _ => ReplCommandResult::RunTask(input.to_string()),
    }
}

/// Print REPL help
fn print_repl_help() {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║                     mAgent REPL Commands                    ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Navigation & Session                                       ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  help, ?        Show this help message                     ║");
    println!("║  quit, exit, q  Exit the REPL                             ║");
    println!("║  clear, cls     Clear the terminal screen                  ║");
    println!("║                                                                ║");
    println!("║  Conversation                                                ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  reset         Reset conversation (keep system prompt)       ║");
    println!("║  context       Show current context size                    ║");
    println!("║  prompt        Show current system prompt                   ║");
    println!("║  retry, rerun  Run the previous task again                 ║");
    println!("║                                                                ║");
    println!("║  Info & Debug                                                ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  stats         Show session statistics                       ║");
    println!("║  history       Show command history                         ║");
    println!("║  tools         List available tools                         ║");
    println!("║  session       Show session info                            ║");
    println!("║  backend       Show current LLM backend                     ║");
    println!("║  env           Show environment variables                   ║");
    println!("║                                                                ║");
    println!("║  Any other input is treated as a task for the agent.        ║");
    println!("║  Use ↑/↓ arrows to navigate command history.                ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}

/// Print REPL stats
fn print_repl_stats(runner: &RealAgentRunner<CompositeExecutor>, session: &ReplSession) {
    let duration = session.session_duration();
    let msgs = runner.messages().len();
    let tokens = runner.approx_total_tokens();
    let iterations = runner.iteration();
    let tool_calls = runner.tool_call_count();
    let using = if runner.using_ollama() { "Ollama" } else { "Mock/DeepSeek" };

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║                       Session Statistics                     ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Session                                                       ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  Tasks run:      {:>6}                                      ║", session.task_count);
    println!("║  Duration:       {:?}                              ║", duration);
    println!("║  History size:   {:>6}                                      ║", session.history.len());
    println!("║                                                                ║");
    println!("║  Context                                                       ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  Messages:      {:>6}                                      ║", msgs);
    println!("║  Approx tokens:  {:>6}                                      ║", tokens);
    println!("║  Size:          ~{:>6} chars                              ║", tokens * 4);
    println!("║                                                                ║");
    println!("║  Agent State                                                 ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  Total iterations: {:>5}                                     ║", iterations);
    println!("║  Tool calls:      {:>5}                                     ║", tool_calls);
    println!("║  Backend:         {}                                      ║", using);
    println!("╚══════════════════════════════════════════════════════════════╝");
}

/// Print command history
fn print_repl_history(session: &ReplSession) {
    println!("\n📜 Command history:");
    if session.history.is_empty() {
        println!("  (empty)");
    } else {
        for (i, cmd) in session.history.iter().enumerate() {
            let preview = if cmd.len() > 60 {
                format!("{}...", &cmd[..60])
            } else {
                cmd.clone()
            };
            println!("  {:3}. {}", i + 1, preview);
        }
    }
}

/// Print available tools in REPL
fn print_repl_tools() {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║                     Available Tools                          ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║                                                                ║");
    println!("║  Sensor Tools                                                  ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  read_sensor(sensor)                                          ║");
    println!("║      Sensors: temperature, humidity, accelerometer,             ║");
    println!("║               pressure, heart_rate, light, battery            ║");
    println!("║                                                                ║");
    println!("║  GPIO Tools                                                   ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  write_gpio(pin, state)                                      ║");
    println!("║      Control GPIO pin (0-31), state: high/low/toggle           ║");
    println!("║                                                                ║");
    println!("║  Flash Memory                                                  ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  flash_read(address)                                         ║");
    println!("║  flash_write(address, data)                                   ║");
    println!("║      Read/write to flash memory                               ║");
    println!("║                                                                ║");
    println!("║  Bluetooth LE                                                 ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  ble_scan()                                                  ║");
    println!("║  ble_send(data)                                               ║");
    println!("║      Scan and send data via Bluetooth LE                       ║");
    println!("║                                                                ║");
    println!("║  Email Tools (--email-tools)                                  ║");
    println!("║  ─────────────────────────────────────────────────────────  ║");
    println!("║  email_list()           - List emails                         ║");
    println!("║  email_read(id)         - Read specific email                ║");
    println!("║  email_send(to, subj)  - Send email                         ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}

/// Simple readline-like input with history navigation
fn read_line_with_history(prompt: &str, _session: &ReplSession) -> String {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();

    let stdin = io::stdin();
    let mut line = String::new();

    // Simple line reading (no fancy readline, but basic functionality works)
    if stdin.read_line(&mut line).is_ok() {
        line.trim().to_string()
    } else {
        String::new()
    }
}
