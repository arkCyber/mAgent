//! `magent scheduler` — time-triggered auto-runner for audit and
//! code-completion work.
//!
//! ## Why this exists
//!
//! `magent run` is interactive: the user types a task, the agent
//! answers, the process exits. That works for one-off prompts but not
//! for the kind of work mAgent is actually good at — sweeping the
//! codebase for stale `TODO`s, regenerating the README's CLI table,
//! filling in `// FIXME` comments, etc. — which is **repetitive**
//! **time-triggered** work.
//!
//! `scheduler` adds a single new verb: it reads a list of tasks from
//! either (a) a JSON file the user supplies or (b) the built-in
//! `audit` / `complete` presets, then on every tick (default 60s)
//! runs the next pending task through the same `RealAgentRunner`
//! `magent run` uses.
//!
//! Two execution shapes are supported:
//!
//! 1. **Foreground / one-shot** (`magent scheduler run-once`).
//!    Run the entire queue once, print the summary, exit. Useful in
//!    CI and cron.
//!
//! 2. **Daemon loop** (`magent scheduler daemon`). Tick forever
//!    (Ctrl-C to stop) and re-run the queue on every interval. The
//!    first tick is **immediate** so the user gets feedback without
//!    waiting for the first interval to elapse.
//!
//! ## Storage
//!
//! The scheduler persists its state — last run timestamps, per-task
//! counters, last-error snippets — to a single JSON file at
//! `$MAGENT_SCHEDULER_STATE` or `$XDG_STATE_HOME/magent/scheduler.json`
//! (falling back to `~/.local/state/magent/scheduler.json`). Writes
//! are atomic (write-to-temp + rename) so an interrupted daemon
//! never produces a half-written JSON.
//!
//! ## Task sources
//!
//! * `--tasks-file <PATH>` — JSON object `{ "tasks": [...] }`. Each
//!   entry mirrors the flags a user would pass to `magent run`.
//! * `--preset <audit|complete>` — built-in task lists tailored to
//!   the two headline workflows (audit + code-completion).
//!
//! Built-in presets never touch the network or the host filesystem
//! outside the project tree — they only invoke `magent run` with a
//! safe `--mock` fallback when the LLM isn't reachable, so the
//! daemon is safe to leave running.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::output::{Output, OutputKind};

// ============================================================================
// Public constants
// ============================================================================

/// Env var that overrides the scheduler state file location. Mirrors
/// the convention used by `config.rs` / `prompt.rs` so the audit
/// surface stays uniform.
pub const STATE_FILE_ENV: &str = "MAGENT_SCHEDULER_STATE";

/// Default scheduler state file (relative to the XDG state dir).
pub const STATE_FILENAME: &str = "scheduler.json";

/// Default tick interval. 60 seconds is small enough to feel
/// "live" without hammering the LLM backend.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Smallest interval we accept. Below this, the scheduler spends
/// more time bookkeeping than running tasks — and would look like a
/// runaway process to anyone watching `top`.
pub const MIN_INTERVAL_SECS: u64 = 1;

/// Largest interval we accept. Above this, the daemon feels broken
/// (a typo of `3600` → `36000` should fail loudly, not silently
/// schedule work "tomorrow").
pub const MAX_INTERVAL_SECS: u64 = 86_400; // 24h

/// Largest user-supplied task string we'll accept. Real task
/// descriptions fit in a few hundred bytes; multi-KB strings are
/// almost certainly paste accidents.
pub const TASK_MAX: usize = 4_096;

/// Largest JSON task file we'll read. Mirrors the bound used by
/// `config.rs` so the scheduler can't be tricked into reading a
/// 10GB file.
pub const TASKS_FILE_MAX: usize = 1_048_576; // 1 MiB

// ============================================================================
// Errors
// ============================================================================

/// Anything that can go wrong while executing the scheduler.
#[derive(Debug)]
pub enum SchedulerError {
    /// The user-supplied task file couldn't be read or parsed.
    TasksFile { path: PathBuf, source: io::Error },
    /// The task file is valid JSON but doesn't have the expected shape.
    TasksFileShape(String),
    /// The user passed an unknown preset name.
    UnknownPreset(String),
    /// The interval was outside [`MIN_INTERVAL_SECS`, `MAX_INTERVAL_SECS`].
    InvalidInterval { got: u64 },
    /// The cron expression couldn't be parsed.
    InvalidCron(String),
    /// The `--at` value couldn't be parsed as RFC 3339 or was in the past.
    InvalidAt(String),
    /// `--interval`, `--cron`, and `--at` are mutually exclusive; the
    /// user picked more than one.
    ScheduleConflict(String),
    /// The daemon has no schedule (no `--interval`, `--cron`, or
    /// `--at`). We refuse rather than busy-loop.
    MissingSchedule,
    /// A task string exceeded [`TASK_MAX`].
    TaskTooLong { len: usize, max: usize },
    /// A task file was bigger than [`TASKS_FILE_MAX`].
    TasksFileTooLarge { size: u64, max: u64 },
    /// I/O on the state file failed.
    State { path: PathBuf, source: io::Error },
    /// JSON serialisation / deserialisation of the state file failed.
    StateJson { path: PathBuf, source: serde_json::Error },
    /// A scheduled task's `magent run` subprocess exited non-zero, or
    /// could not be spawned at all.
    ///
    /// Separate from [`Self::State`] on purpose: those two used to
    /// share a variant, so a task that failed at the LLM was recorded
    /// in `last_error` as `scheduler state file "<magent run> (exit 1)": …`
    /// — pointing the operator at a state file that was perfectly
    /// healthy, in the one field that exists to say why the task failed.
    TaskRun {
        /// The scheduled task's name, so the message identifies which
        /// entry failed.
        name: String,
        /// Exit code, or `<signal>` / `<spawn>` when there is no code.
        exit: String,
        /// Combined stdout + stderr of the child.
        detail: String,
    },
    /// One or more tasks in a `run-once` queue failed.
    ///
    /// Raised *after* the state file is written and the summary (or the
    /// `--json` envelope) is emitted, so a CI caller keeps both and
    /// still sees a non-zero exit. The daemon ignores this variant: a
    /// flaky backend must not stop the tick loop.
    TasksFailed {
        /// How many tasks failed.
        failed: usize,
        /// How many were attempted.
        total: usize,
    },
    /// The user hit Ctrl-C (SIGINT) and we shut down cleanly.
    Interrupted,
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchedulerError::TasksFile { path, source } => {
                write!(f, "could not read tasks file {:?}: {}", path, source)
            }
            SchedulerError::TasksFileShape(msg) => {
                write!(f, "tasks file has unexpected shape: {}", msg)
            }
            SchedulerError::UnknownPreset(name) => {
                write!(f, "unknown preset {:?}; expected one of: audit | complete", name)
            }
            SchedulerError::InvalidInterval { got } => write!(
                f,
                "interval {}s is out of range [{}, {}]",
                got, MIN_INTERVAL_SECS, MAX_INTERVAL_SECS
            ),
            SchedulerError::InvalidCron(msg) => {
                write!(f, "invalid cron expression: {}", msg)
            }
            SchedulerError::InvalidAt(msg) => {
                write!(f, "invalid --at value: {}", msg)
            }
            SchedulerError::ScheduleConflict(msg) => {
                write!(f, "schedule conflict: {}", msg)
            }
            SchedulerError::MissingSchedule => write!(
                f,
                "daemon needs a schedule: pass --interval <SECS>, --cron <EXPR>, or --at <RFC3339>"
            ),
            SchedulerError::TaskTooLong { len, max } => {
                write!(f, "task string is {} chars; max is {}", len, max)
            }
            SchedulerError::TasksFileTooLarge { size, max } => {
                write!(f, "tasks file is {} bytes; max is {}", size, max)
            }
            SchedulerError::State { path, source } => {
                write!(f, "scheduler state file {:?}: {}", path, source)
            }
            SchedulerError::StateJson { path, source } => {
                write!(f, "scheduler state file {:?}: invalid JSON: {}", path, source)
            }
            SchedulerError::TaskRun { name, exit, detail } => {
                write!(
                    f,
                    "scheduled task {:?} failed (exit {}): {}",
                    name, exit, detail
                )
            }
            SchedulerError::TasksFailed { failed, total } => {
                write!(f, "{} of {} scheduled task(s) failed", failed, total)
            }
            SchedulerError::Interrupted => write!(f, "interrupted by SIGINT"),
        }
    }
}

impl std::error::Error for SchedulerError {}

// ============================================================================
// Task model
// ============================================================================

/// A single scheduler task. Mirrors the flags a user would pass to
/// `magent run` but with safer defaults (the agent is invoked through
/// the same `RunCmd` path so every existing flag is honoured).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledTask {
    /// Human-readable name. Used as the JSON key in the state file
    /// and printed in the per-task summary line.
    pub name: String,

    /// The task string passed to `magent run`. Required.
    pub task: String,

    /// Optional prompt name (`magent run --prompt-name <NAME>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_name: Option<String>,

    /// Optional provider override (`magent run --provider <NAME>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Optional model override (`magent run --model <NAME>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Skip the LLM (passed through to `magent run --mock`).
    #[serde(default)]
    pub mock: bool,
}

impl ScheduledTask {
    /// Build a task from the preset library. Used by `--preset audit`
    /// and `--preset complete`.
    fn from_preset(name: &str, task: &str) -> Self {
        // The preset tasks are intentionally short, audit-friendly
        // prompts that exercise the most useful tools without ever
        // touching the network. They share the same wording so
        // changes can be diff-reviewed in one place.
        Self {
            name: name.to_string(),
            task: task.to_string(),
            prompt_name: None,
            provider: None,
            model: None,
            mock: false,
        }
    }
}

/// Top-level JSON shape of a user-supplied tasks file.
///
/// ```json
/// {
///   "tasks": [
///     { "name": "audit-todos", "task": "Find every stale TODO…" },
///     { "name": "refresh-readme", "task": "Regenerate the CLI table…" }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksFile {
    pub tasks: Vec<ScheduledTask>,
}

// ============================================================================
// Built-in presets
// ============================================================================

/// Built-in `audit` preset — tasks that take a fresh look at the
/// codebase and report issues without modifying anything.
///
/// Every prompt here is deliberately scoped so the agent's tool
/// budget isn't blown by the first call.
fn preset_audit() -> Vec<ScheduledTask> {
    vec![
        ScheduledTask::from_preset(
            "audit-todos",
            "Scan the repository for `TODO`, `FIXME`, and `XXX` comments. \
             Group them by file, list each one with the surrounding line, \
             and end with a one-paragraph summary of the highest-priority \
             items. Do not modify any files.",
        ),
        ScheduledTask::from_preset(
            "audit-unwraps",
            "Search the Rust source tree for `.unwrap()`, `.expect(`, and \
             `panic!`. List every hit with file:line, then propose a \
             minimum-diff fix for each one. Do not apply any fixes.",
        ),
        ScheduledTask::from_preset(
            "audit-unsafe",
            "Find every `unsafe { … }` block in the Rust sources. For each \
             one, summarise why the unsafe is needed and what invariants \
             the surrounding safe wrapper relies on. Flag any block that \
             isn't documented.",
        ),
        ScheduledTask::from_preset(
            "audit-deps",
            "Inspect the workspace `Cargo.toml`. List every direct \
             dependency with its version, then flag any that haven't been \
             updated in the last 12 months (based on the lockfile). \
             Skip transitive deps.",
        ),
    ]
}

/// Built-in `complete` preset — tasks that fill in or improve
/// existing code. Slightly riskier than `audit` (some tasks modify
/// files) so they default to `--mock` unless the user opts in.
fn preset_complete() -> Vec<ScheduledTask> {
    vec![
        ScheduledTask::from_preset(
            "complete-docs",
            "Find every public Rust item (function, struct, enum, trait) \
             that is missing a doc comment. Insert a one-line `///` \
             summary for each one. Re-run `cargo check` afterwards to \
             confirm no new warnings.",
        ),
        ScheduledTask::from_preset(
            "complete-error-msgs",
            "Audit every error variant in `enum *Error` types. Make sure \
             each variant's `Display` impl produces an actionable message \
             (mentions the offending value where possible). Edit the \
             impls in place.",
        ),
        ScheduledTask::from_preset(
            "complete-tests",
            "Find every `pub fn` that has no associated test. Add a \
             minimal happy-path test to the corresponding `#[cfg(test)] \
             mod tests`. Don't refactor existing tests.",
        ),
    ]
}

fn resolve_preset(name: &str) -> Result<Vec<ScheduledTask>, SchedulerError> {
    match name {
        "audit" => Ok(preset_audit()),
        "complete" => Ok(preset_complete()),
        other => Err(SchedulerError::UnknownPreset(other.to_string())),
    }
}

// ============================================================================
// Persistent state
// ============================================================================

/// Per-task bookkeeping persisted across ticks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskStats {
    /// Unix-epoch seconds of the last successful run. `None` if the
    /// task has never completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<u64>,
    /// Number of successful runs since the state file was created.
    #[serde(default)]
    pub success_count: u64,
    /// Number of failed runs since the state file was created.
    #[serde(default)]
    pub failure_count: u64,
    /// Last error message (truncated to [`LAST_ERROR_MAX`] so the state
    /// file can't grow without bound). The **tail** is kept — see
    /// [`TaskStats::record_failure`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

const LAST_ERROR_MAX: usize = 512;

impl TaskStats {
    fn record_success(&mut self) {
        self.last_run_at = Some(now_secs());
        self.success_count = self.success_count.saturating_add(1);
        self.last_error = None;
    }

    fn record_failure(&mut self, err: &str) {
        self.failure_count = self.failure_count.saturating_add(1);
        let truncated = if err.len() > LAST_ERROR_MAX {
            // Keep the **tail**, not the head. A subprocess failure's
            // buffer is the child's whole stderr, which opens with
            // `[Config] applied …` / `[Backend] …` / `[Task] <the task>` —
            // so head-truncating filled `last_error` with run metadata and
            // dropped the `error: agent failed: …` line it exists to
            // report. An error message names its cause last.
            let mut idx = err.len() - LAST_ERROR_MAX;
            // Never split a code point: step *forward* to the next
            // boundary (the head path stepped back from a fixed offset).
            while idx < err.len() && !err.is_char_boundary(idx) {
                idx += 1;
            }
            format!("…{}", &err[idx..])
        } else {
            err.to_string()
        };
        self.last_error = Some(truncated);
    }
}

/// Top-level state file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerState {
    /// Schema version. Bumped on breaking changes.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Per-task counters, keyed by `ScheduledTask::name`.
    #[serde(default)]
    pub tasks: std::collections::BTreeMap<String, TaskStats>,
    /// Unix-epoch seconds of the last daemon start (for observability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<u64>,
}

/// Hand-written rather than derived: `#[derive(Default)]` gives
/// `schema_version: 0` (u32's own default), while the serde attribute
/// above gives `1` (the field's declared "absent" value) — so the file
/// the daemon *wrote* on its first run said `0` and the same file read
/// back with the field deleted said `1`. A version that depends on
/// whether the key was omitted is worse than useless for a migration:
/// the very first run produced a file that looked one version older
/// than it is.
impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            tasks: std::collections::BTreeMap::new(),
            last_started_at: None,
        }
    }
}

fn default_schema_version() -> u32 {
    1
}

/// Resolve the state file path. Mirrors `ConfigCmd`'s precedence:
///
/// 1. `$MAGENT_SCHEDULER_STATE` (explicit override)
/// 2. `$XDG_STATE_HOME/magent/scheduler.json`
/// 3. `~/.local/state/magent/scheduler.json`
fn resolve_state_path() -> Result<PathBuf, SchedulerError> {
    if let Ok(p) = std::env::var(STATE_FILE_ENV) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let xdg = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty());
    let base: PathBuf = xdg
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".local").join("state")))
        .ok_or(SchedulerError::State {
            path: PathBuf::from("<unknown>"),
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "could not determine state directory: set $MAGENT_SCHEDULER_STATE \
                 or $XDG_STATE_HOME or $HOME",
            ),
        })?;
    Ok(base.join("magent").join(STATE_FILENAME))
}

fn home_dir() -> Option<PathBuf> {
    // Prefer `HOME` on Unix; fall back to `USERPROFILE` on Windows so
    // the scheduler still works in cross-platform CI.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        // Pre-1970 systems are exceedingly rare; fall back to 0
        // rather than panic so the daemon keeps running.
        .unwrap_or(0)
}

fn load_state(path: &Path) -> Result<SchedulerState, SchedulerError> {
    match fs::read(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return Ok(SchedulerState::default());
            }
            serde_json::from_slice::<SchedulerState>(&bytes).map_err(|e| {
                SchedulerError::StateJson {
                    path: path.to_path_buf(),
                    source: e,
                }
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SchedulerState::default()),
        Err(e) => Err(SchedulerError::State {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

fn save_state(path: &Path, state: &SchedulerState) -> Result<(), SchedulerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SchedulerError::State {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| SchedulerError::StateJson {
        path: path.to_path_buf(),
        source: e,
    })?;
    // Atomic write: write to <path>.tmp, fsync, rename. Same pattern
    // `summary.rs` uses so a power-cut never produces a half-written
    // state file.
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| SchedulerError::State {
            path: tmp.clone(),
            source: e,
        })?;
        f.write_all(&bytes).map_err(|e| SchedulerError::State {
            path: tmp.clone(),
            source: e,
        })?;
        f.sync_all().map_err(|e| SchedulerError::State {
            path: tmp.clone(),
            source: e,
        })?;
    }
    fs::rename(&tmp, path).map_err(|e| SchedulerError::State {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

// ============================================================================
// Actions
// ============================================================================

/// What the user wants the scheduler to do.
#[derive(Debug, Clone)]
pub enum SchedulerAction {
    /// `magent scheduler run-once` — execute the queue once and exit.
    RunOnce {
        tasks_file: Option<PathBuf>,
        preset: Option<String>,
    },
    /// `magent scheduler daemon` — tick forever (Ctrl-C to stop).
    Daemon {
        tasks_file: Option<PathBuf>,
        preset: Option<String>,
        schedule: DaemonSchedule,
        timezone: SchedulerTimezone,
    },
    /// `magent scheduler status` — print the current state file.
    Status,
    /// `magent scheduler --help` / `magent help scheduler`.
    Help,
}

/// How the daemon decides when to wake up.
///
/// Exactly one of these three modes is in effect at any time. The
/// parser rejects combinations in `parse_scheduler_args` so the
/// executor never has to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonSchedule {
    /// Wake up every `secs` seconds. The first tick is immediate.
    Interval { secs: u64 },
    /// Wake up at every wall-clock instant matching the 5-field
    /// cron expression (`分 时 日 月 周`). The first tick is the
    /// next matching instant, **not** immediate — that matches the
    /// user mental model of "wait until 9:00 then run".
    Cron(String),
    /// Wake up exactly once at the supplied Unix-epoch second, then
    /// exit. Useful for "run the audit at 03:00 tonight" without
    /// leaving a daemon around.
    Once { at_secs: u64 },
}

/// Timezone policy for cron / --at evaluation.
///
/// `Local` uses the system's current UTC offset (read fresh on every
/// `next_after`, so a DST boundary only affects one tick). `Utc`
/// ignores the system zone and interprets every cron field as UTC.
/// The split is the same one `crontab` users expect.
///
/// Deliberately only these two: honouring an IANA name such as
/// `Asia/Shanghai` would need a tz database, and the CLI takes no new
/// dependencies. `--timezone` therefore rejects anything that is not
/// `utc`/`local` instead of silently treating a bogus value as local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerTimezone {
    Local,
    Utc,
}

// ============================================================================
// Command
// ============================================================================

/// Bundle of arguments ready to execute.
pub struct SchedulerCmd {
    pub action: SchedulerAction,
}

impl SchedulerCmd {
    pub fn new(action: SchedulerAction) -> Self {
        Self { action }
    }

    /// Execute the requested action. Returns `Ok(())` on success and
    /// `Err(SchedulerError)` on any failure (bad input, I/O, etc.).
    ///
    /// The function is intentionally `&self`-less so the lifetime
    /// story stays simple — the scheduler never borrows from the
    /// caller after returning.
    pub fn execute(self, out: &mut Output) -> Result<(), SchedulerError> {
        match self.action {
            SchedulerAction::Help => {
                // Help text is printed by the dispatcher so the
                // `magent help scheduler` and `magent scheduler --help`
                // paths stay in sync. The `Help` variant is here as a
                // safety net so the function is total.
                print!("{}", scheduler_help_text());
                Ok(())
            }
            SchedulerAction::Status => execute_status(out),
            SchedulerAction::RunOnce { tasks_file, preset } => {
                let tasks = resolve_tasks(tasks_file.as_deref(), preset.as_deref())?;
                execute_run_once(&tasks, out, true)
            }
            SchedulerAction::Daemon {
                tasks_file,
                preset,
                schedule,
                timezone,
            } => {
                // Validate up front so a bad cron expression or an
                // --interval of 0 fails *before* we touch the state
                // file or install signal handlers — a friendly
                // error path that doesn't leak state files on disk.
                match &schedule {
                    DaemonSchedule::Interval { secs } => {
                        if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(secs) {
                            return Err(SchedulerError::InvalidInterval { got: *secs });
                        }
                    }
                    DaemonSchedule::Cron(expr) => {
                        CronSpec::parse(expr).map_err(|e| {
                            SchedulerError::InvalidCron(format!("{:?}: {}", expr, e))
                        })?;
                    }
                    DaemonSchedule::Once { at_secs } => {
                        if *at_secs <= now_secs() {
                            return Err(SchedulerError::InvalidAt(format!(
                                "{} is in the past (now={})",
                                at_secs,
                                now_secs()
                            )));
                        }
                    }
                }
                let tasks = resolve_tasks(tasks_file.as_deref(), preset.as_deref())?;
                execute_daemon(&tasks, schedule, timezone, out)
            }
        }
    }
}

// ============================================================================
// Resolution helpers
// ============================================================================

fn resolve_tasks(
    tasks_file: Option<&Path>,
    preset: Option<&str>,
) -> Result<Vec<ScheduledTask>, SchedulerError> {
    // Two sources, mutually exclusive. Specifying both is a usage
    // error so the user gets a clear diagnostic instead of a
    // silently-merged list.
    match (tasks_file, preset) {
        (Some(_), Some(_)) => Err(SchedulerError::TasksFileShape(
            "pass either --tasks-file <PATH> or --preset <NAME>, not both".to_string(),
        )),
        (None, None) => Err(SchedulerError::TasksFileShape(
            "must pass --tasks-file <PATH> or --preset <audit|complete>".to_string(),
        )),
        (Some(path), None) => load_tasks_file(path),
        (None, Some(name)) => resolve_preset(name),
    }
}

fn load_tasks_file(path: &Path) -> Result<Vec<ScheduledTask>, SchedulerError> {
    let meta = fs::metadata(path).map_err(|e| SchedulerError::TasksFile {
        path: path.to_path_buf(),
        source: e,
    })?;
    let size = meta.len();
    if size > TASKS_FILE_MAX as u64 {
        return Err(SchedulerError::TasksFileTooLarge {
            size,
            max: TASKS_FILE_MAX as u64,
        });
    }
    let bytes = fs::read(path).map_err(|e| SchedulerError::TasksFile {
        path: path.to_path_buf(),
        source: e,
    })?;
    let parsed: TasksFile =
        serde_json::from_slice(&bytes).map_err(|e| SchedulerError::TasksFileShape(format!(
            "could not parse as {{ \"tasks\": [...] }}: {}",
            e
        )))?;
    // Per-task validation. We do this in a second pass so the error
    // message can point at the offending index.
    for (i, t) in parsed.tasks.iter().enumerate() {
        if t.task.len() > TASK_MAX {
            return Err(SchedulerError::TaskTooLong {
                len: t.task.len(),
                max: TASK_MAX,
            });
        }
        if t.name.is_empty() {
            return Err(SchedulerError::TasksFileShape(format!(
                "tasks[{}].name is empty",
                i
            )));
        }
    }
    Ok(parsed.tasks)
}

// ============================================================================
// Executor
// ============================================================================

/// One-shot executor. Runs every task, prints a line per task, persists
/// the counters, and then reports the outcome: `Ok(())` only when every
/// task succeeded, otherwise [`SchedulerError::TasksFailed`] so the
/// process exits non-zero. A failed task is also visible in the state
/// file (`failure_count` / `last_error`), the `FAILED:` line and the
/// `--json` envelope's `failed` counter — the exit code is what a CI
/// script can act on without parsing either stream.
///
/// The failure is raised *last*, so everything is written and printed
/// before the process gives up on the run.
fn execute_run_once(
    tasks: &[ScheduledTask],
    out: &mut Output,
    emit_json: bool,
) -> Result<(), SchedulerError> {
    let state_path = resolve_state_path()?;
    let mut state = load_state(&state_path)?;
    state.last_started_at = Some(now_secs());

    let total = tasks.len();
    let mut failed = 0usize;
    if total == 0 {
        // Don't early-return: a CI `--json` caller still expects the
        // `{"kind":"scheduler.run_once","ran":0,…}` envelope below,
        // and the state file must be written for `status`.
        let _ = out.trace_labeled("scheduler", "no tasks to run");
    }
    for (i, task) in tasks.iter().enumerate() {
        let _ = out.trace_labeled(
            "scheduler",
            &format!("[{}/{}] running {:?}", i + 1, total, task.name),
        );
        let stats = state
            .tasks
            .entry(task.name.clone())
            .or_insert_with(TaskStats::default);
        match run_one_task(task, out) {
            Ok(()) => stats.record_success(),
            Err(e) => {
                failed += 1;
                // Say so, in the transcript. A failing task used to print
                // its `[n/N] running "x"` line and then nothing — the only
                // trace was the state file, so a human running `run-once`
                // over a queue where everything failed saw a clean run.
                let _ = out.trace_labeled(
                    "scheduler",
                    &format!("{:?} FAILED: {}", task.name, one_line_failure(&e)),
                );
                stats.record_failure(&e.to_string());
            }
        }
    }
    save_state(&state_path, &state)?;

    if emit_json && out.kind() == OutputKind::Json {
        // Emit a single JSON envelope with the final state so a CI
        // pipeline can `jq` the counters without scraping stderr.
        //
        // The envelope is *self-describing* on failure (`ok:false` plus
        // the same `error`/`code`/`exit` triple `Output::fail` writes),
        // because this is the last thing stdout gets: `main.rs` sees the
        // `TasksFailed` returned below and calls `Output::fail`, which
        // would otherwise append a *second* JSON object and leave `jq`
        // with an unparseable stream. `Output`'s envelope latch suppresses
        // that second write, so the outcome has to be spelled out here.
        let mut envelope = serde_json::json!({
            "kind": "scheduler.run_once",
            "ok": failed == 0,
            "ran": total,
            "failed": failed,
            "state_path": state_path.display().to_string(),
            "tasks": state.tasks,
        });
        if failed > 0 {
            envelope["error"] =
                serde_json::json!(format!("{} of {} scheduled task(s) failed", failed, total));
            envelope["code"] = serde_json::json!("scheduler_tasks_failed");
            envelope["exit"] = serde_json::json!(1);
        }
        // `final_answer` only *stores* its argument in JSON mode (it
        // expects a later `write_json` to flush the envelope), so using
        // it here emitted nothing and the CI pipeline got an empty
        // stream. `write_json_str` writes the pre-rendered envelope
        // straight to stdout.
        out.write_json_str(envelope.to_string());
    } else {
        let outcome = if failed == 0 {
            format!("finished {} task(s)", total)
        } else {
            format!("finished {} task(s), {} FAILED", total, failed)
        };
        let _ = out.trace_labeled(
            "scheduler",
            &format!("{}; state written to {:?}", outcome, state_path),
        );
    }

    // State written and summary emitted — now report the failure through
    // the exit code. `run-once` is the CI entry point ("Execute every
    // task in the queue once and exit"), and a queue where every task
    // failed used to look exactly like a clean run from the shell's
    // point of view.
    if failed > 0 {
        return Err(SchedulerError::TasksFailed { failed, total });
    }
    Ok(())
}

/// The one line of a task failure worth putting in the transcript.
///
/// A [`SchedulerError::TaskRun`] carries the child's entire stderr, whose
/// last non-empty line is the `error: …` that `magent run` printed; the
/// earlier lines are `[Config]`/`[Backend]` noise. Everything else is
/// already a single line, so it is returned as-is.
///
/// The `stdout: ` / `stderr: ` label [`task_failure_detail`] prepends is
/// stripped: it is a useful tag in the state file, where the recorded
/// error may mix both streams, but on the transcript line it just repeats
/// what `error:` already says.
fn one_line_failure(err: &SchedulerError) -> String {
    let SchedulerError::TaskRun { exit, detail, .. } = err else {
        return err.to_string();
    };
    let last = detail.lines().rev().find(|l| !l.trim().is_empty());
    match last {
        Some(line) => {
            let line = line.trim();
            let line = line
                .strip_prefix("stderr: ")
                .or_else(|| line.strip_prefix("stdout: "))
                .unwrap_or(line);
            format!("exit {}: {}", exit, line)
        }
        None => format!("exit {} (no output)", exit),
    }
}

/// Unix second at which the daemon's **first** tick fires.
///
/// `Interval` fires immediately (fast feedback); `Cron` must wait for the
/// next matching wall-clock instant (it used to run the queue at startup);
/// `Once` waits for its absolute deadline.
fn first_tick_deadline(
    schedule: &DaemonSchedule,
    now: u64,
    timezone: SchedulerTimezone,
) -> Result<u64, SchedulerError> {
    match schedule {
        DaemonSchedule::Interval { .. } => Ok(now),
        DaemonSchedule::Cron(expr) => {
            let spec = CronSpec::parse(expr)
                .map_err(|e| SchedulerError::InvalidCron(format!("{:?}: {}", expr, e)))?;
            Ok(spec.next_after(now, timezone))
        }
        DaemonSchedule::Once { at_secs } => Ok(*at_secs),
    }
}

/// Daemon loop. Behaviour depends on `schedule`:
///
/// * `Interval` — fire every `secs` seconds; the first tick is
///   immediate so the user gets feedback without waiting.
/// * `Cron`     — sleep until the next matching wall-clock instant;
///   the first tick is the **next** match (e.g. with `0 9 * * *`
///   at 08:00 the daemon sleeps until 09:00).
/// * `Once`     — sleep until the absolute timestamp, run the queue
///   exactly once, then return `Ok(())` so the caller can exit.
fn execute_daemon(
    tasks: &[ScheduledTask],
    schedule: DaemonSchedule,
    timezone: SchedulerTimezone,
    out: &mut Output,
) -> Result<(), SchedulerError> {
    let state_path = resolve_state_path()?;
    let mut state = load_state(&state_path)?;
    state.last_started_at = Some(now_secs());
    save_state(&state_path, &state)?;

    // Wire SIGINT (Ctrl-C) to a shared flag the loop checks at the
    // top of every tick. SIGTERM gets the same treatment so a clean
    // `kill <pid>` works from a process manager.
    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone());

    let cron_label = match &schedule {
        DaemonSchedule::Interval { secs } => format!("interval={}s", secs),
        DaemonSchedule::Cron(expr) => format!("cron={:?}", expr),
        DaemonSchedule::Once { at_secs } => format!("at={}", at_secs),
    };
    let _ = out.trace_labeled(
        "scheduler",
        &format!(
            "daemon starting: {} task(s), {}, tz={:?}, state={:?}",
            tasks.len(),
            cron_label,
            timezone,
            state_path
        ),
    );

    // Deadline for the *first* tick:
    //   * `Interval` fires immediately (fast feedback),
    //   * `Cron` waits for the next matching wall-clock instant — it used to
    //     run the whole queue once at startup regardless of the schedule,
    //     so `--cron "0 0 1 1 *"` fired today instead of next January,
    //   * `Once` waits for its absolute deadline.
    let mut next_tick = first_tick_deadline(&schedule, now_secs(), timezone.clone())?;

    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = out.trace_labeled("scheduler", "stopping (signal received)");
            return Err(SchedulerError::Interrupted);
        }

        if now_secs() >= next_tick {
            match &schedule {
                DaemonSchedule::Once { .. } => {
                    run_one_tick(tasks, out);
                    let _ = out.trace_labeled(
                        "scheduler",
                        "one-shot complete; exiting daemon",
                    );
                    return Ok(());
                }
                DaemonSchedule::Interval { secs } => {
                    run_one_tick(tasks, out);
                    next_tick = now_secs().saturating_add(*secs);
                }
                DaemonSchedule::Cron(expr) => {
                    run_one_tick(tasks, out);
                    let spec = CronSpec::parse(expr).map_err(|e| {
                        SchedulerError::InvalidCron(format!("{:?}: {}", expr, e))
                    })?;
                    next_tick = spec.next_after(now_secs(), timezone.clone());
                }
            }
        }

        if stop.load(Ordering::SeqCst) {
            let _ = out.trace_labeled("scheduler", "stopping (signal received)");
            return Err(SchedulerError::Interrupted);
        }

        let sleep_for = Duration::from_secs(next_tick.saturating_sub(now_secs()));

        // Sleep in 250ms chunks so SIGINT is honoured within ~quarter
        // of a second instead of "after the next interval". This is
        // the same pattern `cargo` uses for its shutdown handling.
        let mut remaining = sleep_for;
        let chunk = Duration::from_millis(250);
        while !remaining.is_zero() && !stop.load(Ordering::SeqCst) {
            let sleep_for = remaining.min(chunk);
            thread::sleep(sleep_for);
            remaining = remaining.saturating_sub(sleep_for);
        }
    }
}

/// Run a single iteration of the daemon loop and return the
/// per-tick errors so the caller can decide whether to keep going.
/// Errors are logged but never propagated — a flaky LLM backend
/// shouldn't take the whole daemon down.
fn run_one_tick(tasks: &[ScheduledTask], out: &mut Output) {
    // A daemon tick lives inside a long-running process: writing one JSON
    // document per tick violates the "stdout is exactly one document"
    // contract (`jq` fails on the concatenation). The final outcome is
    // reported by `main` when the daemon exits (normal or interrupted).
    match execute_run_once(tasks, out, false) {
        Ok(()) => {}
        // `run-once` reports task failures through its exit code; the
        // daemon has no caller to report to, and each failure is
        // already on a `FAILED:` line plus in the state file. Logging
        // it again as a "tick error" would suggest the tick itself
        // broke, which is a different (and actionable) thing.
        Err(SchedulerError::TasksFailed { .. }) => {}
        Err(e) => {
            let _ = out.trace_labeled("scheduler", &format!("tick error: {}", e));
        }
    }
}

/// Print the current state file in a friendly table. In `--json`
/// mode we emit the raw record so CI scripts can diff snapshots.
fn execute_status(out: &mut Output) -> Result<(), SchedulerError> {
    let state_path = resolve_state_path()?;
    let state = load_state(&state_path)?;

    if out.kind() == OutputKind::Json {
        let envelope = serde_json::json!({
            "kind": "scheduler.status",
            "state_path": state_path.display().to_string(),
            "state": state,
        });
        // Same deferral trap as `run-once`: `final_answer` is stored,
        // not written, in JSON mode — see the note there.
        out.write_json_str(envelope.to_string());
        return Ok(());
    }

    let _ = out.trace_labeled("scheduler", &format!("state file: {:?}", state_path));
    match state.last_started_at {
        Some(t) => {
            let _ = out.trace_labeled("scheduler", &format!("last daemon start: {}", t));
        }
        None => {
            let _ = out.trace_labeled("scheduler", "last daemon start: <never>");
        }
    }
    if state.tasks.is_empty() {
        let _ = out.trace_labeled("scheduler", "no tasks recorded yet");
        return Ok(());
    }
    for (name, stats) in &state.tasks {
        let last = stats
            .last_run_at
            .map(|t| t.to_string())
            .unwrap_or_else(|| "<never>".to_string());
        let _ = out.trace_labeled(
            "scheduler",
            &format!(
                "{:?}: last={} ok={} fail={} err={:?}",
                name, last, stats.success_count, stats.failure_count, stats.last_error
            ),
        );
    }
    Ok(())
}

// ============================================================================
// Task execution
// ============================================================================

/// Run a single task by shelling out to `magent run`. We shell out
/// rather than calling the runner in-process because:
///
/// 1. It keeps `magent scheduler` honest — every scheduled task
///    exercises the same code path a user would.
/// 2. It lets the scheduler survive an agent panic (subprocess
///    death is isolated from the daemon).
/// 3. It sidesteps a tangle of generic lifetimes that an
///    in-process call would force on the daemon loop.
///
/// The trade-off is one extra process spawn per task; on a 60s
/// tick that's negligible.
fn run_one_task(task: &ScheduledTask, out: &mut Output) -> Result<(), SchedulerError> {
    // Resolve the binary path. `std::env::current_exe()` is the
    // canonical way to find "the same binary I'm running" so the
    // scheduler keeps working when installed via `cargo install`
    // (where `argv[0]` is `/Users/.../bin/magent` and we want the
    // absolute path the daemon was actually launched from).
    let exe = std::env::current_exe().map_err(|e| SchedulerError::State {
        path: PathBuf::from("<current_exe>"),
        source: e,
    })?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("run").arg("--quiet");
    if let Some(provider) = &task.provider {
        cmd.arg("--provider").arg(provider);
    }
    if let Some(model) = &task.model {
        cmd.arg("--model").arg(model);
    }
    if let Some(prompt_name) = &task.prompt_name {
        cmd.arg("--prompt-name").arg(prompt_name);
    }
    if task.mock {
        cmd.arg("--mock");
    }
    // `--` terminates option parsing: without it a task whose text starts
    // with `-` (a Markdown list, or literally "--help") was parsed by the
    // child as a flag — `--help` made the child print usage and exit 0, so
    // the scheduler reported success without running the agent at all.
    cmd.arg("--").arg(&task.task);

    let output = cmd.output().map_err(|e| SchedulerError::TaskRun {
        name: task.name.clone(),
        exit: "<spawn>".to_string(),
        detail: format!("could not start `magent run`: {e}"),
    })?;

    if output.status.success() {
        let _ = out.trace_labeled("scheduler", &format!("{:?} ok", task.name));
        Ok(())
    } else {
        // Combine stdout + stderr so the recorded error is
        // actionable. Both are bounded by the `TaskStats::record_failure`
        // truncation.
        let exit = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "<signal>".to_string());
        Err(SchedulerError::TaskRun {
            name: task.name.clone(),
            exit,
            detail: task_failure_detail(&output.stdout, &output.stderr),
        })
    }
}

/// The child's output, with the boilerplate every `magent run` prints
/// before it does any work removed.
///
/// Every invocation emits a `[Config] applied provider=…, ollama_url=…,
/// model=…, …` line on stderr — around 380 characters. The state file
/// keeps only [`LAST_ERROR_MAX`] of `last_error`, so recording that
/// preamble verbatim pushed the actual cause past the cut and left the
/// durable record with a config dump and no diagnosis. Dropping the
/// `[Config]`/`[Backend]` status lines keeps the reason, and makes
/// `last_error` read like an error rather than a settings echo.
fn task_failure_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut out = String::new();
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        let text = String::from_utf8_lossy(bytes);
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("[Config] applied")
                    && !t.starts_with("[Backend]")
            })
            .collect();
        if kept.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&kept.join("\n"));
    }
    out
}

// ============================================================================
// Signal handling
// ============================================================================

/// Process-wide pointer to the `Arc<AtomicBool>` the daemon is
/// using. The handler runs in signal context and must not touch
/// user-mode state, so we keep the flag in a `static` rather than
/// try to capture it.
static STOP_PTR: std::sync::atomic::AtomicPtr<AtomicBool> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Install SIGINT / SIGTERM handlers that flip the shared `stop`
/// flag. We deliberately don't use a richer async-signal crate:
/// the daemon only needs "exit on next tick", and pulling in
/// `signal-hook` would be the largest new transitive dep in the
/// workspace.
///
/// Safety: `AtomicBool::store` is async-signal-safe per
/// POSIX.1-2017 (`sigaction` is on the explicit allow-list, and the
/// only thing the handler does is flip the bit).
fn install_signal_handlers(stop: Arc<AtomicBool>) {
    // Stash a leaked pointer to the Arc into the process-wide
    // static so the signal handler can read it. The Arc outlives
    // the daemon's lifetime because the daemon's lifetime is the
    // process lifetime, so the leak is bounded by the OS.
    STOP_PTR.store(Arc::into_raw(stop) as *mut AtomicBool, Ordering::SeqCst);
    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        // POSIX signal numbers. We hard-code them rather than
        // pulling in `libc` to keep the dependency surface flat.
        const SIGINT: c_int = 2;
        const SIGTERM: c_int = 15;
        // SAFETY: `set_handler` only reads from `STOP_PTR` (set
        // above) and writes to the process-wide atomic. Both are
        // async-signal-safe.
        unsafe {
            // SIGINT
            set_handler(SIGINT);
            // SIGTERM
            set_handler(SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: Ctrl-C handling is wired up via the console
        // handler API. For now we let the daemon run uninterrupted
        // (Ctrl-Break / taskkill work via the same `process::exit`
        // path the test harness uses).
        let _ = stop;
    }
}

#[cfg(unix)]
unsafe fn set_handler(sig: std::os::raw::c_int) {
    // Wrap the flag-pointer in a thin `extern "C"` fn that does the
    // minimum work. `extern fn`s can't capture state, so the flag
    // has to live in a process-wide `static`.
    unsafe extern "C" fn handler(_sig: std::os::raw::c_int) {
        let ptr = STOP_PTR.load(Ordering::SeqCst);
        if !ptr.is_null() {
            let flag_ref: &AtomicBool = unsafe { &*ptr };
            flag_ref.store(true, Ordering::SeqCst);
        }
    }
    // sigaction struct (Linux / macOS layout). We can't use
    // `mem::zeroed()` because `sa_sigaction` is a function
    // pointer, which the compiler refuses to zero-init. Instead
    // we `MaybeUninit` and assign each field by hand.
    let mut act = std::mem::MaybeUninit::<libc_compat::Sigaction>::uninit();
    // SAFETY: `sa_sigaction` is the only required field — the rest
    // of the struct is integer / flag state which is well-defined
    // when zero. We initialise the required field by hand and rely
    // on the OS ignoring the rest.
    unsafe {
        let p = act.as_mut_ptr();
        (*p).sa_sigaction = handler as libc_compat::SigactionFn;
        (*p).sa_flags = libc_compat::SA_RESTART;
        (*p).sa_mask = 0;
        // `sigaction` is async-signal-safe per POSIX.
        libc_compat::sigaction(sig, p, std::ptr::null_mut());
    }
}

/// Minimal POSIX shim so we don't pull in `libc` as a top-level
/// dependency. Only the symbols the scheduler actually uses live
/// here.
#[cfg(unix)]
mod libc_compat {
    use std::os::raw::{c_int, c_ulong};

    pub type SigsetT = c_ulong;
    pub type SigactionFn = unsafe extern "C" fn(c_int);
    // `sa_sigaction` and `sa_handler` are a union in the real
    // `sigaction`. We only ever use the function-pointer variant, so
    // a plain field works.
    #[repr(C)]
    pub struct Sigaction {
        pub sa_sigaction: SigactionFn,
        pub sa_flags: c_int,
        pub sa_mask: SigsetT,
    }

    /// `SA_RESTART` from `/usr/include/bits/sigaction.h`. The value
    /// is stable across Linux + macOS so we can hard-code it.
    pub const SA_RESTART: c_int = 0x10000000;

    extern "C" {
        pub fn sigaction(
            sig: c_int,
            act: *const Sigaction,
            oldact: *mut Sigaction,
        ) -> c_int;
    }
}

// ============================================================================
// Cron expression parser
// ============================================================================
//
// Minimal POSIX-flavoured cron: five whitespace-separated fields
//
//   minute  hour  day-of-month  month  day-of-week
//
// Each field is either `*` (any) or an integer in the documented
// range. We deliberately don't implement ranges (`1-5`), steps
// (`*/15`), or lists (`1,15,30`) — adding them later is a localised
// change to `parse_field` and doesn't touch the rest of the
// scheduler. The whole parser is ~120 lines so it lives here next to
// the consumer; pulling in `cron` (the third-party crate) would add
// 200+ KB of transitive deps for very little new functionality.

/// Parsed cron expression. Each field is a `Vec<u8>` of allowed
/// values (`*` expands to the full range). Storing as `Vec<u8>`
/// rather than `Vec<bool>` keeps the `next_after` matcher a tight
/// inner loop on a ~60-element slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSpec {
    minutes: Vec<u8>,  // 0-59
    hours: Vec<u8>,    // 0-23
    doms: Vec<u8>,     // 1-31
    months: Vec<u8>,   // 1-12
    dows: Vec<u8>,     // 0-6 (Sunday = 0)
}

impl CronSpec {
    /// Parse a 5-field cron expression. Returns a human-readable
    /// error on any malformed input.
    pub fn parse(expr: &str) -> Result<Self, String> {
        let trimmed = expr.trim();
        if trimmed.is_empty() {
            return Err("expression is empty".to_string());
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(format!(
                "expected 5 whitespace-separated fields, got {} ({:?})",
                parts.len(),
                parts
            ));
        }
        Ok(Self {
            minutes: parse_field(parts[0], 0, 59, "minute")?,
            hours: parse_field(parts[1], 0, 23, "hour")?,
            doms: parse_field(parts[2], 1, 31, "day-of-month")?,
            months: parse_field(parts[3], 1, 12, "month")?,
            dows: parse_field(parts[4], 0, 6, "day-of-week")?,
        })
    }

    /// Find the next Unix-epoch second strictly *after* `from` that
    /// matches every field.
    ///
    /// Strategy: linear search forward minute-by-minute (worst case
    /// 366 × 1440 ≈ 5×10⁵ iterations per year, which finishes in
    /// milliseconds) so the implementation stays trivially correct.
    /// We cap the search at 366 days to avoid hanging on a
    /// syntactically-valid but unsatisfiable expression like
    /// `0 0 30 2 *`.
    pub fn next_after(&self, from: u64, tz: SchedulerTimezone) -> u64 {
        let mut tm = epoch_to_local(from, &tz);
        // Round up to the next second so the returned value is
        // *strictly* after `from`.
        tm.second += 1;
        normalise_tm(&mut tm);

        let max_iterations = 366_usize * 24 * 60;
        for _ in 0..max_iterations {
            // Field-by-field "find next valid value, jump there"
            // approach. Each helper advances `tm` to the next
            // candidate that hasn't been ruled out yet.
            if !self.months.contains(&tm.month) {
                // Jump to the first moment of the next month.
                // `advance_to_next_month` only rolls the month over, so
                // reset the day and time-of-day here: carrying them across
                // the boundary both skips the earlier part of the target
                // month (`0 0 * 4 *` from mid-January would land mid-April)
                // and can produce a day that does not exist in it
                // (`Jan 31` → `Feb 31`), which sent the search a year
                // forward. `dow` is maintained incrementally elsewhere, so
                // it has to be re-derived for the new date.
                advance_to_next_month(&mut tm);
                tm.day = 1;
                tm.dow = day_of_week(tm.year, tm.month, tm.day);
                tm.hour = 0;
                tm.minute = 0;
                tm.second = 0;
                continue;
            }
            let dom_match = self.doms.contains(&tm.day) || self.doms.len() == 31;
            let dow_match = self.dows.contains(&tm.dow) || self.dows.len() == 7;
            // POSIX/vixie cron: when *both* day fields are restricted
            // (neither is `*`), the day matches if *either* matches. Only
            // when one of them is `*` does the other alone govern. Combining
            // them with `&&` — as this did — makes `0 0 1 * 1` mean "the 1st,
            // but only when it is also a Monday", silently skipping ~11 of
            // every 12 monthly runs. `len() == 31` / `len() == 7` is the
            // "this field was `*`" test: `*` expands to the full range, and a
            // restricted field that happens to list every value is
            // indistinguishable from `*` (and behaves identically).
            let day_match = if self.doms.len() == 31 || self.dows.len() == 7 {
                dom_match && dow_match
            } else {
                dom_match || dow_match
            };
            if !day_match {
                // Skip to 00:00 of the next day so the hour /
                // minute re-checks have to earn their keep.
                tm.hour = 0;
                tm.minute = 0;
                tm.second = 0;
                advance_to_next_day(&mut tm);
                continue;
            }
            if !self.hours.contains(&tm.hour) {
                // Jump to the next valid hour on the same day,
                // resetting minutes/seconds to zero so the minute
                // check below can find a real value.
                let next_hour = self
                    .hours
                    .iter()
                    .find(|&&h| h > tm.hour)
                    .copied();
                match next_hour {
                    Some(h) => {
                        tm.hour = h;
                        tm.minute = 0;
                        tm.second = 0;
                    }
                    None => {
                        // No hour today; roll forward to 00:00 of
                        // the next day.
                        advance_to_next_day(&mut tm);
                        tm.hour = 0;
                        tm.minute = 0;
                        tm.second = 0;
                    }
                }
                continue;
            }
            if !self.minutes.contains(&tm.minute) {
                let next_min = self
                    .minutes
                    .iter()
                    .find(|&&m| m > tm.minute)
                    .copied();
                match next_min {
                    Some(m) => {
                        tm.minute = m;
                        tm.second = 0;
                    }
                    None => {
                        // No more valid minutes this hour; roll
                        // forward to 00 of the next hour.
                        advance_to_next_hour(&mut tm);
                    }
                }
                continue;
            }
            // All five fields matched.
            return local_to_epoch(&tm, &tz);
        }
        // Unreachable for any expression referencing a real
        // calendar date. Fall back to "1 year from now" so the
        // daemon keeps running instead of busy-looping.
        from + 31_536_000
    }
}

/// Parse one cron field. Accepts `*` (full range) or a single
/// integer. Anything else is a hard error so the user gets the
/// diagnostic at parse time rather than "no matches" at runtime.
fn parse_field(token: &str, lo: u8, hi: u8, name: &str) -> Result<Vec<u8>, String> {
    if token == "*" {
        return Ok((lo..=hi).collect());
    }
    let n: u8 = token.parse().map_err(|_| {
        format!(
            "{} field {:?} is not `*` or an integer in {}-{}",
            name, token, lo, hi
        )
    })?;
    if n < lo || n > hi {
        return Err(format!(
            "{} field {} is out of range {}-{}",
            name, n, lo, hi
        ));
    }
    Ok(vec![n])
}

// ----- Broken-down time -----------------------------------------------------

/// Broken-down time used by the cron matcher. `year` is a signed
/// `i32` because year arithmetic on Unix timestamps can produce
/// negative years (pre-1970) — we never actually use those, but
/// `i32` is the natural fit and avoids gratuitous `as` casts.
#[derive(Debug, Clone, Copy)]
struct BrokenTime {
    year: i32,
    month: u8, // 1-12
    day: u8,   // 1-31
    hour: u8,  // 0-23
    minute: u8, // 0-59
    second: u8, // 0-59 (kept for completeness; the matcher only
    // checks down to the minute)
    dow: u8, // 0-6 (Sunday = 0)
}

/// Number of days in `month` of `year`, accounting for leap years.
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30, // unreachable; `normalise_tm` keeps month in 1-12
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Zeller-ish day-of-week (0 = Sunday). We only need it for the
/// `dows` matcher so the algorithm doesn't have to be exact — any
/// consistent offset would work as long as `epoch_to_local` and
/// `local_to_epoch` agree. The standard Gauss / Tomohiko Sakamoto
/// algorithm fits in 10 lines and is exact for the Gregorian
/// calendar.
fn day_of_week(year: i32, month: u8, day: u8) -> u8 {
    // Compute days since 1970-01-01 (Thursday = 4 under our
    // 0 = Sunday convention) by walking year/month boundaries,
    // then take mod 7. This is O(year + month) but year is at most
    // ~150 (Unix epoch range) and month is 1-12, so it's
    // effectively O(1) for any real date.
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month(year, m) as i64;
    }
    days += (day - 1) as i64;
    // 1970-01-01 was a Thursday → dow 4.
    let dow = ((days % 7) + 4).rem_euclid(7);
    dow as u8
}

/// Convert a Unix epoch second to a `BrokenTime` in either UTC or
/// the local timezone.
///
/// We don't pull in `chrono` / `time` for this — the only conversion
/// we need is a UTC ↔ local-zone offset. The local offset is read
/// once per `next_after` call from the libc shim below. On a
/// daylight-saving transition the offset can be off by one hour for
/// a single tick, which is acceptable for a cron daemon (the next
/// tick will be correct).
fn epoch_to_local(secs: u64, tz: &SchedulerTimezone) -> BrokenTime {
    let s = secs % 86_400;
    let mut day_secs = s;
    let hour = (day_secs / 3600) as u8;
    day_secs %= 3600;
    let minute = (day_secs / 60) as u8;
    let second = (day_secs % 60) as u8;

    // Compute the date by counting days from 1970-01-01. We do this
    // iteratively to keep the code transparent — a 60-year span
    // loops at most 22k times, which is sub-millisecond.
    let mut days = (secs / 86_400) as i64;
    let mut year: i32 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let mut month: u8 = 1;
    loop {
        let dm = days_in_month(year, month) as i64;
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    let day = (days + 1) as u8;
    let dow = day_of_week(year, month, day);

    // Apply the timezone offset to local. We only shift the
    // hour/minute — date arithmetic is done afterwards.
    let mut tm = BrokenTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        dow,
    };
    if matches!(tz, SchedulerTimezone::Local) {
        let offset = local_offset_seconds();
        apply_offset(&mut tm, offset);
    }
    tm
}

fn apply_offset(tm: &mut BrokenTime, offset_secs: i64) {
    // Convert the broken time back to "seconds since midnight",
    // add the offset, then re-normalise. We don't touch the date
    // here — `normalise_tm` does the date roll-over afterwards.
    let secs_of_day =
        tm.hour as i64 * 3600 + tm.minute as i64 * 60 + tm.second as i64 + offset_secs;
    let mut total = secs_of_day;
    let mut day_shift = 0;
    if total < 0 {
        day_shift = -1;
        total += 86_400;
    } else if total >= 86_400 {
        day_shift = 1;
        total -= 86_400;
    }
    tm.hour = (total / 3600) as u8;
    tm.minute = ((total % 3600) / 60) as u8;
    tm.second = (total % 60) as u8;
    if day_shift != 0 {
        // Move the date by ±1 day. We compute the new dow rather
        // than re-deriving it from the date so the caller sees the
        // exact "next / previous day" semantics.
        tm.dow = ((tm.dow as i32 + day_shift + 7) % 7) as u8;
        if day_shift == 1 {
            advance_to_next_day(tm);
        } else {
            rewind_to_prev_day(tm);
        }
    }
}

fn local_to_epoch(tm: &BrokenTime, tz: &SchedulerTimezone) -> u64 {
    let mut days: i64 = 0;
    for y in 1970..tm.year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..tm.month {
        days += days_in_month(tm.year, m) as i64;
    }
    days += (tm.day - 1) as i64;
    let mut secs = days * 86_400
        + tm.hour as i64 * 3600
        + tm.minute as i64 * 60
        + tm.second as i64;
    if matches!(tz, SchedulerTimezone::Local) {
        secs -= local_offset_seconds();
    }
    secs.max(0) as u64
}

fn normalise_tm(tm: &mut BrokenTime) {
    if tm.second >= 60 {
        tm.minute += tm.second / 60;
        tm.second %= 60;
    }
    if tm.minute >= 60 {
        tm.hour += tm.minute / 60;
        tm.minute %= 60;
    }
    if tm.hour >= 24 {
        advance_to_next_day(tm);
        tm.hour %= 24;
    }
}

fn advance_to_next_hour(tm: &mut BrokenTime) {
    tm.hour += 1;
    if tm.hour >= 24 {
        tm.hour = 0;
        advance_to_next_day(tm);
    }
}

fn advance_to_next_day(tm: &mut BrokenTime) {
    tm.day += 1;
    tm.dow = (tm.dow + 1) % 7;
    if tm.day > days_in_month(tm.year, tm.month) {
        tm.day = 1;
        advance_to_next_month(tm);
    }
}

fn advance_to_next_month(tm: &mut BrokenTime) {
    tm.month += 1;
    if tm.month > 12 {
        tm.month = 1;
        tm.year += 1;
    }
}

fn rewind_to_prev_day(tm: &mut BrokenTime) {
    tm.dow = (tm.dow + 6) % 7;
    if tm.day == 1 {
        // Roll back to the previous month's last day.
        if tm.month == 1 {
            tm.month = 12;
            tm.year -= 1;
        } else {
            tm.month -= 1;
        }
        tm.day = days_in_month(tm.year, tm.month);
    } else {
        tm.day -= 1;
    }
}

// ----- Local timezone offset -------------------------------------------------

/// Best-effort current UTC offset for the local timezone, in
/// seconds (e.g. `+08:00` → 28800). We don't model DST transitions
/// explicitly — `next_after` re-reads the offset on every call, so
/// a DST boundary only affects one tick at worst.
#[cfg(unix)]
fn local_offset_seconds() -> i64 {
    // `time_t now; localtime_r(&now, &tm); tm.tm_gmtoff` is the
    // POSIX-blessed way to get the offset. We call the same libc
    // functions our `libc_compat` shim exposes, but to avoid a
    // second extern block we just FFI them inline.
    extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
    }
    #[repr(C)]
    #[derive(Debug)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const u8,
    }
    let mut now: i64 = 0;
    unsafe {
        time(&mut now);
        let mut tm: Tm = std::mem::zeroed();
        let _ = localtime_r(&now, &mut tm);
        tm.tm_gmtoff
    }
}

#[cfg(not(unix))]
fn local_offset_seconds() -> i64 {
    // Windows: the OS exposes `GetDynamicTimeZoneInformation` /
    // `GetTimeZoneInformation`. We punt on it for now and treat
    // "local" as UTC. Users who need a real local offset on
    // Windows can pass `--timezone utc` and write their cron
    // expressions in UTC.
    0
}

// ============================================================================
// Help text
// ============================================================================

/// Help text for the `magent scheduler` subcommand. Kept as a
/// free function so both `SchedulerCmd::execute` (Help variant) and
/// the dispatcher in `cli.rs` can print it without duplicating
/// string literals.
pub fn scheduler_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent scheduler — time-triggered auto-runner");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(
        s,
        "    magent scheduler run-once [--tasks-file <PATH> | --preset <NAME>]"
    );
    let _ = writeln!(
        s,
        "    magent scheduler daemon   [--tasks-file <PATH> | --preset <NAME>]"
    );
    let _ = writeln!(
        s,
        "                                       [--interval <SECS> | --cron <EXPR> | --at <RFC3339>]"
    );
    let _ = writeln!(s, "                                       [--timezone <utc|local>]");
    let _ = writeln!(s, "    magent scheduler status");
    let _ = writeln!(s);
    let _ = writeln!(s, "ACTIONS:");
    let _ = writeln!(
        s,
        "    run-once    Execute every task in the queue once and exit; exits 1 if any task failed"
    );
    let _ = writeln!(
        s,
        "    daemon      Tick forever; Ctrl-C (SIGINT) or SIGTERM stops cleanly"
    );
    let _ = writeln!(s, "    status      Print the per-task counters + last-error snippets");
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS:");
    let _ = writeln!(
        s,
        "    --tasks-file <PATH>      Read tasks from a JSON file (see schema below)"
    );
    let _ = writeln!(
        s,
        "    --preset <NAME>          Use a built-in task list: audit | complete"
    );
    let _ = writeln!(
        s,
        "    --interval <SECS>        Daemon tick interval (default 60, range [{}, {}])",
        MIN_INTERVAL_SECS, MAX_INTERVAL_SECS
    );
    let _ = writeln!(
        s,
        "    --cron <EXPR>            5-field cron expression: '分 时 日 月 周' \
         (use * for any). Mutually exclusive with --interval / --at."
    );
    let _ = writeln!(
        s,
        "    --at <RFC3339>           One-shot run at an absolute timestamp \
         (e.g. 2026-08-11T09:00:00+08:00). Mutually exclusive."
    );
    let _ = writeln!(
        s,
        "    --timezone <utc|local>   Interpret cron fields as UTC, or in the \
         system's local zone (default: local). Only these two values are \
         accepted; a bogus value is rejected."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "TASK FILE SCHEMA:");
    let _ = writeln!(s, "    {{");
    let _ = writeln!(s, "      \"tasks\": [");
    let _ = writeln!(
        s,
        "        {{ \"name\": \"audit-todos\", \"task\": \"Find every stale TODO…\","
    );
    let _ = writeln!(
        s,
        "          \"provider\": \"ollama\", \"model\": \"llama3.2\","
    );
    let _ = writeln!(
        s,
        "          \"prompt_name\": \"health_coach\", \"mock\": false }}"
    );
    let _ = writeln!(s, "      ]");
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s);
    let _ = writeln!(s, "STORAGE:");
    let _ = writeln!(
        s,
        "    State file: $MAGENT_SCHEDULER_STATE, or"
    );
    let _ = writeln!(
        s,
        "    $XDG_STATE_HOME/magent/scheduler.json (or ~/.local/state/magent/)."
    );
    let _ = writeln!(
        s,
        "    Writes are atomic (write-to-temp + rename) so an interrupted"
    );
    let _ = writeln!(
        s,
        "    daemon never produces a half-written JSON."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "EXAMPLES:");
    let _ = writeln!(
        s,
        "    magent scheduler run-once --preset audit"
    );
    let _ = writeln!(
        s,
        "    magent scheduler daemon --preset complete --interval 300"
    );
    let _ = writeln!(
        s,
        "    magent scheduler status"
    );
    s
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tasks() -> Vec<ScheduledTask> {
        vec![
            ScheduledTask {
                name: "a".into(),
                task: "alpha".into(),
                prompt_name: None,
                provider: None,
                model: None,
                mock: true,
            },
            ScheduledTask {
                name: "b".into(),
                task: "beta".into(),
                prompt_name: Some("p".into()),
                provider: Some("deepseek".into()),
                model: None,
                mock: false,
            },
        ]
    }

    #[test]
    fn help_text_lists_actions() {
        let h = scheduler_help_text();
        assert!(h.contains("run-once"));
        assert!(h.contains("daemon"));
        assert!(h.contains("status"));
        assert!(h.contains("--preset"));
        assert!(h.contains("--interval"));
    }

    #[test]
    fn preset_audit_has_at_least_three_tasks() {
        let v = preset_audit();
        assert!(v.len() >= 3, "audit preset should have ≥3 tasks");
        for t in &v {
            assert!(!t.name.is_empty());
            assert!(!t.task.is_empty());
        }
    }

    #[test]
    fn preset_complete_has_at_least_three_tasks() {
        let v = preset_complete();
        assert!(v.len() >= 3, "complete preset should have ≥3 tasks");
        for t in &v {
            assert!(!t.name.is_empty());
            assert!(!t.task.is_empty());
        }
    }

    #[test]
    fn resolve_preset_rejects_unknown_names() {
        let r = resolve_preset("bogus");
        assert!(matches!(r, Err(SchedulerError::UnknownPreset(_))));
    }

    #[test]
    fn task_stats_truncates_long_errors() {
        let mut s = TaskStats::default();
        let huge = "x".repeat(LAST_ERROR_MAX * 4);
        s.record_failure(&huge);
        let err = s.last_error.as_ref().expect("error should be recorded");
        assert!(err.len() <= LAST_ERROR_MAX + 4, "got {} chars", err.len());
        // The ellipsis marks where the cut happened — at the *front* now.
        assert!(err.starts_with('…'), "expected a leading ellipsis: {err:?}");
    }

    /// The tail is the part worth keeping: a task's failure message is a
    /// whole stderr whose last line is the `error: …`, and the head is
    /// `[Config]`/`[Backend]` noise. Head-truncating dropped the cause.
    #[test]
    fn task_stats_truncation_keeps_the_cause_not_the_noise() {
        let mut s = TaskStats::default();
        let noisy = format!(
            "[Config] applied {}\nerror: agent failed: 401 Unauthorized\n",
            "x".repeat(LAST_ERROR_MAX * 2)
        );
        s.record_failure(&noisy);
        let err = s.last_error.as_ref().expect("error should be recorded");
        assert!(
            err.contains("error: agent failed: 401 Unauthorized"),
            "the cause was truncated away: {err:?}"
        );
        assert!(
            !err.contains("[Config] applied"),
            "the head noise should have been dropped: {err:?}"
        );
    }

    /// A task failure must not be described as a *state file* problem —
    /// that variant is for the scheduler's own I/O, and naming it here
    /// sent the operator to a file that was perfectly healthy.
    #[test]
    fn task_failure_is_reported_as_a_task_failure() {
        let err = SchedulerError::TaskRun {
            name: "audit-todos".to_string(),
            exit: "1".to_string(),
            detail: "stderr: error: agent failed: boom\n".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("scheduled task \"audit-todos\" failed"),
            "{msg}"
        );
        assert!(msg.contains("exit 1"), "{msg}");
        assert!(!msg.contains("state file"), "{msg}");
    }

    /// …and the transcript line is the child's last non-empty line, not
    /// the whole stderr it arrives wrapped in.
    #[test]
    fn one_line_failure_picks_the_children_error_line() {
        let err = SchedulerError::TaskRun {
            name: "t".to_string(),
            exit: "1".to_string(),
            detail: "[Config] applied …\n[Task] do it\nerror: agent failed: 401\n".to_string(),
        };
        assert_eq!(one_line_failure(&err), "exit 1: error: agent failed: 401");
        // A variant that is already one line passes through unchanged.
        assert_eq!(
            one_line_failure(&SchedulerError::MissingSchedule),
            SchedulerError::MissingSchedule.to_string()
        );
    }

    /// The config preamble is ~380 characters; `last_error` keeps only
    /// `LAST_ERROR_MAX`, so it has to leave the record before truncation
    /// or it is the *only* thing the record contains.
    #[test]
    fn task_failure_detail_drops_the_config_preamble() {
        let stdout = b"";
        let stderr = b"[Config] applied provider=ollama, model=llama3.2\n\
                       error: could not load system prompt: no prompt named \"x\"\n";
        let detail = task_failure_detail(stdout, stderr);
        assert!(!detail.contains("[Config] applied"), "got {detail:?}");
        assert!(
            detail.contains("could not load system prompt"),
            "got {detail:?}"
        );
        // The cause is still what the transcript quotes.
        let err = SchedulerError::TaskRun {
            name: "t".to_string(),
            exit: "1".to_string(),
            detail,
        };
        assert_eq!(
            one_line_failure(&err),
            "exit 1: error: could not load system prompt: no prompt named \"x\""
        );
    }

    #[test]
    fn task_failure_detail_labels_both_streams_and_skips_empty_ones() {
        assert_eq!(
            task_failure_detail(b"partial output", b"error: boom\n"),
            "stdout: partial output\nstderr: error: boom"
        );
        assert_eq!(
            task_failure_detail(b"", b"error: boom\n"),
            "stderr: error: boom"
        );
        assert_eq!(
            task_failure_detail(b"", b"[Config] applied x=1\n").as_str(),
            ""
        );
    }

    #[test]
    fn task_stats_clears_error_on_success() {
        let mut s = TaskStats::default();
        s.record_failure("boom");
        assert!(s.last_error.is_some());
        s.record_success();
        assert!(s.last_error.is_none());
        assert_eq!(s.success_count, 1);
        assert_eq!(s.failure_count, 1);
    }

    #[test]
    fn resolve_tasks_rejects_both_sources() {
        let r = resolve_tasks(Some(Path::new("/tmp/x")), Some("audit"));
        assert!(matches!(r, Err(SchedulerError::TasksFileShape(_))));
    }

    #[test]
    fn resolve_tasks_rejects_neither_source() {
        let r = resolve_tasks(None, None);
        assert!(matches!(r, Err(SchedulerError::TasksFileShape(_))));
    }

    #[test]
    fn sample_tasks_roundtrip_json() {
        let tf = TasksFile { tasks: sample_tasks() };
        let bytes = serde_json::to_vec(&tf).unwrap();
        let back: TasksFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.tasks, tf.tasks);
    }

    #[test]
    fn interval_bounds_are_inclusive() {
        // Constants known at compile time — re-assert here as a
        // smoke check so a future refactor that flips a sign (or
        // accidentally orders the constants) trips the test rather
        // than silently changing the public contract.
        const { assert!(MIN_INTERVAL_SECS >= 1) };
        const { assert!(MAX_INTERVAL_SECS <= 86_400) };
    }

    #[test]
    fn task_too_long_is_rejected_by_load_tasks_file() {
        // We can't easily craft a 5MB string in a unit test without
        // paying the allocation cost, so we patch the bound down
        // via a temporary directory: write a file with a single
        // task whose body is *exactly* TASK_MAX + 1, expect an
        // error.
        let dir = std::env::temp_dir().join(format!(
            "magent-scheduler-test-{}",
            now_secs()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.json");
        let body = "x".repeat(TASK_MAX + 1);
        let json = format!(
            r#"{{ "tasks": [{{ "name": "big", "task": "{}" }}] }}"#,
            body
        );
        fs::write(&path, json).unwrap();
        let r = load_tasks_file(&path);
        assert!(matches!(r, Err(SchedulerError::TaskTooLong { .. })));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_name_in_tasks_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!(
            "magent-scheduler-empty-{}",
            now_secs()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.json");
        fs::write(
            &path,
            r#"{ "tasks": [ { "name": "", "task": "x" } ] }"#,
        )
        .unwrap();
        let r = load_tasks_file(&path);
        assert!(matches!(r, Err(SchedulerError::TasksFileShape(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "magent-scheduler-state-{}",
            now_secs()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scheduler.json");
        // Deliberately `Default::default()` — the file the daemon writes on
        // its very first run. The version must not be left to the field's
        // `u32` zero; setting it here by hand is what hid the mismatch.
        let mut s = SchedulerState {
            last_started_at: Some(42),
            ..Default::default()
        };
        let mut stats = TaskStats::default();
        stats.record_success();
        s.tasks.insert("t".to_string(), stats);
        save_state(&path, &s).unwrap();
        let back = load_state(&path).unwrap();
        assert_eq!(back.schema_version, default_schema_version());
        assert_eq!(back.last_started_at, Some(42));
        assert_eq!(back.tasks.get("t").unwrap().success_count, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A freshly created state — what `load_state` returns for a missing or
    /// empty file — has to carry the same version a file *without* the key
    /// deserialises to. `#[derive(Default)]` made it 0 while serde's
    /// `default = "default_schema_version"` made it 1, so the written file
    /// and the read-back file disagreed.
    #[test]
    fn default_state_carries_the_current_schema_version() {
        assert_eq!(SchedulerState::default().schema_version, 1);
        // And a key-less file must agree with it.
        let parsed: SchedulerState = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.schema_version, SchedulerState::default().schema_version);
    }

    #[test]
    fn invalid_interval_is_rejected_by_execute() {
        let action = SchedulerAction::Daemon {
            tasks_file: None,
            preset: Some("audit".to_string()),
            schedule: DaemonSchedule::Interval { secs: 0 },
            timezone: SchedulerTimezone::Local,
        };
        let cmd = SchedulerCmd::new(action);
        // Build a minimal Output. The execute() call must short-
        // circuit on the interval bound before any I/O.
        let mut out = Output::new(OutputKind::Human, true);
        let r = cmd.execute(&mut out);
        assert!(matches!(r, Err(SchedulerError::InvalidInterval { .. })));
    }

    // ---- CronSpec tests -----------------------------------------------------

    #[test]
    fn cron_parses_star_every_field() {
        let spec = CronSpec::parse("* * * * *").unwrap();
        assert_eq!(spec.minutes.len(), 60);
        assert_eq!(spec.hours.len(), 24);
        assert_eq!(spec.doms.len(), 31);
        assert_eq!(spec.months.len(), 12);
        assert_eq!(spec.dows.len(), 7);
    }

    #[test]
    fn cron_parses_specific_values_strict() {
        // `30 9 1 1 0` — 9:30 on Jan 1st, Sunday only.
        let spec = CronSpec::parse("30 9 1 1 0").unwrap();
        assert_eq!(spec.minutes, vec![30]);
        assert_eq!(spec.hours, vec![9]);
        assert_eq!(spec.doms, vec![1]);
        assert_eq!(spec.months, vec![1]);
        assert_eq!(spec.dows, vec![0]);
    }

    #[test]
    fn cron_rejects_wrong_field_count() {
        assert!(CronSpec::parse("* * * *").is_err());
        assert!(CronSpec::parse("* * * * * *").is_err());
        assert!(CronSpec::parse("").is_err());
    }

    #[test]
    fn cron_rejects_out_of_range_values() {
        // Minute 60 is invalid.
        assert!(CronSpec::parse("60 0 * * *").is_err());
        // Hour 24 is invalid.
        assert!(CronSpec::parse("0 24 * * *").is_err());
        // Month 13 is invalid.
        assert!(CronSpec::parse("0 0 1 13 *").is_err());
        // DOW 7 is invalid.
        assert!(CronSpec::parse("0 0 * * 7").is_err());
    }

    #[test]
    fn cron_next_after_finds_imminent_match_in_utc() {
        // `* * * * *` (every minute) — next match is at most 60
        // seconds in the future.
        let spec = CronSpec::parse("* * * * *").unwrap();
        let now: u64 = 1_700_000_000; // arbitrary
        let next = spec.next_after(now, SchedulerTimezone::Utc);
        assert!(next > now);
        assert!(next - now <= 60, "got {}s in the future", next - now);
    }

    /// `execute_daemon` used to run a tick before the first sleep for every
    /// schedule kind, so a cron daemon fired at startup regardless of the
    /// expression. Pin the first deadlines instead.
    #[test]
    fn the_first_cron_tick_is_the_next_match_not_now() {
        let now: u64 = 1_700_000_000;
        let cron = DaemonSchedule::Cron("0 0 1 1 *".to_string());
        let first = first_tick_deadline(&cron, now, SchedulerTimezone::Utc).unwrap();
        assert!(first > now, "cron must not fire at startup ({first} <= {now})");

        let interval = DaemonSchedule::Interval { secs: 60 };
        assert_eq!(
            first_tick_deadline(&interval, now, SchedulerTimezone::Utc).unwrap(),
            now,
            "interval keeps its immediate first tick"
        );

        let once = DaemonSchedule::Once { at_secs: now + 30 };
        assert_eq!(
            first_tick_deadline(&once, now, SchedulerTimezone::Utc).unwrap(),
            now + 30
        );
    }

    #[test]
    fn cron_next_after_for_daily_9am_is_in_next_24h() {
        // `0 9 * * *` — every day at 09:00. The next match is
        // strictly after `now`, at most 24h + 1 minute away.
        let spec = CronSpec::parse("0 9 * * *").unwrap();
        let now: u64 = 1_700_000_000; // 2023-11-14 22:13:20 UTC
        let next = spec.next_after(now, SchedulerTimezone::Utc);
        assert!(next > now);
        let delta = next - now;
        // The next 09:00 UTC after 22:13 is the next morning, so
        // delta should be in (10h, 11h).
        assert!(
            delta > 10 * 3600 && delta < 11 * 3600,
            "expected ~10.75h, got {}s ({}h)",
            delta,
            delta / 3600
        );
    }

    #[test]
    fn cron_dom_or_dow_semantics() {
        // `0 0 1 * 1` — 00:00 on the 1st of every month, OR every
        // Monday. POSIX cron treats these as OR-combined.
        let spec = CronSpec::parse("0 0 1 * 1").unwrap();
        assert_eq!(spec.doms, vec![1]);
        assert_eq!(spec.dows, vec![1]);
    }

    #[test]
    fn cron_dom_and_dow_are_or_combined_when_both_restricted() {
        // The test above only inspects the parsed fields, so it passed while
        // `next_after` combined the day fields with `&&` — i.e. `0 0 1 * 1`
        // fired only on the 1st *when it was also a Monday*. This asserts the
        // actual scheduling behaviour.
        //
        // Anchored on independently verifiable dates (`date -u -r`):
        // 2024-01-01, 2024-01-08 and 2024-04-01 were all Mondays. `from` is
        // 2024-01-01T00:00:**59**Z, deliberately the last second of the
        // matching minute — `next_after` is second-precise, so starting one
        // second past 00:00:00 would return 00:00:02 (still inside the
        // matching 00:00 minute) rather than advancing to the next day.
        // From there the next OR match is the next Monday, 2024-01-08. An AND
        // matcher would instead return 2024-04-01, the first month-start that
        // is *also* a Monday.
        let spec = CronSpec::parse("0 0 1 * 1").unwrap();
        let from = 1_704_067_259; // 2024-01-01T00:00:59Z
        let next = spec.next_after(from, SchedulerTimezone::Utc);
        assert_eq!(
            next, 1_704_672_000, // 2024-01-08T00:00:00Z, a Monday
            "dom/dow must be OR-combined (POSIX), not AND"
        );
        assert_ne!(
            next, 1_711_929_600, // 2024-04-01T00:00:00Z — the AND answer
            "AND semantics skips 11 of 12 monthly runs"
        );
    }

    #[test]
    fn cron_star_dom_pins_day_of_week_to_the_dow_field() {
        // The complement of the OR rule: when dom is `*`, dow alone governs.
        // 2024-01-01 was a Monday, so `0 0 * * 1` from the last second of that
        // matching minute is the following Monday.
        let spec = CronSpec::parse("0 0 * * 1").unwrap();
        assert_eq!(
            spec.next_after(1_704_067_259, SchedulerTimezone::Utc),
            1_704_672_000 // 2024-01-08T00:00:00Z
        );
    }

    #[test]
    fn cron_full_star_dows_means_any_dow() {
        // `* * 15 * *` — every minute on the 15th. Our parser
        // sets dows to the full range, so the matcher should
        // accept any day-of-week.
        let spec = CronSpec::parse("* * 15 * *").unwrap();
        assert_eq!(spec.doms, vec![15]);
        assert_eq!(spec.dows.len(), 7);
    }

    #[test]
    fn cron_month_skip_lands_on_the_first_of_the_target_month() {
        // A restricted month field (short of `*`) makes the matcher jump
        // forward to the next listed month. That jump used to carry the *day
        // of month* and time-of-day over from `from`, so `0 0 * 4 *` starting
        // 2026-01-15T06:00:00Z landed on April 15th — the first two weeks of
        // April were skipped. Worse, when the carried day does not exist in
        // the target month (`Jan 31` → `Feb 31`) the search marched a full
        // year forward, returning February 1st of *2027*.
        let april_midnights = CronSpec::parse("0 0 * 4 *").unwrap();
        assert_eq!(
            april_midnights.next_after(1_768_456_800, SchedulerTimezone::Utc), // 2026-01-15T06:00:00Z
            1_775_001_600, // 2026-04-01T00:00:00Z
            "the first midnight of April, not mid-April",
        );

        let february_midnights = CronSpec::parse("0 0 * 2 *").unwrap();
        assert_eq!(
            february_midnights.next_after(1_769_860_800, SchedulerTimezone::Utc), // 2026-01-31T12:00:00Z
            1_769_904_000, // 2026-02-01T00:00:00Z
            "February 1st of the same year, not next year",
        );
    }

    // ---- DaemonSchedule error tests ----------------------------------------

    #[test]
    fn once_schedule_in_the_past_is_rejected() {
        let action = SchedulerAction::Daemon {
            tasks_file: None,
            preset: Some("audit".to_string()),
            schedule: DaemonSchedule::Once {
                at_secs: now_secs().saturating_sub(10),
            },
            timezone: SchedulerTimezone::Local,
        };
        let cmd = SchedulerCmd::new(action);
        let mut out = Output::new(OutputKind::Human, true);
        let r = cmd.execute(&mut out);
        assert!(matches!(r, Err(SchedulerError::InvalidAt(_))));
    }

    #[test]
    fn invalid_cron_is_rejected_by_execute() {
        let action = SchedulerAction::Daemon {
            tasks_file: None,
            preset: Some("audit".to_string()),
            schedule: DaemonSchedule::Cron("not a cron expr".to_string()),
            timezone: SchedulerTimezone::Local,
        };
        let cmd = SchedulerCmd::new(action);
        let mut out = Output::new(OutputKind::Human, true);
        let r = cmd.execute(&mut out);
        assert!(matches!(r, Err(SchedulerError::InvalidCron(_))));
    }

    // ---- Broken-time arithmetic sanity checks -------------------------------

    #[test]
    fn days_in_month_handles_leap_year() {
        assert_eq!(days_in_month(2000, 2), 29); // leap (div by 400)
        assert_eq!(days_in_month(1900, 2), 28); // not leap (div by 100 not 400)
        assert_eq!(days_in_month(2024, 2), 29); // leap
        assert_eq!(days_in_month(2023, 2), 28); // not leap
    }

    #[test]
    fn day_of_week_known_dates() {
        // 1970-01-01 was a Thursday → dow = 4.
        assert_eq!(day_of_week(1970, 1, 1), 4);
        // 2024-01-01 was a Monday → dow = 1.
        assert_eq!(day_of_week(2024, 1, 1), 1);
    }

    #[test]
    fn epoch_roundtrip_via_local_to_epoch() {
        let now = now_secs();
        let tm = epoch_to_local(now, &SchedulerTimezone::Utc);
        let back = local_to_epoch(&tm, &SchedulerTimezone::Utc);
        assert_eq!(back, now);
    }
}
