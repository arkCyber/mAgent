//! Hand-rolled CLI argument parser.
//!
//! We avoid pulling in `clap` (or any other arg-parsing crate) on
//! purpose. The CLI surface is small enough that hand-parsing is
//! cheaper than adding a 200 KB transitive dependency, and it makes the
//! CLI's behaviour 100% obvious from one file.
//!
//! ## Usage
//!
//! ```no_run
//! use magent::cli::{Args, Command};
//!
//! let argv: Vec<String> = std::env::args().collect();
//! let args = Args::parse(&argv).unwrap();
//! let _ = match args.command {
//!     Command::Run(opts) => { /* ... */ }
//!     Command::Doctor => { /* ... */ }
//!     Command::Help => { /* ... */ }
//!     Command::Version => { /* ... */ }
//!     // New commands added below as the CLI grows.
//!     _ => {}
//! };
//! ```
//!
//! ## Supported form
//!
//! ```text
//! magent [--version | --help]
//! magent run [OPTIONS] <TASK>
//! magent set-prompt [ACTION] [OPTIONS]
//! magent config [ACTION] [ARGS]
//! magent doctor
//! ```
//!
//! Options recognised on `run`:
//!
//! * `--provider <NAME>` — LLM provider: `ollama` (default) or `deepseek`.
//! * `-m`, `--model <NAME>` — Model name (provider-dependent default).
//! * `-u`, `--ollama <URL>` — Ollama base URL (default `http://localhost:11434`).
//! * `--deepseek-url <URL>` — DeepSeek base URL (default `https://api.deepseek.com/v1`).
//! * `-k`, `--api-key <KEY>` — DeepSeek API key. Falls back to `DEEPSEEK_API_KEY` then `OLLAMA_API_KEY`.
//! * `-i`, `--max-iterations <N>` — Cap the ReAct loop (default 10).
//! * `-t`, `--max-tool-calls <N>` — Cap tool executions (default 8).
//! * `-p`, `--prompt <FILE>` — Load a custom system prompt from a file.
//! * `--prompt-name <NAME>` — Use a prompt stored via `magent set-prompt set <NAME>`. Wins over `--prompt`.
//! * `-q`, `--quiet` — Suppress step-by-step output (final answer only).
//! * `--mock` — Skip the LLM entirely; use canned responses.
//! * `--probe-ollama` — Force a fresh LLM probe on every `run()` call, and
//!   refuse the simulated fallback when it fails. Mutually exclusive with
//!   `--mock`.
//! * `--temperature <F>` — Sampling temperature (default 0.3).
//! * `--num-predict <N>` — LLM `max_tokens` (default 512).
//! * `--max-messages <N>` — Cap the live conversation history (default 32). `0` disables.
//! * `--tool-max-chars <N>` — Cap each tool result's `content` at `N` characters (default 800). `0` disables.
//! * `--json` — Emit a single JSON envelope with the result + stats.
//! * `--no-color` — Disable ANSI colour even on a TTY.
//!
//! ## Provider configuration
//!
//! Ollama is the default. To switch to DeepSeek:
//!
//! ```sh
//! magent run --provider deepseek --api-key sk-... "Your task"
//! # or via env var:
//! DEEPSEEK_API_KEY=sk-... magent run --provider deepseek "Your task"
//! ```

use std::path::PathBuf;

use crate::config;
use crate::prompt;
use crate::scheduler;
use crate::summary;
#[cfg(feature = "web3")]
use crate::web3::{
    PayloadSource, Web3Action, Web3DidOptions, Web3NewOptions, Web3PubkeyOptions,
    Web3SignOptions, Web3VerifyOptions,
};
#[cfg(feature = "blockchain")]
use crate::web3_blockchain::{
    BalanceOptions, BindCreateOptions, BlockchainAction, KeySource, TxBuildOptions, VcIssueOptions,
    VcPresentOptions,
};

// ============================================================================
// Top-level types
// ============================================================================

/// Top-level parsed CLI input.
#[derive(Debug, Clone)]
pub struct Args {
    pub command: Command,
    /// Global flags parsed out of the top-level argv (e.g. `--no-color`,
    /// `--json`). Per-command options live on the `Command` variant.
    pub global: GlobalFlags,
}

/// Flags that apply to every subcommand.
#[derive(Debug, Clone, Default)]
pub struct GlobalFlags {
    /// `--json` — emit the command's *data* as a single JSON envelope on
    /// stdout instead of human prose.
    ///
    /// The contract is "data commands emit parseable JSON on stdout", not
    /// "everything becomes JSON": `help`, the `*-help` texts and the
    /// usage error on stderr are deliberately human-readable, so
    /// `magent --json help` still prints the prose help. Scripts
    /// redirecting stdout should therefore only rely on it for the
    /// commands that document a `--json` shape.
    pub json: bool,
    pub no_color: bool,
    /// `--verbose` / `-v` — turn on debug-level `env_logger` output.
    /// Does not affect the human/JSON trace itself (that's gated by
    /// `--quiet`); it only controls the additional structured log
    /// stream that goes to stderr alongside the labelled trace.
    pub verbose: bool,
    /// `--log-level <LEVEL>` — explicit log level override. Wins
    /// over `RUST_LOG`. Accepted values: `error`, `warn`, `info`,
    /// `debug`, `trace`, `off`. Anything else is rejected at parse
    /// time so users see a clear error instead of silently falling
    /// back to the default.
    pub log_level: Option<String>,
}

/// The first positional argument selects the subcommand.
#[derive(Debug, Clone)]
pub enum Command {
    /// `magent run [OPTIONS] <TASK>` — run a single agent task, or
    /// `magent run --repl` for an interactive session. The CLI requires
    /// an explicit `--repl`; a bare `magent run` with no task is a
    /// usage error.
    Run(Box<RunOptions>),
    /// `magent run --help` / `magent help run` — print `run`-specific usage.
    RunHelp,
    /// `magent set-prompt ...` — manage stored system prompts.
    SetPrompt(prompt::SetPromptAction),
    /// `magent set-prompt --help` / `magent help set-prompt`.
    SetPromptHelp,
    /// `magent summary ...` — manage stored run summaries.
    Summary(summary::SummaryAction),
    /// `magent summary --help` / `magent help summary`.
    SummaryHelp,
    /// `magent config ...` — manage the system config file.
    Config(config::ConfigAction),
    /// `magent config --help` / `magent help config`.
    ConfigHelp,
    /// `magent scheduler ...` — time-triggered auto-runner for audit
    /// and code-completion tasks. Supports `run-once`, `daemon`, and
    /// `status` sub-actions.
    Scheduler(scheduler::SchedulerAction),
    /// `magent scheduler --help` / `magent help scheduler`.
    SchedulerHelp,
    /// `magent web3 ...` — Web3 identity, sign, verify. Gated on the
    /// `web3` feature; the variant is only present when the binary
    /// was built with that feature enabled.
    #[cfg(feature = "web3")]
    Web3(Web3Action),
    /// `magent web3 --help` / `magent help web3`.
    #[cfg(feature = "web3")]
    Web3Help,
    /// `magent doctor` — check Ollama / environment sanity.
    Doctor,
    /// `magent doctor --help` / `magent help doctor` — print `doctor`
    /// usage.
    DoctorHelp,
    /// `magent --help` / `magent help` — print usage.
    Help,
    /// `magent --version` — print version.
    Version,
}

/// Options specific to `magent run`.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct RunOptions {
    /// The user task. Always present — `run` without a task is an error.
    pub task: String,

    /// Which LLM provider to use. `"ollama"` (default, talks to a local
    /// Ollama server) or `"deepseek"` (talks to DeepSeek's hosted API).
    pub provider: String,
    /// Model name. **Empty** (`""`) means "no explicit model — use the
    /// provider's default" (Ollama: `llama3.2`; DeepSeek:
    /// `deepseek-chat`). The CLI uses this empty sentinel to tell
    /// apart "user asked for model X" from "user didn't ask at all",
    /// so it can pick a sensible per-provider default at the last
    /// moment instead of baking one into the parser.
    pub model: String,
    /// Ollama base URL. Only honoured when `provider == "ollama"`.
    pub ollama_url: String,
    /// DeepSeek base URL. Only honoured when `provider == "deepseek"`.
    /// Default: `https://api.deepseek.com/v1`.
    pub deepseek_url: String,
    /// API key for DeepSeek. If left empty, the runner will read
    /// `DEEPSEEK_API_KEY` (or `OLLAMA_API_KEY` for symmetry) from the
    /// environment.
    pub api_key: Option<String>,
    /// Cap on ReAct-loop iterations. `None` → fall through to the
    /// config file (and finally the built-in default).
    pub max_iterations: Option<usize>,
    /// Cap on tool executions. `None` → fall through to the config
    /// file (and finally the built-in default).
    pub max_tool_calls: Option<usize>,
    /// `--timeout <SECS>` — wall-clock budget for the whole run.
    /// `None` → no time bound (only the iteration/tool-call caps
    /// apply). Checked between ReAct steps, not inside an in-flight
    /// request (each client has its own timeout).
    pub timeout_secs: Option<u64>,
    /// Path to a custom system prompt file. `None` → use the default.
    pub prompt_file: Option<PathBuf>,
    /// Name of a prompt stored via `magent set-prompt set <NAME>`.
    /// Resolved at run time by [`crate::prompt::resolve_for_run`].
    /// Wins over `prompt_file` if both are set, so a stored prompt
    /// always takes precedence over a hand-written `.txt` file.
    pub prompt_name: Option<String>,
    /// Suppress step-by-step output (final answer only).
    pub quiet: bool,
    /// Skip LLM entirely.
    pub mock: bool,
    /// Probe LLM on every run, even after the first successful connect.
    /// A failed probe is an error rather than a silent fall back to the
    /// simulated responder, so a session never reports canned text as if
    /// the model had produced it. Rejected together with `--mock`.
    pub probe_ollama: bool,
    /// Sampling temperature. `None` → fall through to the config
    /// file (and finally the built-in default).
    pub temperature: Option<f32>,
    /// Sampling `num_predict`. `None` → fall through to the config
    /// file (and finally the built-in default).
    pub num_predict: Option<usize>,
    /// Max messages kept in the live conversation history. The
    /// system prompt and the original task are always preserved.
    /// `None` → use config / library default.
    pub max_messages: Option<usize>,
    /// Max characters per tool result. Longer results are clipped to
    /// a head + tail window with a marker. `None` → use config /
    /// library default.
    pub tool_max_chars: Option<usize>,
    /// `--save-summary <TOPIC>` — persist the head/tail window
    /// from this run into the summaries store under `<TOPIC>`.
    /// The file lands in the directory chosen by
    /// `MAGENT_SUMMARIES_DIR` (same precedence as the prompt
    /// store). Default `None` → don't persist.
    pub save_summary_topic: Option<String>,
    /// `--save-summary-overwrite` — when `--save-summary` is set,
    /// replace an existing summary of the same name. Default
    /// behaviour is to refuse, so CI runs that retry don't
    /// silently overwrite the previous run's summary.
    pub save_summary_overwrite: bool,
    /// `--load-summary <TOPIC>` — inject the previous run's
    /// `head_tail_window` into `messages` as a system note before
    /// the live system prompt, so the new run can continue from
    /// where the previous one left off.
    pub load_summary_topic: Option<String>,
    /// `--email-tools` — enable email MCP tools by spawning
    /// `magent-email-mcp` as a stdio subprocess. The binary path
    /// is the argument value; if omitted, defaults to
    /// `target/release/magent-email-mcp`.
    pub email_tools: Option<String>,
    /// `--repl` — enter interactive REPL mode. When true, the agent
    /// maintains conversation context across multiple turns.
    pub repl_mode: bool,
    /// `--sign` + `--signer <NAME>` — sign the JSON envelope
    /// produced at the end of the run with the Ed25519 identity
    /// `<NAME>` from the CLI vault. The signed envelope wraps the
    /// regular `RunReport` with a `payload_type` discriminator
    /// (`magent/run_report:v1`) so downstream tools can verify
    /// the run cryptographically. The result is written to a
    /// file (`--signed-output <PATH>`) so it doesn't compete with
    /// `--json` on stdout.
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub sign_with_vault_identity: Option<String>,
    /// `--signed-output <PATH>` — file the signed envelope is
    /// written to. Defaults to `<task-slug>-signed.json` in the
    /// current working directory when `--sign` is in effect.
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub signed_output: Option<PathBuf>,
    /// `--passphrase-env <VAR>` — name of the environment variable
    /// holding the vault passphrase used to decrypt the `--signer`
    /// identity. Defaults to `MAGENT_WEB3_PASSPHRASE`.
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub passphrase_env: Option<String>,
    /// `--not-after <SECS>` — optional expiry window for the
    /// signed envelope. The verifier will reject an envelope whose
    /// `not_after_unix < now`. Helpful for time-boxed
    /// attestations ("this run is valid for the next hour").
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub not_after_unix: Option<u64>,
    /// `--not-before <SECS>` — optional "valid from" window.
    /// Symmetric to `--not-after`. Useful for delayed-attestation
    /// flows.
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub not_before_unix: Option<u64>,
    /// `--verify-signed <PATH>` — instead of running the agent,
    /// just verify the signed envelope at `<PATH>`. Exits 0 on
    /// success, prints the report (human-mode) / envelope (JSON
    /// mode) and exits non-zero on any failure (tamper, expired,
    /// unknown payload type, wrong signer).
    ///
    /// Gated on the `web3_app` feature.
    #[cfg(feature = "web3_app")]
    pub verify_signed_path: Option<PathBuf>,
}

/// Anything that can go wrong while parsing argv.
#[derive(Debug)]
pub enum ParseError {
    /// `--<flag>` was given but no value followed.
    MissingValue(String),
    /// `--<flag> <value>` got a value we couldn't parse as the expected type.
    InvalidValue {
        flag: String,
        value: String,
        expected: String,
    },
    /// `run` was given without a positional task.
    MissingTask,
    /// `run --repl` was given a positional task. The REPL takes its
    /// input interactively, so the task would be silently dropped.
    ReplWithTask,
    /// `--json` was combined with `run --repl`. The REPL streams an
    /// interactive transcript to stdout, which is not a JSON
    /// envelope, so the two are mutually exclusive.
    ReplJsonIncompatible,
    /// `--verify-signed` was combined with `run --repl`. Both are
    /// terminal modes of `run` — verification returns before the REPL
    /// is entered — so `--repl` would be silently dropped.
    #[cfg(feature = "web3_app")]
    ReplVerifySigned,
    /// `--repl` was combined with `--prompt -`. Both consume stdin: the
    /// prompt reader drains it to EOF before the REPL starts, so the
    /// session would immediately see EOF and exit having run nothing.
    ReplPromptStdin,
    /// `--probe-ollama` was combined with `--mock`. One insists the
    /// LLM be reachable before answering; the other refuses to
    /// contact it at all. Honouring either would silently drop the
    /// other, so reject the pair.
    ProbeWithMock,
    /// `run` was given more than one positional argument.
    TooManyPositional { expected: usize, got: usize },
    /// An unrecognised flag was passed.
    UnknownFlag(String),
    /// Any other usage error — a missing positional, an unknown
    /// sub-action, two mutually exclusive flags, a subcommand this
    /// binary was not built with.
    ///
    /// Kept separate from [`ParseError::UnknownFlag`] because the
    /// `Display` impl prefixes that variant with `unknown flag: `,
    /// which turned every usage error into a nonsense sentence:
    /// `unknown flag: scheduler daemon: --interval, --cron, and --at are
    /// mutually exclusive`, or `unknown flag: web3 new: missing <NAME>`.
    /// These messages are already complete sentences and are rendered
    /// verbatim.
    Usage(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingValue(flag) => {
                write!(f, "flag '{}' requires a value", flag)
            }
            ParseError::InvalidValue {
                flag,
                value,
                expected,
            } => {
                write!(
                    f,
                    "invalid value '{}' for flag '{}' (expected {})",
                    value, flag, expected
                )
            }
            ParseError::MissingTask => write!(
                f,
                "'magent run' requires a task — e.g. `magent run \"Read the temperature\"` or `magent run --repl`"
            ),
            ParseError::ReplWithTask => write!(
                f,
                "`magent run --repl` is interactive and takes no task argument — drop the task, or omit --repl to run it once"
            ),
            ParseError::ReplJsonIncompatible => write!(
                f,
                "`magent run --repl` streams an interactive transcript and cannot emit a JSON envelope — drop --json"
            ),
            #[cfg(feature = "web3_app")]
            ParseError::ReplVerifySigned => write!(
                f,
                "`magent run --repl` is interactive and `--verify-signed` only verifies an envelope (the REPL would never start) — pick one"
            ),
            ParseError::ReplPromptStdin => write!(
                f,
                "`--prompt -` reads the system prompt from stdin, which the interactive REPL is also reading — pipe the prompt through a file or `--prompt-name` instead, or drop --repl"
            ),
            ParseError::ProbeWithMock => write!(
                f,
                "`--probe-ollama` requires a reachable LLM while `--mock` never contacts one — drop one of them"
            ),
            ParseError::TooManyPositional { expected, got } => {
                write!(f, "expected {} positional arg(s), got {}", expected, got)
            }
            ParseError::UnknownFlag(s) => write!(f, "unknown flag: {}", s),
            ParseError::Usage(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for ParseError {}

impl Args {
    /// Parse argv (excluding the program name) into an [`Args`].
    pub fn parse(argv: &[String]) -> Result<Self, ParseError> {
        let mut iter = argv.iter();

        // argv[0] is the program name — skip it.
        let _program = iter.next();

        // Global flags we recognise in any position before the subcommand.
        let mut global = GlobalFlags::default();

        // First pass: extract global flags and find the subcommand.
        // We split the iterator so we can hand a fresh slice to the
        // per-subcommand parser without losing the subcommand token.
        let mut positional_first: Option<String> = None;
        let mut after_first: &[String] = &[];
        // Use `while let Some(arg) = iter.next()` instead of
        // `for arg in iter.by_ref()` so we can peek `iter.next()`
        // for the value of `--log-level` without conflicting
        // borrows. The body still matches `arg.as_str()` on
        // every iteration.
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--version" => return Ok(Args { command: Command::Version, global }),
                "-V" => return Ok(Args { command: Command::Version, global }),
                "--help" | "-h" => return Ok(Args { command: Command::Help, global }),
                "--json" => global.json = true,
                "--no-color" => global.no_color = true,
                "--verbose" | "-v" => global.verbose = true,
                "--log-level" => {
                    // `--log-level <LEVEL>` — peek the next token
                    // without consuming the iterator so the second
                    // pass still sees it as a flag name (helps
                    // error messages stay consistent).
                    let next = iter.next().ok_or_else(|| {
                        ParseError::MissingValue("--log-level".to_string())
                    })?;
                    global.log_level = Some(next.clone());
                }
                // Anything else is either a subcommand or a flag we don't
                // recognise yet — leave the rest of argv for the second pass.
                _ => {
                    positional_first = Some(arg.clone());
                    after_first = iter.as_slice();
                    break;
                }
            }
        }

        // Second pass — extract any global flags that the user put
        // *after* the subcommand (e.g. `magent run --json "task"`).
        // Most tools accept this; the previous version didn't, which
        // surprised users. We strip them out of `after_first` so
        // subcommand parsers don't see them and trip on the
        // `unknown flag: --json` path.
        let after_first = extract_global_flags(after_first, &mut global);

        // No subcommand → show help.
        let Some(first) = positional_first else {
            return Ok(Args {
                command: Command::Help,
                global,
            });
        };

        match first.as_str() {
            "run" => {
                let opts = parse_run_args(after_first.iter().peekable())?;
                let command = match opts {
                    RunParseOutcome::Run(opts) => {
                        // The REPL writes an interactive transcript
                        // straight to stdout, bypassing the `Output`
                        // layer, so `--json` would yield prose on
                        // stdout rather than a JSON envelope. Reject
                        // the combination rather than break the
                        // `--json ⇒ valid JSON on stdout` contract.
                        if opts.repl_mode && global.json {
                            return Err(ParseError::ReplJsonIncompatible);
                        }
                        Command::Run(opts)
                    }
                    RunParseOutcome::Help => Command::RunHelp,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "doctor" => {
                // `magent doctor --help` / `magent doctor -h` — same
                // idea as `run --help` but doctor has no options, so we
                // just scan argv for the help flag.
                if after_first.iter().any(|a| a == "--help" || a == "-h") {
                    Ok(Args {
                        command: Command::DoctorHelp,
                        global,
                    })
                } else if after_first.is_empty() {
                    Ok(Args {
                        command: Command::Doctor,
                        global,
                    })
                } else {
                    // Pass any unknown flag through unchanged so the
                    // existing error message ("unknown flag: …") still
                    // fires for typos.
                    Err(ParseError::UnknownFlag(after_first[0].clone()))
                }
            }
            "set-prompt" => {
                // `magent set-prompt set|show|list|delete|export ...`
                // We re-parse everything from after_first here because
                // the sub-action syntax differs per action (positional
                // names, repeatable tags, etc.) and bundling it into
                // the same generic loop as `run` would muddle both.
                let action = parse_set_prompt_args(after_first.iter())?;
                let command = match action {
                    SetPromptParseOutcome::Action(a) => Command::SetPrompt(a),
                    SetPromptParseOutcome::Help => Command::SetPromptHelp,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "config" => {
                // `magent config init|where|show|list|get|set|reset|format ...`
                // Same shape as `set-prompt`: hand-rolled per-action
                // parser so the command surface stays self-documenting.
                let action = parse_config_args(after_first.iter())?;
                let command = match action {
                    ConfigParseOutcome::Action(a) => Command::Config(a),
                    ConfigParseOutcome::Help => Command::ConfigHelp,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "summary" => {
                let action = parse_summary_args(after_first.iter())?;
                let command = match action {
                    SummaryParseOutcome::Action(a) => Command::Summary(a),
                    SummaryParseOutcome::Help => Command::SummaryHelp,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "scheduler" => {
                // `magent scheduler run-once|daemon|status [--tasks-file ...] [--preset ...]`
                let action = parse_scheduler_args(after_first.iter())?;
                let command = match action {
                    SchedulerParseOutcome::Action(a) => Command::Scheduler(a),
                    SchedulerParseOutcome::Help => Command::SchedulerHelp,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            #[cfg(feature = "web3")]
            "web3" => {
                // `magent web3 new|identity|did|pubkey|sign|verify|list|export|delete ...`
                let action = parse_web3_args(after_first.iter())?;
                let command = match action {
                    Web3ParseOutcome::Action(a) => Command::Web3(a),
                    Web3ParseOutcome::Help => Command::Web3Help,
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "help" => {
                // `magent help` → top-level help.
                // `magent help run` → run-specific help.
                // `magent help doctor` → doctor-specific help.
                // `magent help set-prompt` → set-prompt help.
                // `magent help config` → config help.
                // `magent help <anything else>` → unknown subcommand.
                let command = match after_first.first().map(|s| s.as_str()) {
                    None | Some("help") => Command::Help,
                    Some("run") => Command::RunHelp,
                    Some("set-prompt") => Command::SetPromptHelp,
                    Some("config") => Command::ConfigHelp,
                    Some("summary") => Command::SummaryHelp,
                    Some("scheduler") => Command::SchedulerHelp,
                    Some("doctor") => Command::DoctorHelp,
                    #[cfg(feature = "web3")]
                    Some("web3") => Command::Web3Help,
                    // `help <topic>` where the topic is not a subcommand.
                    // Rendering this as an unknown *flag* gave
                    // `unknown flag: help --bogus-xyz`.
                    Some(other) => {
                        return Err(ParseError::Usage(format!(
                            "no help for `{}` — try `help run`, `help set-prompt`, `help summary`, \
                             `help config`, `help scheduler`, or `help doctor`",
                            other
                        )))
                    }
                };
                Ok(Args {
                    command,
                    global,
                })
            }
            "version" => {
                // `version` takes no arguments. Global flags were already
                // stripped by `extract_global_flags`, so anything left here
                // is a typo we should reject rather than silently swallow.
                if let Some(extra) = after_first.first() {
                    return Err(ParseError::UnknownFlag(extra.clone()));
                }
                Ok(Args {
                    command: Command::Version,
                    global,
                })
            }
            // An unrecognised subcommand. This used to be reported as an
            // unknown *flag*, so a build without the `web3` feature
            // answered `magent web3 …` with `unknown flag: web3` — as if
            // `web3` were a mistyped flag rather than a subcommand that
            // this binary does not have.
            other => Err(ParseError::Usage(unknown_subcommand_message(other))),
        }
    }
}

/// Message for a subcommand this binary does not recognise, singling out
/// the ones that exist but are compiled out by a feature flag.
fn unknown_subcommand_message(token: &str) -> String {
    // `web3` / `blockchain` subcommands are gated behind features that are
    // off by default, so "unknown" usually means "not built in".
    #[cfg(not(feature = "web3"))]
    if token == "web3" {
        return format!(
            "unknown subcommand: {token} (this binary was built without the `web3` feature)"
        );
    }
    format!("unknown subcommand: {token} (try `help` for the list of subcommands)")
}

/// Internal return type for `parse_run_args` so we can distinguish
/// "user asked for `run --help`" from "user actually wants to run a
/// task". Without this we'd have to either return a half-built
/// `RunOptions` or error out, both of which made the CLI's help UX
/// confusing.
#[derive(Debug, Clone)]
enum RunParseOutcome {
    /// A real `run` invocation with a task and options.
    Run(Box<RunOptions>),
    /// `magent run --help` / `magent run -h` — caller should print
    /// `run_help_text()` and exit 0.
    Help,
}

/// Strip global flags out of an argv slice and apply them to
/// `global`. Returns a new `Vec<String>` containing only the
/// non-global tokens so the subcommand parser can consume it
/// without tripping on `--json` / `-v` / etc.
///
/// Accepts any of the documented global flags in any position:
/// `--json`, `--no-color`, `-v` / `--verbose`, and
/// `--log-level <LEVEL>`. Unknown flags (typos, subcommand-
/// specific flags like `--mock`) are left in the returned vec
/// so the subcommand parser can produce its normal error.
///
/// The previous behaviour was to reject global flags after the
/// subcommand with `unknown flag: --json`, which surprised users
/// who'd seen other CLIs (cargo, kubectl, …) accept
/// `magent run --json …`. This helper fixes that without
/// weakening the strict per-subcommand flag validation.
fn extract_global_flags(args: &[String], global: &mut GlobalFlags) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => global.json = true,
            "--no-color" => global.no_color = true,
            "-v" | "--verbose" => global.verbose = true,
            "--log-level" => {
                // Same value-peeking policy as the first-pass loop
                // in `Args::parse`: missing value → error so the
                // user gets a clear message instead of a silent
                // fallback. We return the error as a single kept
                // token so the subcommand parser surfaces it.
                match iter.next() {
                    Some(next) => global.log_level = Some(next.clone()),
                    None => kept.push("--log-level".to_string()),
                }
            }
            // Anything else stays in the slice for the subcommand
            // parser. We deliberately keep unknown global-looking
            // flags (e.g. `--json=off` if we ever add it) here so
            // they get the normal `unknown flag` error from the
            // subcommand parser instead of being silently dropped.
            _ => kept.push(arg.clone()),
        }
    }
    kept
}

/// Parse everything after `magent run …` into either a [`RunOptions`]
/// or a `Help` request.
fn parse_run_args<'a>(
    mut iter: std::iter::Peekable<std::slice::Iter<'a, String>>,
) -> Result<RunParseOutcome, ParseError> {
    let mut opts = RunOptions::default();

    /// Consume the next argv token as an option value, rejecting a following
    /// flag token.
    ///
    /// The generic value path used to call `iter.next()` unconditionally, so
    /// `magent run --model --mock "hi"` consumed `--mock` as the model name
    /// and ran against the real backend instead of the simulator. A lone `-`
    /// remains a valid value (stdin convention).
    fn take_value<'a>(
        iter: &mut std::iter::Peekable<std::slice::Iter<'a, String>>,
        flag: &str,
    ) -> Result<&'a String, ParseError> {
        match iter.peek() {
            Some(v) if !v.starts_with('-') || v.as_str() == "-" => iter
                .next()
                .ok_or_else(|| ParseError::MissingValue(flag.to_string())),
            _ => Err(ParseError::MissingValue(flag.to_string())),
        }
    }

    while let Some(arg) = iter.next() {
        // `--` ends option parsing: everything after it is positional, so a
        // task beginning with `-` ("-5 degrees") is representable. Previously
        // `--` was treated as the empty long-flag key.
        if arg == "--" {
            let rest: Vec<&String> = iter.collect();
            if rest.is_empty() {
                break;
            }
            let leading = usize::from(!opts.task.is_empty());
            if leading > 0 || rest.len() > 1 {
                return Err(ParseError::TooManyPositional {
                    expected: 1,
                    got: leading + rest.len(),
                });
            }
            opts.task = rest[0].clone();
            break;
        }

        // Long-form options first so `--mock` doesn't fall through to the
        // `-m` short-option matcher.
        if let Some(value) = arg.strip_prefix("--") {
            // `--key=value` form.
            if let Some((k, v)) = value.split_once('=') {
                if matches!(k, "help" | "h") {
                    return Ok(RunParseOutcome::Help);
                }
                apply_long_flag(k, v, &mut opts)?;
                continue;
            }
            // `--key <value>` form — peek next arg.
            let key = value;
            match key {
                "mock" => opts.mock = true,
                "repl" => opts.repl_mode = true,
                "probe-ollama" => opts.probe_ollama = true,
                "quiet" => opts.quiet = true,
                "save-summary-overwrite" => opts.save_summary_overwrite = true,
                // `--sign` (no value) is shorthand for
                // "sign with the default identity from the
                // vault". The actual identity name is the value
                // the user passes to `--signer`, OR
                // `$MAGENT_AGENT_IDENTITY` if neither flag names
                // one. We use a sentinel `Some("default")` here
                // so the dispatcher's "is signing on?" check is
                // purely a `is_some()`.
                #[cfg(feature = "web3_app")]
                "sign" => opts.sign_with_vault_identity = Some("default".to_string()),
                "email-tools" => {
                    // `--email-tools` or `--email-tools <path>`.
                    // Peek, don't consume: only treat the next token as the
                    // path when it's a non-flag token. Previously we called
                    // `iter.next()` unconditionally, which swallowed a
                    // following flag (e.g. `--email-tools --provider deepseek`
                    // lost `--provider`, making `deepseek` the task and the
                    // real task an extra positional → spurious help output).
                    match iter.peek() {
                        Some(v) if !v.starts_with('-') => {
                            opts.email_tools = Some((*v).clone());
                            iter.next(); // consume the path we just peeked
                        }
                        _ => opts.email_tools = Some(String::new()),
                    }
                }
                "help" => return Ok(RunParseOutcome::Help),
                other => {
                    // Anything else expects a value. Do not swallow a
                    // following flag as that value.
                    let v = take_value(&mut iter, other)?;
                    apply_long_flag(other, v, &mut opts)?;
                }
            }
            continue;
        }

        // Short-form options: `-m <value>` / `-m<value>` / `-h`.
        if let Some(stripped) = arg.strip_prefix('-') {
            if stripped.is_empty() {
                // Bare `-` isn't a flag.
                opts.task.push('-');
                continue;
            }
            // `-h` is the short form of `--help`.
            if stripped == "h" {
                return Ok(RunParseOutcome::Help);
            }
            // `-q` is the short form of `--quiet`. Boolean flags
            // (no value) are handled here so we don't try to swallow
            // the next argv token as their "value".
            if stripped == "q" {
                opts.quiet = true;
                continue;
            }
            // Handle `-m llama3.2` (separate arg) and `-mllama3.2` (attached).
            // Split off the first *character*, not the first byte: `&stripped[..1]`
            // panics with "byte index 1 is not a char boundary" for a
            // multi-byte short flag (`magent run -温…`), turning a bad CLI
            // argument into a crash instead of an "unknown flag" error.
            let key_len = stripped.chars().next().map_or(0, char::len_utf8);
            let key = &stripped[..key_len];
            let rest = &stripped[key_len..];
            if !rest.is_empty() {
                // `-mllama3.2`
                apply_short_flag(key, rest, &mut opts)?;
                continue;
            }
            // `-m <value>` — same flag-token guard as the long form.
            let flag_name = match key {
                "m" => "-m".to_string(),
                "u" => "-u".to_string(),
                "i" => "-i".to_string(),
                "t" => "-t".to_string(),
                "p" => "-p".to_string(),
                "e" => "-e".to_string(),
                _ => arg.to_string(),
            };
            let v = take_value(&mut iter, &flag_name)?;
            apply_short_flag(key, v, &mut opts)?;
            continue;
        }

        // Positional argument: this is the task. Everything after is an error.
        if !opts.task.is_empty() {
            // We already have a task; this is the second positional.
            // Count extras for a friendlier error.
            let mut extra = 1;
            while iter.next().is_some() {
                extra += 1;
            }
            return Err(ParseError::TooManyPositional {
                expected: 1,
                got: 1 + extra,
            });
        }
        opts.task = arg.clone();
    }

    // TRACE: REQ-VFY-001 — empty `task` is rejected unless `--repl` was
    // explicitly set. The previous behaviour ("empty task == REPL mode")
    // was implicit and undocumented; it caused the
    // `rejects_run_with_no_task` test to fail and conflicted with the
    // safety-critical expectation that an explicit task is required
    // before the agent executes any tool call.
    //
    // `--verify-signed <PATH>` is a pure verification path that never
    // executes the agent, so it's exempt from the task requirement.
    //
    // `--repl` with a positional task is rejected rather than silently
    // ignored: `execute()` enters the REPL as soon as `repl_mode` is
    // set, so the task would never run and the user would be none the
    // wiser.
    // `--probe-ollama` demands a live LLM on every run; `--mock` answers
    // from canned responses without contacting one. Whichever won, the
    // other would be silently dropped, so refuse the pair.
    if opts.probe_ollama && opts.mock {
        return Err(ParseError::ProbeWithMock);
    }
    if opts.repl_mode && !opts.task.is_empty() {
        return Err(ParseError::ReplWithTask);
    }
    // `--prompt -` drains stdin to EOF while resolving the system prompt,
    // and the REPL reads the same stdin. The prompt would consume every
    // line the user meant to type, so the session would start at EOF and
    // exit having run nothing — a silent no-op.
    if opts.repl_mode
        && opts
            .prompt_file
            .as_deref()
            .is_some_and(|p| p.as_os_str() == "-")
    {
        return Err(ParseError::ReplPromptStdin);
    }
    #[cfg(feature = "web3_app")]
    if opts.repl_mode && opts.verify_signed_path.is_some() {
        // `execute()` returns from the verify branch *before* it checks
        // `repl_mode`, so the REPL would never start and `--repl` would be
        // dropped without a word. Both are terminal modes of `run`, so
        // refuse the combination rather than pick one silently.
        return Err(ParseError::ReplVerifySigned);
    }
    #[cfg(feature = "web3_app")]
    if opts.verify_signed_path.is_some() && !opts.task.is_empty() {
        // `execute()` returns from the verify branch before the task is
        // looked at, so the task was silently discarded.
        return Err(ParseError::Usage(
            "`--verify-signed <PATH>` verifies an existing envelope and does not run \
             a task — pass either the path or a task, not both"
                .to_string(),
        ));
    }
    #[cfg(feature = "web3_app")]
    if opts.task.is_empty()
        && !opts.repl_mode
        && opts.verify_signed_path.is_none()
    {
        return Err(ParseError::MissingTask);
    }
    #[cfg(not(feature = "web3_app"))]
    if opts.task.is_empty() && !opts.repl_mode {
        return Err(ParseError::MissingTask);
    }
    Ok(RunParseOutcome::Run(Box::new(opts)))
}

/// Internal return type for `parse_set_prompt_args` so we can
/// distinguish "user asked for `--help`" from a real action.
#[derive(Debug, Clone)]
enum SetPromptParseOutcome {
    Action(prompt::SetPromptAction),
    Help,
}

/// Parse everything after `magent set-prompt …` into a
/// [`prompt::SetPromptAction`] (or a Help request).
///
/// The grammar is intentionally narrow — five positional actions,
/// each with its own flag set — because `set-prompt` is a
/// maintenance command, not an interactive loop. Anything weird
/// surfaces as [`ParseError::Usage`] for a consistent error
/// UX with `run` / `doctor`.
fn parse_set_prompt_args<'a, I: Iterator<Item = &'a String>>(
    iter: I,
) -> Result<SetPromptParseOutcome, ParseError> {
    // `--help` / `-h` short-circuit before we look at the action.
    let mut snapshot: Vec<String> = iter.cloned().collect();
    if snapshot.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(SetPromptParseOutcome::Help);
    }

    let action = match snapshot.first().map(|s| s.as_str()) {
        None => {
            // `magent set-prompt` with no args → show help instead of
            // silently doing nothing. Same UX as `git remote` with no
            // args.
            return Ok(SetPromptParseOutcome::Help);
        }
        Some("set") => {
            snapshot.remove(0);
            parse_set_prompt_set(&snapshot)?
        }
        Some("show") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent set-prompt show` requires exactly one <NAME>".to_string(),
                ));
            }
            prompt::SetPromptAction::Show(snapshot[1].clone())
        }
        Some("list") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent set-prompt list` takes no arguments".to_string(),
                ));
            }
            prompt::SetPromptAction::List
        }
        Some("delete") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent set-prompt delete` requires exactly one <NAME>".to_string(),
                ));
            }
            prompt::SetPromptAction::Delete(snapshot[1].clone())
        }
        Some("export") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent set-prompt export` requires exactly one <NAME>".to_string(),
                ));
            }
            prompt::SetPromptAction::Export(snapshot[1].clone())
        }
        Some("import") => {
            // `magent set-prompt import <PATH> [--name <NAME>] [--force]`
            return Ok(SetPromptParseOutcome::Action(
                parse_set_prompt_import(&snapshot[1..])?,
            ));
        }
        Some("template") => {
            // `magent set-prompt template <NAME> [--var KEY=VALUE]… [--vars-from PATH]`
            return Ok(SetPromptParseOutcome::Action(
                parse_set_prompt_template(&snapshot[1..])?,
            ));
        }
        #[cfg(feature = "web3_app")]
        Some("sign") => {
            // `magent set-prompt sign <NAME> [--signer <NAME>] [--signed-output <PATH>]`
            return Ok(SetPromptParseOutcome::Action(
                parse_set_prompt_sign(&snapshot[1..])?,
            ));
        }
        #[cfg(feature = "web3_app")]
        Some("verify-signed") => {
            // `magent set-prompt verify-signed <PATH>`
            return Ok(SetPromptParseOutcome::Action(
                parse_set_prompt_verify_signed(&snapshot[1..])?,
            ));
        }
        Some(other) => {
            return Err(ParseError::Usage(format!(
                "set-prompt {} (try `set`, `show`, `list`, `delete`, `export`, `import`, `template`{})",
                other,
                if cfg!(feature = "web3_app") { ", `sign`, `verify-signed`" } else { "" },
            )));
        }
    };

    Ok(SetPromptParseOutcome::Action(action))
}

/// Internal return type for `parse_summary_args` so we can
/// distinguish "user asked for `summary --help`" from a real
/// action. Mirrors [`SetPromptParseOutcome`] so the two stores
/// feel identical to the dispatcher.
#[derive(Debug, Clone)]
enum SummaryParseOutcome {
    Action(summary::SummaryAction),
    Help,
}

/// Parse everything after `magent summary …` into a
/// [`summary::SummaryAction`] (or a Help request).
///
/// The grammar is intentionally narrow — seven positional
/// actions, each with its own flag set — mirroring the
/// `set-prompt` subcommand. Anything weird surfaces as
/// [`ParseError::Usage`] for a consistent error UX with
/// `run` / `doctor` / `set-prompt`.
fn parse_summary_args<'a, I: Iterator<Item = &'a String>>(
    iter: I,
) -> Result<SummaryParseOutcome, ParseError> {
    // `--help` / `-h` short-circuit before we look at the action.
    let snapshot: Vec<String> = iter.cloned().collect();
    if snapshot.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(SummaryParseOutcome::Help);
    }

    let action = match snapshot.first().map(|s| s.as_str()) {
        None => {
            // `magent summary` with no args → show help.
            return Ok(SummaryParseOutcome::Help);
        }
        Some("save") => {
            return Ok(SummaryParseOutcome::Action(parse_summary_save(
                &snapshot[1..],
            )?));
        }
        Some("show") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent summary show` requires exactly one <TOPIC>".to_string(),
                ));
            }
            summary::SummaryAction::Show(snapshot[1].clone())
        }
        Some("list") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent summary list` takes no arguments".to_string(),
                ));
            }
            summary::SummaryAction::List
        }
        Some("delete") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent summary delete` requires exactly one <TOPIC>".to_string(),
                ));
            }
            summary::SummaryAction::Delete(snapshot[1].clone())
        }
        Some("export") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent summary export` requires exactly one <TOPIC>".to_string(),
                ));
            }
            summary::SummaryAction::Export(snapshot[1].clone())
        }
        Some("load") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent summary load` requires exactly one <TOPIC>".to_string(),
                ));
            }
            summary::SummaryAction::Load(snapshot[1].clone())
        }
        Some("rollback") => {
            return Ok(SummaryParseOutcome::Action(parse_summary_rollback(
                &snapshot[1..],
            )?));
        }
        Some(other) => {
            return Err(ParseError::Usage(format!(
                "summary {} (try `save`, `show`, `list`, `delete`, `export`, `load`, `rollback`)",
                other
            )));
        }
    };

    Ok(SummaryParseOutcome::Action(action))
}

/// Parse the tail of `magent summary save <TOPIC> …` into a
/// [`SummarySaveOptions`].
///
/// `<TOPIC>` is mandatory. Flags are positional-or-named:
/// * `--from <FILE>` — read the JSON record from disk instead of
///   from stdin.
/// * `--overwrite` — replace an existing record of the same name.
///   Default behaviour is to refuse, to prevent accidental CI
///   overwrites.
/// * `--dir <PATH>` — override the default summaries directory
///   for this invocation (mostly for tests / CI).
fn parse_summary_save(args: &[String]) -> Result<summary::SummaryAction, ParseError> {
    let topic = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            return Err(ParseError::Usage(
                "`magent summary save` requires <TOPIC> as the first argument".to_string(),
            ));
        }
    };
    let mut opts = summary::SummarySaveOptions {
        topic,
        from: None,
        overwrite: false,
        dir: None,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                let p = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--from".to_string()))?;
                opts.from = Some(PathBuf::from(p));
                i += 2;
            }
            "--overwrite" => {
                opts.overwrite = true;
                i += 1;
            }
            "--dir" => {
                let p = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--dir".to_string()))?;
                opts.dir = Some(PathBuf::from(p));
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "summary save: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(summary::SummaryAction::Save(opts))
}

/// Parse the tail of `magent summary rollback <TOPIC> <INDEX> …`
/// into a [`SummaryRollbackOptions`].
fn parse_summary_rollback(args: &[String]) -> Result<summary::SummaryAction, ParseError> {
    let topic = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            return Err(ParseError::Usage(
                "`magent summary rollback` requires <TOPIC> as the first argument".to_string(),
            ));
        }
    };
    let index_str = match args.get(1) {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            return Err(ParseError::Usage(
                "`magent summary rollback` requires <INDEX> after <TOPIC>".to_string(),
            ));
        }
    };
    let index: usize = index_str
        .parse()
        .map_err(|_| ParseError::Usage(format!("rollback index {:?}", index_str)))?;
    let mut dir = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                let p = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--dir".to_string()))?;
                dir = Some(PathBuf::from(p));
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "summary rollback: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(summary::SummaryAction::Rollback(summary::SummaryRollbackOptions {
        topic,
        index,
        dir,
    }))
}

// ============================================================================
// `magent web3 …` argument parser
// ============================================================================

/// Internal return type for `parse_web3_args` so we can
/// distinguish "user asked for `web3 --help`" from a real action.
/// Mirrors [`SchedulerParseOutcome`].
#[cfg(feature = "web3")]
#[derive(Debug, Clone)]
enum Web3ParseOutcome {
    Action(Web3Action),
    Help,
}

/// Parse everything after `magent web3 …` into a
/// [`Web3Action`] (or a Help request).
///
/// The grammar is:
///
/// ```text
/// magent web3 new <NAME> [--passphrase-env <VAR>] [--force] [--vault <PATH>]
/// magent web3 identity <NAME>
/// magent web3 did [--from-seed <HEX> | --from-pubkey <HEX>]
/// magent web3 pubkey --from-seed <HEX>
/// magent web3 sign <NAME> [--payload <FILE|->] [--output <FILE>]
///                          [--passphrase-env <VAR>] [--vault <PATH>]
/// magent web3 verify --payload <FILE|-> --envelope <FILE>
/// magent web3 list
/// magent web3 export <NAME>
/// magent web3 delete <NAME>
/// ```
///
/// The parser deliberately rejects unknown flags per-action so a
/// typo (`--passphraze-env`) doesn't silently disable the
/// passphrase prompt.
#[cfg(feature = "web3")]
fn parse_web3_args<'a, I: Iterator<Item = &'a String>>(
    iter: I,
) -> Result<Web3ParseOutcome, ParseError> {
    let snapshot: Vec<String> = iter.cloned().collect();
    if snapshot.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Web3ParseOutcome::Help);
    }
    let Some(first) = snapshot.first().map(|s| s.as_str()) else {
        return Ok(Web3ParseOutcome::Help);
    };

    let action = match first {
        "new" => {
            let name = snapshot
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 new: missing <NAME>".to_string()))?
                .clone();
            let opts = parse_web3_new_flags(&snapshot[2..])?;
            Web3Action::New(Web3NewOptions {
                name,
                passphrase_env: opts.passphrase_env,
                force: opts.force,
                vault_override: opts.vault,
            })
        }
        "identity" => {
            let name = snapshot
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 identity: missing <NAME>".to_string()))?
                .clone();
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 identity: takes no flags beyond <NAME>".to_string(),
                ));
            }
            Web3Action::Identity(name)
        }
        "did" => {
            let opts = parse_web3_did_flags(&snapshot[1..])?;
            Web3Action::Did(opts)
        }
        "pubkey" => {
            let opts = parse_web3_pubkey_flags(&snapshot[1..])?;
            Web3Action::Pubkey(opts)
        }
        "sign" => {
            let name = snapshot
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 sign: missing <NAME>".to_string()))?
                .clone();
            let opts = parse_web3_sign_flags(&snapshot[2..])?;
            Web3Action::Sign(Web3SignOptions {
                name,
                payload: opts.payload,
                output: opts.output,
                passphrase_env: opts.passphrase_env,
                vault_override: opts.vault,
            })
        }
        "verify" => {
            let opts = parse_web3_verify_flags(&snapshot[1..])?;
            Web3Action::Verify(opts)
        }
        "list" => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "web3 list: takes no arguments".to_string(),
                ));
            }
            Web3Action::List
        }
        "export" => {
            let name = snapshot
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 export: missing <NAME>".to_string()))?
                .clone();
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 export: takes no flags beyond <NAME>".to_string(),
                ));
            }
            Web3Action::Export(name)
        }
        "delete" => {
            let name = snapshot
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 delete: missing <NAME>".to_string()))?
                .clone();
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 delete: takes no flags beyond <NAME>".to_string(),
                ));
            }
            Web3Action::Delete(name)
        }
        #[cfg(feature = "blockchain")]
        "chain" => Web3Action::Blockchain(parse_blockchain_chain(&snapshot[1..])?),
        #[cfg(feature = "blockchain")]
        "balance" => Web3Action::Blockchain(parse_blockchain_balance(&snapshot[1..])?),
        #[cfg(feature = "blockchain")]
        "tx" => Web3Action::Blockchain(parse_blockchain_tx(&snapshot[1..])?),
        #[cfg(feature = "blockchain")]
        "bind" => Web3Action::Blockchain(parse_blockchain_bind(&snapshot[1..])?),
        #[cfg(feature = "blockchain")]
        "vc" => Web3Action::Blockchain(parse_blockchain_vc(&snapshot[1..])?),
        other => {
            #[cfg(feature = "blockchain")]
            const EXTRA: &str = ", `chain`, `bind`, `balance`, `tx`, or `vc`";
            #[cfg(not(feature = "blockchain"))]
            const EXTRA: &str = "";
            return Err(ParseError::Usage(format!(
                "web3 {} (try `new`, `identity`, `did`, `pubkey`, `sign`, `verify`, `list`, `export`, or `delete`{})",
                other, EXTRA
            )));
        }
    };

    Ok(Web3ParseOutcome::Action(action))
}

// ----------------------------------------------------------------------------
// `magent web3 chain|bind|balance|tx|vc …` parsing
// ----------------------------------------------------------------------------

/// Read the value that follows a flag, advancing `i` past it.
#[cfg(feature = "blockchain")]
fn take_flag_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, ParseError> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| ParseError::Usage(format!("{flag} requires a value")))
}

/// Reject a flag this action does not know, so a typo never silently does
/// nothing (matching the rest of the `web3` parser).
#[cfg(feature = "blockchain")]
fn unexpected_flag(flag: &str, usage: &str) -> ParseError {
    ParseError::Usage(format!("unknown flag {flag} ({usage})"))
}

#[cfg(feature = "blockchain")]
fn parse_blockchain_chain(args: &[String]) -> Result<BlockchainAction, ParseError> {
    match args.first().map(String::as_str) {
        Some("list") => {
            if args.len() != 1 {
                return Err(ParseError::Usage(
                    "web3 chain list: takes no arguments".to_string(),
                ));
            }
            Ok(BlockchainAction::ChainList)
        }
        Some("info") => {
            let chain = args
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 chain info: missing <CHAIN>".to_string()))?
                .clone();
            if args.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 chain info: takes no flags beyond <CHAIN>".to_string(),
                ));
            }
            Ok(BlockchainAction::ChainInfo(chain))
        }
        _ => Err(ParseError::Usage(
            "web3 chain: expected `list` or `info <CHAIN>`".to_string(),
        )),
    }
}

#[cfg(feature = "blockchain")]
fn parse_blockchain_balance(args: &[String]) -> Result<BlockchainAction, ParseError> {
    let address = args
        .first()
        .ok_or_else(|| ParseError::Usage("web3 balance: missing <ADDRESS>".to_string()))?
        .clone();
    let usage = "usage: web3 balance <ADDRESS> [--chain <CHAIN>]";
    let mut chain = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--chain" => chain = Some(take_flag_value(args, &mut i, "--chain")?),
            other => return Err(unexpected_flag(other, usage)),
        }
        i += 1;
    }
    Ok(BlockchainAction::Balance(BalanceOptions { address, chain }))
}

#[cfg(feature = "blockchain")]
fn parse_blockchain_tx(args: &[String]) -> Result<BlockchainAction, ParseError> {
    let usage = "usage: web3 tx build <TO> --value <ETH> [--data <HEX>] [--chain <CHAIN>]";
    if args.first().map(String::as_str) != Some("build") {
        return Err(ParseError::Usage(
            "web3 tx: expected `build <TO>`".to_string(),
        ));
    }
    let to = args
        .get(1)
        .ok_or_else(|| ParseError::Usage("web3 tx build: missing <TO>".to_string()))?
        .clone();
    let mut value = None;
    let mut data = None;
    let mut chain = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--value" => value = Some(take_flag_value(args, &mut i, "--value")?),
            "--data" => data = Some(take_flag_value(args, &mut i, "--data")?),
            "--chain" => chain = Some(take_flag_value(args, &mut i, "--chain")?),
            other => return Err(unexpected_flag(other, usage)),
        }
        i += 1;
    }
    Ok(BlockchainAction::TxBuild(TxBuildOptions {
        to,
        value,
        data,
        chain: chain.unwrap_or_else(|| "ethereum".to_string()),
    }))
}

#[cfg(feature = "blockchain")]
fn parse_blockchain_bind(args: &[String]) -> Result<BlockchainAction, ParseError> {
    match args.first().map(String::as_str) {
        Some("create") => {
            let usage = "usage: web3 bind create <DID> --chain <CHAIN> --from-seed <HEX> \
                         [--expiry-days <N>] [--output <FILE>]";
            let did = args
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 bind create: missing <DID>".to_string()))?
                .clone();
            let mut chain = None;
            let mut from_seed = None;
            let mut expiry_days = None;
            let mut output = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--chain" => chain = Some(take_flag_value(args, &mut i, "--chain")?),
                    "--from-seed" => {
                        from_seed = Some(take_flag_value(args, &mut i, "--from-seed")?)
                    }
                    "--expiry-days" => {
                        let raw = take_flag_value(args, &mut i, "--expiry-days")?;
                        expiry_days = Some(raw.parse::<u64>().map_err(|_| {
                            ParseError::Usage(format!("--expiry-days {raw:?} is not a number"))
                        })?);
                    }
                    "--output" => output = Some(take_flag_value(args, &mut i, "--output")?.into()),
                    other => return Err(unexpected_flag(other, usage)),
                }
                i += 1;
            }
            let chain = chain.ok_or_else(|| {
                ParseError::Usage("web3 bind create: --chain is required".to_string())
            })?;
            let from_seed = from_seed.ok_or_else(|| {
                ParseError::Usage("web3 bind create: --from-seed is required".to_string())
            })?;
            Ok(BlockchainAction::BindCreate(BindCreateOptions {
                did,
                chain,
                from_seed,
                expiry_days,
                output,
            }))
        }
        Some("verify") => {
            let path = args
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 bind verify: missing <FILE>".to_string()))?
                .into();
            if args.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 bind verify: takes no flags beyond <FILE>".to_string(),
                ));
            }
            Ok(BlockchainAction::BindVerify(path))
        }
        _ => Err(ParseError::Usage(
            "web3 bind: expected `create <DID>` or `verify <FILE>`".to_string(),
        )),
    }
}

/// `--from-seed <HEX>`, `--issuer <NAME>` / `--holder <NAME>`, plus the vault
/// flags a named identity needs. `require` is the vault flag (`--issuer` or
/// `--holder`) so the error message names the right one.
/// Accumulator for the key-source flags (`--from-seed` / a vault identity,
/// plus `--passphrase-env` / `--vault`) shared by `web3 vc issue` and
/// `web3 vc present`.
#[cfg(feature = "blockchain")]
#[derive(Default)]
struct KeySourceFlags {
    seed: Option<String>,
    vault_name: Option<String>,
    passphrase_env: Option<String>,
    vault: Option<PathBuf>,
}

#[cfg(feature = "blockchain")]
impl KeySourceFlags {
    /// Consume the flag at `args[*i]` if it is one of the key-source flags,
    /// or report it as unexpected. `require` is the vault-identity flag name
    /// this subcommand uses (`--issuer` or `--holder`).
    fn consume(
        &mut self,
        args: &[String],
        i: &mut usize,
        require: &str,
        usage: &str,
    ) -> Result<(), ParseError> {
        match args[*i].as_str() {
            "--from-seed" => self.seed = Some(take_flag_value(args, i, "--from-seed")?),
            f if f == require => self.vault_name = Some(take_flag_value(args, i, require)?),
            "--passphrase-env" => {
                self.passphrase_env = Some(take_flag_value(args, i, "--passphrase-env")?)
            }
            "--vault" => self.vault = Some(take_flag_value(args, i, "--vault")?.into()),
            other => return Err(unexpected_flag(other, usage)),
        }
        Ok(())
    }

    /// Resolve the accumulated flags into a [`KeySource`], rejecting the
    /// "neither" and "both" cases with a usage error naming `scope`.
    fn finish(self, scope: &str) -> Result<KeySource, ParseError> {
        match (self.seed, self.vault_name) {
            (Some(hex), None) => Ok(KeySource::Seed(hex)),
            (None, Some(name)) => Ok(KeySource::Vault {
                name,
                passphrase_env: self.passphrase_env,
                vault: self.vault,
            }),
            (Some(_), Some(_)) => Err(ParseError::Usage(format!(
                "{scope}: pass either --from-seed or a vault identity, not both"
            ))),
            (None, None) => Err(ParseError::Usage(format!(
                "{scope}: need --from-seed <HEX> or a vault identity"
            ))),
        }
    }
}

#[cfg(feature = "blockchain")]
fn parse_blockchain_vc(args: &[String]) -> Result<BlockchainAction, ParseError> {
    match args.first().map(String::as_str) {
        Some("issue") => {
            let usage = "usage: web3 vc issue <SUBJECT_DID> --type <TYPE> [--claim <KEY>=<JSON>]... \
                         [--from-seed <HEX> | --issuer <NAME>] [--valid-until <ISO8601>] [--output <FILE>]";
            let subject_did = args
                .get(1)
                .ok_or_else(|| {
                    ParseError::Usage("web3 vc issue: missing <SUBJECT_DID>".to_string())
                })?
                .clone();
            let mut credential_type = None;
            let mut claims = Vec::new();
            let mut valid_until = None;
            let mut output = None;
            let mut key = KeySourceFlags::default();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--type" => credential_type = Some(take_flag_value(args, &mut i, "--type")?),
                    "--claim" => {
                        let raw = take_flag_value(args, &mut i, "--claim")?;
                        claims.push(parse_claim(&raw)?);
                    }
                    "--valid-until" => {
                        valid_until = Some(take_flag_value(args, &mut i, "--valid-until")?)
                    }
                    "--output" => output = Some(take_flag_value(args, &mut i, "--output")?.into()),
                    _ => key.consume(args, &mut i, "--issuer", usage)?,
                }
                i += 1;
            }
            let credential_type = credential_type.ok_or_else(|| {
                ParseError::Usage("web3 vc issue: --type is required".to_string())
            })?;
            let issuer = key.finish("web3 vc issue")?;
            Ok(BlockchainAction::VcIssue(VcIssueOptions {
                subject_did,
                credential_type,
                claims,
                issuer,
                valid_until,
                output,
            }))
        }
        Some("verify") => {
            let path = args
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 vc verify: missing <FILE>".to_string()))?
                .into();
            if args.len() != 2 {
                return Err(ParseError::Usage(
                    "web3 vc verify: takes no flags beyond <FILE>".to_string(),
                ));
            }
            Ok(BlockchainAction::VcVerify(path))
        }
        Some("verify-presentation") => {
            let usage = "usage: web3 vc verify-presentation <FILE> [--challenge <STR>]";
            let path: PathBuf = args
                .get(1)
                .ok_or_else(|| {
                    ParseError::Usage("web3 vc verify-presentation: missing <FILE>".to_string())
                })?
                .into();
            let mut challenge = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--challenge" => {
                        challenge = Some(take_flag_value(args, &mut i, "--challenge")?)
                    }
                    other => {
                        return Err(ParseError::Usage(format!(
                            "{usage} — unexpected argument {other:?}"
                        )))
                    }
                }
                i += 1;
            }
            Ok(BlockchainAction::VcVerifyPresentation { path, challenge })
        }
        Some("present") => {
            let usage = "usage: web3 vc present <FILE> --challenge <STR> \
                         [--from-seed <HEX> | --holder <NAME>] [--domain <D>] [--output <FILE>]";
            let vc_file = args
                .get(1)
                .ok_or_else(|| ParseError::Usage("web3 vc present: missing <FILE>".to_string()))?
                .into();
            let mut challenge = None;
            let mut domain = None;
            let mut output = None;
            let mut key = KeySourceFlags::default();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--challenge" => {
                        challenge = Some(take_flag_value(args, &mut i, "--challenge")?)
                    }
                    "--domain" => domain = Some(take_flag_value(args, &mut i, "--domain")?),
                    "--output" => output = Some(take_flag_value(args, &mut i, "--output")?.into()),
                    _ => key.consume(args, &mut i, "--holder", usage)?,
                }
                i += 1;
            }
            let challenge = challenge.ok_or_else(|| {
                ParseError::Usage("web3 vc present: --challenge is required".to_string())
            })?;
            let holder = key.finish("web3 vc present")?;
            Ok(BlockchainAction::VcPresent(VcPresentOptions {
                vc_file,
                holder,
                challenge,
                domain,
                output,
            }))
        }
        _ => Err(ParseError::Usage(
            "web3 vc: expected `issue`, `verify`, or `present`".to_string(),
        )),
    }
}

/// Parse a `--claim <KEY>=<JSON>` value. The right-hand side is JSON if it
/// parses as such, and a plain string otherwise, so `--claim role=admin` and
/// `--claim age=42` both do the obvious thing.
#[cfg(feature = "blockchain")]
fn parse_claim(raw: &str) -> Result<(String, serde_json::Value), ParseError> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| ParseError::Usage(format!("--claim {raw:?} must be <KEY>=<JSON>")))?;
    if key.is_empty() {
        return Err(ParseError::Usage(format!(
            "--claim {raw:?} has an empty key"
        )));
    }
    let value = serde_json::from_str(value)
        .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
    Ok((key.to_string(), value))
}

#[cfg(feature = "web3")]
#[derive(Debug, Default)]
struct Web3NewFlags {
    passphrase_env: Option<String>,
    force: bool,
    vault: Option<PathBuf>,
}

#[cfg(feature = "web3")]
fn parse_web3_new_flags(args: &[String]) -> Result<Web3NewFlags, ParseError> {
    let mut out = Web3NewFlags::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--passphrase-env" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    ParseError::MissingValue("--passphrase-env".to_string())
                })?;
                out.passphrase_env = Some(v.clone());
                i += 2;
            }
            "--force" => {
                out.force = true;
                i += 1;
            }
            "--vault" => {
                let v = args.get(i + 1).ok_or_else(|| ParseError::MissingValue("--vault".to_string()))?;
                out.vault = Some(PathBuf::from(v));
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "web3 new: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(out)
}

#[cfg(feature = "web3")]
fn parse_web3_did_flags(args: &[String]) -> Result<Web3DidOptions, ParseError> {
    let mut from_seed = None;
    let mut from_pubkey = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from-seed" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--from-seed".to_string()))?;
                from_seed = Some(v.clone());
                i += 2;
            }
            "--from-pubkey" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--from-pubkey".to_string()))?;
                from_pubkey = Some(v.clone());
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "web3 did: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(Web3DidOptions {
        from_seed_hex: from_seed,
        from_pubkey_hex: from_pubkey,
    })
}

#[cfg(feature = "web3")]
fn parse_web3_pubkey_flags(args: &[String]) -> Result<Web3PubkeyOptions, ParseError> {
    let mut from_seed = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from-seed" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--from-seed".to_string()))?;
                from_seed = Some(v.clone());
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "web3 pubkey: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(Web3PubkeyOptions {
        from_seed_hex: from_seed,
    })
}

#[cfg(feature = "web3")]
#[derive(Debug, Default)]
struct Web3SignFlags {
    payload: PayloadSource,
    output: Option<PathBuf>,
    passphrase_env: Option<String>,
    vault: Option<PathBuf>,
}

#[cfg(feature = "web3")]
fn parse_web3_sign_flags(args: &[String]) -> Result<Web3SignFlags, ParseError> {
    // Default payload source is stdin so `cat file | magent web3 sign
    // alice` Just Works. The user can override with `--payload <FILE>`
    // (or `--payload -` to be explicit).
    let mut out = Web3SignFlags {
        payload: PayloadSource::Stdin,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--payload" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--payload".to_string()))?;
                out.payload = if v == "-" {
                    PayloadSource::Stdin
                } else {
                    PayloadSource::File(PathBuf::from(v))
                };
                i += 2;
            }
            "--output" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--output".to_string()))?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            "--passphrase-env" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    ParseError::MissingValue("--passphrase-env".to_string())
                })?;
                out.passphrase_env = Some(v.clone());
                i += 2;
            }
            "--vault" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--vault".to_string()))?;
                out.vault = Some(PathBuf::from(v));
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "web3 sign: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(out)
}

#[cfg(feature = "web3")]
fn parse_web3_verify_flags(args: &[String]) -> Result<Web3VerifyOptions, ParseError> {
    let mut payload: Option<PayloadSource> = None;
    let mut envelope: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--payload" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--payload".to_string()))?;
                payload = Some(if v == "-" {
                    PayloadSource::Stdin
                } else {
                    PayloadSource::File(PathBuf::from(v))
                });
                i += 2;
            }
            "--envelope" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--envelope".to_string()))?;
                envelope = Some(PathBuf::from(v));
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "web3 verify: unknown flag {}",
                    other
                )));
            }
        }
    }
    let payload = payload
        .ok_or_else(|| ParseError::Usage("web3 verify: --payload is required".to_string()))?;
    let envelope = envelope
        .ok_or_else(|| ParseError::Usage("web3 verify: --envelope is required".to_string()))?;
    Ok(Web3VerifyOptions { payload, envelope })
}

// ============================================================================
// `magent scheduler …` argument parser
// ============================================================================

/// Internal return type for `parse_scheduler_args` so we can
/// distinguish "user asked for `scheduler --help`" from a real
/// action. Mirrors [`SummaryParseOutcome`].
#[derive(Debug, Clone)]
enum SchedulerParseOutcome {
    Action(scheduler::SchedulerAction),
    Help,
}

/// Parse everything after `magent scheduler …` into a
/// [`scheduler::SchedulerAction`] (or a Help request).
///
/// The grammar is:
///
/// ```text
/// magent scheduler [run-once | daemon | status]
///                  [--tasks-file <PATH> | --preset <NAME>]
///                  [--interval <SECS>]
/// ```
///
/// `--interval` is only valid on `daemon`. Passing it on `run-once`
/// or `status` is a usage error so the user gets a clear diagnostic
/// instead of silently ignoring the flag.
fn parse_scheduler_args<'a, I: Iterator<Item = &'a String>>(
    iter: I,
) -> Result<SchedulerParseOutcome, ParseError> {
    let snapshot: Vec<String> = iter.cloned().collect();
    if snapshot.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(SchedulerParseOutcome::Help);
    }

    let action = match snapshot.first().map(|s| s.as_str()) {
        None => return Ok(SchedulerParseOutcome::Help),
        Some("status") => {
            // `status` takes no extra args; flag-like tokens would
            // be a typo (`--status`, perhaps) so we let them fall
            // through to the generic unknown-flag path.
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent scheduler status` takes no arguments".to_string(),
                ));
            }
            scheduler::SchedulerAction::Status
        }
        Some("run-once") => {
            let opts = parse_scheduler_source_flags(&snapshot[1..])?;
            // Schedule-selection flags belong to `daemon`. They used to be
            // accepted and dropped, so `run-once --cron "0 0 1 1 *"` ran the
            // queue once and exited as if the schedule had applied.
            // `--timezone` stays accepted (it has no effect on an immediate
            // run, and rejecting it would break existing invocations).
            if opts.interval_secs.is_some() || opts.cron_expr.is_some() || opts.at_rfc3339.is_some()
            {
                return Err(ParseError::Usage(
                    "`magent scheduler run-once` does not take --interval, --cron, or \
                     --at; those select the `daemon` schedule"
                        .to_string(),
                ));
            }
            scheduler::SchedulerAction::RunOnce {
                tasks_file: opts.tasks_file,
                preset: opts.preset,
            }
        }
        Some("daemon") => {
            let opts = parse_scheduler_source_flags(&snapshot[1..])?;
            // Resolve the schedule with mutual-exclusion checks
            // right here in the parser so the executor can assume
            // a single, validated schedule mode.
            let schedule = opts.resolve_schedule()?;
            let timezone = opts.resolve_timezone()?;
            scheduler::SchedulerAction::Daemon {
                tasks_file: opts.tasks_file,
                preset: opts.preset,
                schedule,
                timezone,
            }
        }
        Some(other) => {
            return Err(ParseError::Usage(format!(
                "scheduler {} (try `run-once`, `daemon`, or `status`)",
                other
            )));
        }
    };

    Ok(SchedulerParseOutcome::Action(action))
}

/// Options shared by `run-once` and `daemon`. `daemon` requires exactly one
/// schedule field; `run-once` rejects `--interval`/`--cron`/`--at` because it
/// runs immediately.
#[derive(Debug, Default)]
struct SchedulerSourceFlags {
    tasks_file: Option<PathBuf>,
    preset: Option<String>,
    interval_secs: Option<u64>,
    cron_expr: Option<String>,
    at_rfc3339: Option<String>,
    timezone: Option<String>,
}

impl SchedulerSourceFlags {
    /// Build the daemon's [`scheduler::DaemonSchedule`] from the
    /// collected flags. The parser doesn't know whether we're
    /// parsing for `run-once` or `daemon`, so this is a separate
    /// step the caller invokes after collecting.
    fn resolve_schedule(&self) -> Result<scheduler::DaemonSchedule, ParseError> {
        let mut kinds = 0;
        if self.interval_secs.is_some() {
            kinds += 1;
        }
        if self.cron_expr.is_some() {
            kinds += 1;
        }
        if self.at_rfc3339.is_some() {
            kinds += 1;
        }
        if kinds == 0 {
            // Default to the built-in interval so the existing
            // `magent scheduler daemon --preset audit` keeps
            // working unchanged.
            return Ok(scheduler::DaemonSchedule::Interval {
                secs: scheduler::DEFAULT_INTERVAL_SECS,
            });
        }
        if kinds > 1 {
            return Err(ParseError::Usage(
                "scheduler daemon: --interval, --cron, and --at are \
                 mutually exclusive; pass exactly one"
                    .to_string(),
            ));
        }
        if let Some(secs) = self.interval_secs {
            return Ok(scheduler::DaemonSchedule::Interval { secs });
        }
        if let Some(expr) = self.cron_expr.clone() {
            return Ok(scheduler::DaemonSchedule::Cron(expr));
        }
        if let Some(rfc) = self.at_rfc3339.clone() {
            // Parse the RFC 3339 / ISO 8601 timestamp using a
            // minimal hand-rolled parser — `chrono` is the standard
            // choice but adds ~150 KB of transitive deps. The
            // shape we accept is `YYYY-MM-DDTHH:MM:SS[Z|±HH:MM]`.
            let at_secs = match parse_rfc3339(&rfc) {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("RFC 3339 timestamp ({})", e);
                    return Err(ParseError::InvalidValue {
                        flag: "--at".to_string(),
                        value: rfc.clone(),
                        expected: msg,
                    });
                }
            };
            return Ok(scheduler::DaemonSchedule::Once { at_secs });
        }
        unreachable!("kinds > 0 but no field was set");
    }

    fn resolve_timezone(&self) -> Result<scheduler::SchedulerTimezone, ParseError> {
        match self.timezone.as_deref() {
            None | Some("local") | Some("LOCAL") => Ok(scheduler::SchedulerTimezone::Local),
            Some("utc") | Some("UTC") => Ok(scheduler::SchedulerTimezone::Utc),
            // Anything else used to fall through to `Local`, so a bogus
            // value ran happily and silently moved the cron wall clock.
            // We have no tz database (by policy: no new dependencies), so
            // reject rather than guess.
            Some(other) => Err(ParseError::Usage(format!(
                "scheduler daemon: --timezone expects `utc` or `local`, got `{}`",
                other
            ))),
        }
    }
}

fn parse_scheduler_source_flags(
    args: &[String],
) -> Result<SchedulerSourceFlags, ParseError> {
    let mut opts = SchedulerSourceFlags::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tasks-file" => {
                let p = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--tasks-file".to_string()))?;
                opts.tasks_file = Some(PathBuf::from(p));
                i += 2;
            }
            "--preset" => {
                let n = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--preset".to_string()))?;
                opts.preset = Some(n.clone());
                i += 2;
            }
            "--interval" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--interval".to_string()))?;
                let n: u64 = v.parse().map_err(|_| ParseError::InvalidValue {
                    flag: "--interval".to_string(),
                    value: v.clone(),
                    expected: "integer seconds".to_string(),
                })?;
                opts.interval_secs = Some(n);
                i += 2;
            }
            "--cron" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--cron".to_string()))?;
                opts.cron_expr = Some(v.clone());
                i += 2;
            }
            "--at" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--at".to_string()))?;
                opts.at_rfc3339 = Some(v.clone());
                i += 2;
            }
            "--timezone" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValue("--timezone".to_string()))?;
                opts.timezone = Some(v.clone());
                i += 2;
            }
            other => {
                return Err(ParseError::Usage(format!(
                    "scheduler: unknown flag {}",
                    other
                )));
            }
        }
    }
    Ok(opts)
}

/// Minimal RFC 3339 / ISO 8601 timestamp parser.
///
/// Accepts:
///
/// * `YYYY-MM-DDTHH:MM:SS` (interpreted as UTC; the trailing `Z` is
///   optional but recommended).
/// * `YYYY-MM-DDTHH:MM:SSZ` (UTC).
/// * `YYYY-MM-DDTHH:MM:SS±HH:MM` (numeric offset).
///
/// Returns the Unix-epoch second. Anything else is rejected with a
/// human-readable error so the CLI can show it to the user.
fn parse_rfc3339(s: &str) -> Result<u64, String> {
    // We only handle the "T" separator form. Space-separated forms
    // (`2026-08-11 09:00:00`) are common in casual usage, so we
    // also accept them with a tiny rewrite.
    let normalized = s.replace(' ', "T");
    if normalized.len() < 19 {
        return Err(format!("too short: {:?}", s));
    }
    let bytes = normalized.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {
        return Err(format!("missing field separators in {:?}", s));
    }
    // Every field below is sliced at a fixed *byte* offset. The separator
    // check above pins bytes 4/7/10/13/16 to ASCII, which happens to make
    // `0..=17` boundary-safe — but nothing constrains bytes 17–18, so the
    // seconds field and the `[19..]` tail are not. A multi-byte character
    // there ("2026-08-11T09:00:日") puts `17..19` inside a code point and
    // `&normalized[17..19]` aborts the process with "byte index 19 is not a
    // char boundary". Require the fixed-width prefix to be ASCII up front;
    // that also guarantees 19 is a boundary.
    if !bytes[..19].is_ascii() {
        return Err(format!("non-ASCII date/time fields in {:?}", s));
    }
    let year = slice_int(&normalized[0..4], "year")?;
    let month = slice_int(&normalized[5..7], "month")?;
    let day = slice_int(&normalized[8..10], "day")?;
    let hour = slice_int(&normalized[11..13], "hour")?;
    let minute = slice_int(&normalized[14..16], "minute")?;
    let second = slice_int(&normalized[17..19], "second")?;
    if !(1..=12).contains(&month) {
        return Err(format!("month {} out of range", month));
    }
    if day < 1 || day > days_in_month_pub(year, month as u8) as i32 {
        return Err(format!("day {} out of range for month {}", day, month));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!(
            "time {}:{}:{} out of range",
            hour, minute, second
        ));
    }
    let tail = &normalized[19..];
    let offset_secs: i32 = if tail.is_empty() || tail == "Z" {
        0
    } else if tail.starts_with('+') || tail.starts_with('-') {
        if tail.len() != 6 || tail.as_bytes()[3] != b':' {
            return Err(format!("bad offset {:?}; expected ±HH:MM", tail));
        }
        let sign = if tail.starts_with('-') { -1 } else { 1 };
        let oh = slice_int(&tail[1..3], "offset hour")?;
        let om = slice_int(&tail[4..6], "offset minute")?;
        sign * (oh * 3600 + om * 60)
    } else {
        return Err(format!("unexpected tail {:?}; expected Z or ±HH:MM", tail));
    };

    // Convert (Y, M, D, h, m, s, offset) → epoch seconds. The
    // arithmetic is the same as the `local_to_epoch` helper inside
    // `scheduler.rs`, but duplicated here to avoid a layering
    // dependency (cli.rs doesn't import private items from
    // scheduler.rs).
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap_pub(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month_pub(year, m as u8) as i64;
    }
    days += (day - 1) as i64;
    let secs = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64 - offset_secs as i64;
    if secs < 0 {
        return Err(format!("date {:?} is before 1970", s));
    }
    Ok(secs as u64)
}

fn slice_int(s: &str, label: &str) -> Result<i32, String> {
    s.parse::<i32>()
        .map_err(|_| format!("{} field {:?} is not an integer", label, s))
}

fn days_in_month_pub(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_pub(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn is_leap_pub(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Parse the tail of `magent set-prompt template <NAME> …` into a
/// [`SetPromptTemplateOptions`].
///
/// Positional `NAME` is mandatory. Then walk `--var KEY=VALUE` and
/// `--vars-from PATH` pairs. We accept both `--var KEY=VALUE` (no
/// space) and `--var KEY VALUE` (separate token) to match the
/// `set` sub-action's style.
fn parse_set_prompt_template(args: &[String]) -> Result<prompt::SetPromptAction, ParseError> {
    let name = args
        .first()
        .filter(|s| !s.starts_with("--"))
        .ok_or_else(|| {
            ParseError::Usage(
                "`magent set-prompt template` requires <NAME> as the first argument".to_string(),
            )
        })?
        .clone();
    let mut opts = prompt::SetPromptTemplateOptions {
        name,
        vars: Vec::new(),
        vars_from: None,
    };
    let mut i = 1;
    while i < args.len() {
        let raw = &args[i];
        let (key, inline): (String, Option<String>) = if let Some(rest) = raw.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                // `--var KEY=VALUE` (split at the *first* `=`; values
                // may legitimately contain `=`).
                let (k, v) = rest.split_at(eq);
                (k.to_string(), Some(v[1..].to_string()))
            } else {
                (rest.to_string(), None)
            }
        } else {
            return Err(ParseError::UnknownFlag(raw.clone()));
        };
        let take_inline_or_next = |i: &mut usize,
                                   k: &str,
                                   inline: Option<String>|
         -> Result<String, ParseError> {
            if let Some(v) = inline {
                return Ok(v);
            }
            // Look at the next token, but refuse to swallow a
            // flag as a value. `--var name --vars-from f` should
            // error out (missing value for --var), not silently
            // bind `name` to the literal string "--vars-from".
            let n = args.get(*i + 1).cloned().ok_or_else(|| {
                ParseError::MissingValue(format!("--{}", k))
            })?;
            if n.starts_with("--") {
                return Err(ParseError::MissingValue(format!("--{}", k)));
            }
            *i += 1;
            Ok(n)
        };
        match key.as_str() {
            "var" => {
                let kv = take_inline_or_next(&mut i, "var", inline)?;
                // Expect `KEY=VALUE`. Split at the first `=`.
                let (k, v) = kv.split_once('=').ok_or_else(|| {
                    ParseError::InvalidValue {
                        flag: "--var".to_string(),
                        value: kv.clone(),
                        expected: "KEY=VALUE".to_string(),
                    }
                })?;
                // Empty keys would silently never match in
                // `render_template`; reject loudly so the user
                // fixes the typo.
                if k.is_empty() {
                    return Err(ParseError::InvalidValue {
                        flag: "--var".to_string(),
                        value: kv.clone(),
                        expected: "non-empty KEY=VALUE".to_string(),
                    });
                }
                opts.vars.push((k.to_string(), v.to_string()));
            }
            "vars-from" => {
                opts.vars_from = Some(PathBuf::from(take_inline_or_next(
                    &mut i, "vars-from", inline,
                )?));
            }
            other => {
                return Err(ParseError::UnknownFlag(format!("--{}", other)))
            }
        }
        i += 1;
    }
    Ok(prompt::SetPromptAction::Template(opts))
}

/// Parse the tail of `magent set-prompt import …` into a
/// [`SetPromptImportOptions`].
fn parse_set_prompt_import(args: &[String]) -> Result<prompt::SetPromptAction, ParseError> {
    let first = args.first().ok_or_else(|| {
        ParseError::Usage(
            "`magent set-prompt import` requires <PATH> as the first argument".to_string(),
        )
    })?;
    if first.starts_with("--") {
        return Err(ParseError::Usage(
            "`magent set-prompt import` requires <PATH> as the first argument".to_string(),
        ));
    }
    let mut opts = prompt::SetPromptImportOptions {
        path: PathBuf::from(first),
        name: None,
        force: false,
    };
    // Walk `--key value` pairs after the path.
    let mut i = 1;
    while i < args.len() {
        let raw = &args[i];
        let (key, inline): (String, Option<String>) = if let Some(rest) = raw.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                let (k, v) = rest.split_at(eq);
                (k.to_string(), Some(v[1..].to_string()))
            } else {
                (rest.to_string(), None)
            }
        } else {
            return Err(ParseError::UnknownFlag(raw.clone()));
        };
        let value = |i: &mut usize, k: &str, inline: Option<String>| -> Result<String, ParseError> {
            if let Some(v) = inline { return Ok(v); }
            // Refuse to consume a flag as a value. `--import
            // ./x.json --name` should error (missing value for
            // --name), not silently bind "" to `--name`.
            let n = args.get(*i + 1).cloned().ok_or_else(|| {
                ParseError::MissingValue(format!("--{}", k))
            })?;
            if n.starts_with("--") {
                return Err(ParseError::MissingValue(format!("--{}", k)));
            }
            *i += 1;
            Ok(n)
        };
        match key.as_str() {
            "name" => opts.name = Some(value(&mut i, "name", inline)?),
            "force" => opts.force = true,
            other => {
                return Err(ParseError::UnknownFlag(format!("--{}", other)))
            }
        }
        i += 1;
    }
    Ok(prompt::SetPromptAction::Import(opts))
}

/// Parse `magent set-prompt sign <NAME> [--signer <NAME>] [--signed-output <PATH>]
/// [--passphrase-env <VAR>] [--not-before <UNIX>] [--not-after <UNIX>]`.
///
/// Gated on the `web3_app` feature so the function doesn't exist
/// in non-Web3 builds; the dispatching match in
/// [`parse_set_prompt_args`] is also feature-gated.
#[cfg(feature = "web3_app")]
fn parse_set_prompt_sign(args: &[String]) -> Result<prompt::SetPromptAction, ParseError> {
    let name = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        Some(s) => {
            return Err(ParseError::Usage(format!(
                "`magent set-prompt sign` requires <NAME> as the first argument, got flag {}",
                s
            )))
        }
        None => {
            return Err(ParseError::Usage(
                "`magent set-prompt sign` requires <NAME> as the first argument".to_string(),
            ))
        }
    };
    let mut opts = prompt::SetPromptSignOptions {
        name,
        signer: "default".to_string(),
        signed_output: None,
        passphrase_env: None,
        not_before_unix: None,
        not_after_unix: None,
    };
    let mut i = 1;
    while i < args.len() {
        let raw = &args[i];
        let (key, inline): (String, Option<String>) = if let Some(rest) = raw.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                let (k, v) = rest.split_at(eq);
                (k.to_string(), Some(v[1..].to_string()))
            } else {
                (rest.to_string(), None)
            }
        } else {
            return Err(ParseError::UnknownFlag(raw.clone()));
        };
        let value = |i: &mut usize, k: &str, inline: Option<String>| -> Result<String, ParseError> {
            if let Some(v) = inline {
                return Ok(v);
            }
            let n = args.get(*i + 1).cloned().ok_or_else(|| {
                ParseError::MissingValue(format!("--{}", k))
            })?;
            if n.starts_with("--") {
                return Err(ParseError::MissingValue(format!("--{}", k)));
            }
            *i += 1;
            Ok(n)
        };
        match key.as_str() {
            "signer" => opts.signer = value(&mut i, "signer", inline)?,
            "signed-output" => {
                opts.signed_output = Some(PathBuf::from(value(&mut i, "signed-output", inline)?))
            }
            "passphrase-env" => {
                opts.passphrase_env = Some(value(&mut i, "passphrase-env", inline)?)
            }
            "not-before" => {
                let raw = value(&mut i, "not-before", inline)?;
                let n: usize = raw.parse().map_err(|_| ParseError::InvalidValue {
                    flag: "--not-before".to_string(),
                    value: raw.clone(),
                    expected: "u64".to_string(),
                })?;
                opts.not_before_unix = Some(u64::try_from(n).map_err(|_| {
                    ParseError::InvalidValue {
                        flag: "--not-before".to_string(),
                        value: raw.clone(),
                        expected: "u64".to_string(),
                    }
                })?);
            }
            "not-after" => {
                let raw = value(&mut i, "not-after", inline)?;
                let n: usize = raw.parse().map_err(|_| ParseError::InvalidValue {
                    flag: "--not-after".to_string(),
                    value: raw.clone(),
                    expected: "u64".to_string(),
                })?;
                opts.not_after_unix = Some(u64::try_from(n).map_err(|_| {
                    ParseError::InvalidValue {
                        flag: "--not-after".to_string(),
                        value: raw.clone(),
                        expected: "u64".to_string(),
                    }
                })?);
            }
            other => {
                return Err(ParseError::UnknownFlag(format!("--{}", other)))
            }
        }
        i += 1;
    }
    Ok(prompt::SetPromptAction::Sign(opts))
}

/// Parse `magent set-prompt verify-signed <PATH>`.
///
/// Just consumes one positional arg; no flags.
#[cfg(feature = "web3_app")]
fn parse_set_prompt_verify_signed(
    args: &[String],
) -> Result<prompt::SetPromptAction, ParseError> {
    let path = match args.first() {
        Some(s) if !s.starts_with("--") => PathBuf::from(s),
        _ => {
            return Err(ParseError::Usage(
                "`magent set-prompt verify-signed` requires <PATH> as the first argument".to_string(),
            ))
        }
    };
    if args.len() > 1 {
        return Err(ParseError::Usage(
            "`magent set-prompt verify-signed` takes a single <PATH> argument".to_string(),
        ));
    }
    Ok(prompt::SetPromptAction::VerifySigned(prompt::SetPromptVerifySignedOptions {
        path,
    }))
}

/// Parse `magent set-prompt set <NAME> --prompt <BODY> [--provider …] [--model …] [--tag …] …`.
fn parse_set_prompt_set(args: &[String]) -> Result<prompt::SetPromptAction, ParseError> {
    // First positional is the name.
    let name = match args.first() {
        Some(n) if !n.starts_with("--") => n.clone(),
        _ => {
            return Err(ParseError::Usage(
                "`magent set-prompt set` requires <NAME> as the first argument".to_string(),
            ));
        }
    };
    let mut opts = prompt::SetPromptSetOptions {
        name,
        prompt: String::new(),
        provider: None,
        model: None,
        description: None,
        author: None,
        tags: Vec::new(),
    };

    // Walk the remaining args as `--key value` or `--key=value` pairs.
    // We do this by taking two args at a time so the bookkeeping is
    // straightforward; single-arg `--tag` (peek) is a special case.
    let mut i = 1;
    while i < args.len() {
        let raw = &args[i];
        // Split off `--key=value` vs `--key` vs `--key value`.
        let (key, inline_value): (String, Option<String>) = if let Some(rest) = raw.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                let (k, v) = rest.split_at(eq);
                (k.to_string(), Some(v[1..].to_string()))
            } else {
                (rest.to_string(), None)
            }
        } else {
            return Err(ParseError::UnknownFlag(raw.clone()));
        };

        let take_next = |i: &mut usize, key: &str, inline: Option<String>| -> Result<Option<String>, ParseError> {
            if let Some(v) = inline {
                return Ok(Some(v));
            }
            // Refuse to consume a flag as a value. `magent set-prompt
            // set my-prompt --prompt --model foo` should fail with
            // a missing-value for `--prompt`, not silently bind
            // the literal string "--model" to the prompt body.
            let next = args.get(*i + 1).cloned();
            if next.is_none() {
                return Err(ParseError::MissingValue(format!("--{}", key)));
            }
            if next.as_deref().map(|s| s.starts_with("--")).unwrap_or(false) {
                return Err(ParseError::MissingValue(format!("--{}", key)));
            }
            *i += 1;
            Ok(next)
        };

        match key.as_str() {
            "prompt" => {
                let v = take_next(&mut i, "prompt", inline_value)?;
                opts.prompt = v.unwrap_or_default();
            }
            "provider" => {
                let v = take_next(&mut i, "provider", inline_value)?;
                opts.provider = v;
            }
            "model" => {
                let v = take_next(&mut i, "model", inline_value)?;
                opts.model = v;
            }
            "description" => {
                let v = take_next(&mut i, "description", inline_value)?;
                opts.description = v;
            }
            "author" => {
                let v = take_next(&mut i, "author", inline_value)?;
                opts.author = v;
            }
            "tag" => {
                // `--tag foo` (peek next) or `--tag=foo` (inline).
                let v = take_next(&mut i, "tag", inline_value)?;
                if let Some(v) = v {
                    if !v.is_empty() {
                        opts.tags.push(v);
                    }
                }
            }
            _ => return Err(ParseError::UnknownFlag(format!("--{}", key))),
        }
        i += 1;
    }

    if opts.prompt.is_empty() {
        return Err(ParseError::MissingValue("--prompt".to_string()));
    }

    Ok(prompt::SetPromptAction::Set(opts))
}

/// Internal return type for `parse_config_args`.
#[derive(Debug, Clone)]
enum ConfigParseOutcome {
    Action(config::ConfigAction),
    Help,
}

/// Parse `magent config …` into a [`config::ConfigAction`] (or Help).
fn parse_config_args<'a, I: Iterator<Item = &'a String>>(
    iter: I,
) -> Result<ConfigParseOutcome, ParseError> {
    // `--help` / `-h` short-circuits.
    let snapshot: Vec<String> = iter.cloned().collect();
    if snapshot.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(ConfigParseOutcome::Help);
    }

    let action = match snapshot.first().map(|s| s.as_str()) {
        None => {
            // `magent config` with no args → show help. Same UX as
            // `set-prompt` and `git remote`.
            return Ok(ConfigParseOutcome::Help);
        }
        Some("init") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config init` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Init
        }
        Some("where") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config where` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Where
        }
        Some("show") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config show` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Show
        }
        Some("list") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config list` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::List
        }
        Some("format") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config format` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Format
        }
        Some("validate") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config validate` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Validate
        }
        Some("export") => {
            if snapshot.len() != 1 {
                return Err(ParseError::Usage(
                    "`magent config export` takes no arguments".to_string(),
                ));
            }
            config::ConfigAction::Export
        }
        Some("get") => {
            if snapshot.len() != 2 {
                return Err(ParseError::Usage(
                    "`magent config get` requires exactly one <KEY>".to_string(),
                ));
            }
            config::ConfigAction::Get(snapshot[1].clone())
        }
        Some("set") => {
            if snapshot.len() != 3 {
                return Err(ParseError::Usage(
                    "`magent config set` requires <KEY> <VALUE>".to_string(),
                ));
            }
            config::ConfigAction::Set {
                key: snapshot[1].clone(),
                value: snapshot[2].clone(),
            }
        }
        Some("reset") => {
            // `reset [--yes]` is the only sub-action that takes an
            // optional flag.
            let yes = if snapshot.len() == 2 && snapshot[1] == "--yes" {
                true
            } else if snapshot.len() == 1 {
                false
            } else {
                return Err(ParseError::Usage(
                    "`magent config reset` takes only `--yes`".to_string(),
                ));
            };
            config::ConfigAction::Reset { yes }
        }
        Some(other) => {
            return Err(ParseError::Usage(format!(
                "config {} (try `init`, `where`, `show`, `list`, `get`, `set`, `reset`, `validate`, `format`)",
                other
            )));
        }
    };

    Ok(ConfigParseOutcome::Action(action))
}

fn apply_long_flag(key: &str, value: &str, opts: &mut RunOptions) -> Result<(), ParseError> {
    match key {
        "provider" => {
            match value {
                "ollama" | "deepseek" => opts.provider = value.to_string(),
                _ => {
                    return Err(ParseError::InvalidValue {
                        flag: "--provider".to_string(),
                        value: value.to_string(),
                        expected: "`ollama` or `deepseek`".to_string(),
                    })
                }
            }
        }
        "model" => opts.model = value.to_string(),
        "ollama" => opts.ollama_url = value.to_string(),
        "deepseek-url" | "deepseek" => opts.deepseek_url = value.to_string(),
        "api-key" => opts.api_key = Some(value.to_string()),
        "max-iterations" => {
            opts.max_iterations = Some(parse_usize_nonzero(key, value)?);
        }
        "max-tool-calls" => {
            opts.max_tool_calls = Some(parse_usize_nonzero(key, value)?);
        }
        "timeout" => {
            // Seconds. `0` would mean "expire before the first step", which
            // is never what a user wants, so reuse the non-zero rule.
            opts.timeout_secs = Some(
                u64::try_from(parse_usize_nonzero(key, value)?).map_err(|_| {
                    ParseError::InvalidValue {
                        flag: "--timeout".to_string(),
                        value: value.to_string(),
                        expected: "a positive number of seconds (>= 1)".to_string(),
                    }
                })?,
            );
        }
        "max-messages" => {
            opts.max_messages = Some(parse_usize(key, value)?);
        }
        "tool-max-chars" => {
            opts.tool_max_chars = Some(parse_usize(key, value)?);
        }
        "prompt" => opts.prompt_file = Some(PathBuf::from(value)),
        "prompt-name" => opts.prompt_name = Some(value.to_string()),
        "save-summary" => opts.save_summary_topic = Some(value.to_string()),
        "load-summary" => opts.load_summary_topic = Some(value.to_string()),
        "email-tools" => opts.email_tools = Some(value.to_string()),
        "temperature" => {
            opts.temperature = Some(parse_f32(key, value)?);
        }
        "num-predict" => {
            opts.num_predict = Some(parse_usize(key, value)?);
        }
        // --- web3_app-signed-envelope flags ------------------------
        // `--sign` + `--signer <NAME>` together turn on signed-
        // envelope emission at the end of the run. We accept either
        // flag as a trigger; once `--sign` is set, `--signer <NAME>`
        // supplies the identity from the vault. The parser keeps
        // the two independent so a future refactor can lift
        // `--sign` to also accept an inline DID (`--sign <DID>`).
        #[cfg(feature = "web3_app")]
        "sign" => opts.sign_with_vault_identity = Some(value.to_string()),
        #[cfg(feature = "web3_app")]
        "signer" => opts.sign_with_vault_identity = Some(value.to_string()),
        #[cfg(feature = "web3_app")]
        "signed-output" => opts.signed_output = Some(PathBuf::from(value)),
        #[cfg(feature = "web3_app")]
        "passphrase-env" => opts.passphrase_env = Some(value.to_string()),
        #[cfg(feature = "web3_app")]
        "not-after" => {
            let v = parse_usize(key, value)?;
            opts.not_after_unix = Some(u64::try_from(v).map_err(|_| {
                ParseError::InvalidValue {
                    flag: "--not-after".to_string(),
                    value: value.to_string(),
                    expected: "non-negative unix-seconds".to_string(),
                }
            })?)
        }
        #[cfg(feature = "web3_app")]
        "not-before" => {
            let v = parse_usize(key, value)?;
            opts.not_before_unix = Some(u64::try_from(v).map_err(|_| {
                ParseError::InvalidValue {
                    flag: "--not-before".to_string(),
                    value: value.to_string(),
                    expected: "non-negative unix-seconds".to_string(),
                }
            })?)
        }
        #[cfg(feature = "web3_app")]
        "verify-signed" => opts.verify_signed_path = Some(PathBuf::from(value)),
        _ => return Err(ParseError::UnknownFlag(format!("--{}", key))),
    }
    Ok(())
}

/// Short flag → field mapper. Mirrors the long flags in
/// [`apply_long_flag`]; every short option advertised in
/// [`run_help_text`] must appear here or the user gets an unhelpful
/// `unknown flag: -q` error.
fn apply_short_flag(key: &str, value: &str, opts: &mut RunOptions) -> Result<(), ParseError> {
    match key {
        "m" => opts.model = value.to_string(),
        "u" => opts.ollama_url = value.to_string(),
        "k" => opts.api_key = Some(value.to_string()),
        "i" => opts.max_iterations = Some(parse_usize_nonzero("-i", value)?),
        "t" => opts.max_tool_calls = Some(parse_usize_nonzero("-t", value)?),
        "p" => opts.prompt_file = Some(PathBuf::from(value)),
        "e" => opts.email_tools = Some(value.to_string()),
        _ => return Err(ParseError::UnknownFlag(format!("-{}", key))),
    }
    Ok(())
}

fn parse_usize(flag: &str, value: &str) -> Result<usize, ParseError> {
    value.parse::<usize>().map_err(|_| ParseError::InvalidValue {
        flag: flag.to_string(),
        value: value.to_string(),
        expected: "a positive integer".to_string(),
    })
}

/// Like [`parse_usize`] but rejects `0`.
///
/// The ReAct runner tests its iteration cap *before* the first step
/// (`iteration >= max_iterations`), so a cap of `0` permits zero steps
/// and every run ends with "run ended without a final answer". The
/// runtime has no "0 = no cap" sentinel (the core `MiniAgent` rejects
/// `max_iterations == 0` outright for the same reason), so `-i 0` /
/// `-t 0` are rejected here with a clear message rather than silently
/// producing a guaranteed-failing run. `0 = off` remains valid only for
/// `--max-messages` / `--tool-max-chars`, which keep [`parse_usize`].
fn parse_usize_nonzero(flag: &str, value: &str) -> Result<usize, ParseError> {
    let n = parse_usize(flag, value)?;
    if n == 0 {
        return Err(ParseError::InvalidValue {
            flag: flag.to_string(),
            value: value.to_string(),
            expected: "a positive integer (>= 1)".to_string(),
        });
    }
    Ok(n)
}

fn parse_f32(flag: &str, value: &str) -> Result<f32, ParseError> {
    let parsed = value.parse::<f32>().map_err(|_| ParseError::InvalidValue {
        flag: flag.to_string(),
        value: value.to_string(),
        expected: "a floating-point number".to_string(),
    })?;
    // `"nan"` / `"inf"` parse as f32 but are not representable in JSON; the
    // core request builder's hand-written encoder would emit the literal
    // `NaN`, which every OpenAI-compatible provider rejects. Reject at the
    // CLI boundary.
    if !parsed.is_finite() {
        return Err(ParseError::InvalidValue {
            flag: flag.to_string(),
            value: value.to_string(),
            expected: "a finite floating-point number".to_string(),
        });
    }
    Ok(parsed)
}

/// Returns the help text the CLI prints when `--help` or no subcommand
/// is given. We don't render it inside `cli.rs` because the help text
/// belongs in `main.rs` (where the version string lives too). This
/// function is just here so the unit tests have a single source of
/// truth for what "the help message" looks like.
pub fn help_text(version: &str) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent {}", version);
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent [GLOBAL FLAGS] <SUBCOMMAND> [OPTIONS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "SUBCOMMANDS:");
    let _ = writeln!(s, "    run             Run an agent task (the headline feature)");
    let _ = writeln!(s, "    set-prompt      Manage stored system prompts (JSON)");
    let _ = writeln!(s, "    summary         Manage stored run summaries (head/tail window + LLM note)");
    let _ = writeln!(s, "    config          Manage the system config file (JSON)");
    let _ = writeln!(
        s,
        "    scheduler       Time-triggered auto-runner (audit + code-completion)"
    );
    #[cfg(feature = "web3")]
    let _ = writeln!(
        s,
        "    web3            Ed25519 identity / sign / verify (encrypted vault)"
    );
    let _ = writeln!(
        s,
        "    doctor          Check LLM backend reachability and tool backend"
    );
    let _ = writeln!(s, "    help            Print this message");
    let _ = writeln!(s);
    let _ = writeln!(s, "GLOBAL FLAGS:");
    let _ = writeln!(s, "    --json       Emit a single JSON envelope with the result");
    let _ = writeln!(s, "    --no-color   Disable ANSI colour on stderr");
    let _ = writeln!(
        s,
        "    -v, --verbose  Enable debug-level logs (via env_logger)"
    );
    let _ = writeln!(
        s,
        "    --log-level <LEVEL>  Set log level (error|warn|info|debug|trace|off)"
    );
    let _ = writeln!(s, "    -h, --help   Print this message");
    let _ = writeln!(s, "    -V, --version");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "ENV:"
    );
    let _ = writeln!(
        s,
        "    RUST_LOG     Standard env_logger filter (honoured when"
    );
    let _ = writeln!(
        s,
        "                 --log-level is not set)."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "LLM PROVIDERS:"
    );
    let _ = writeln!(
        s,
        "    By default magent talks to a local Ollama server on port"
    );
    let _ = writeln!(
        s,
        "    11434. Pass --provider deepseek to talk to DeepSeek's"
    );
    let _ = writeln!(
        s,
        "    hosted API instead (requires an API key, via --api-key or"
    );
    let _ = writeln!(
        s,
        "    the DEEPSEEK_API_KEY environment variable)."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "EXIT CODES:");
    let _ = writeln!(
        s,
        "    0    Success (for `scheduler run-once`, every task succeeded)."
    );
    let _ = writeln!(
        s,
        "    1    The command ran and failed — a failed agent task, a"
    );
    let _ = writeln!(
        s,
        "         backend error, or a `scheduler run-once` where at"
    );
    let _ = writeln!(
        s,
        "         least one task failed. With --json, stdout still holds"
    );
    let _ = writeln!(
        s,
        "         an {{\"ok\":false,\"error\":…,\"code\":…}} envelope."
    );
    let _ = writeln!(
        s,
        "    2    Your input was rejected — a bad flag, a missing file,"
    );
    let _ = writeln!(s, "         or a corrupt config. Nothing ran.");
    let _ = writeln!(
        s,
        "    101  Internal error (a bug in magent). Report it; the"
    );
    let _ = writeln!(
        s,
        "         message names the location and diagnoses itself as a bug."
    );
    let _ = writeln!(
        s,
        "    130  Interrupted (SIGINT) — a clean `scheduler daemon` stop."
    );
    let _ = writeln!(
        s,
        "    141  SIGPIPE — the reader went away (`magent … | head`), the"
    );
    let _ = writeln!(s, "         same as any other Unix filter. Not an error.");
    let _ = writeln!(s);
    let _ = writeln!(s, "See `magent run --help` for run-specific options.");
    s
}

/// Help text for the `magent run` subcommand. This is shown when the
/// user passes `--help` to `run` (or when they make a parsing error).
pub fn run_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent run");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent run [OPTIONS] <TASK>    Run a single task");
    let _ = writeln!(s, "    magent run --repl [OPTIONS]   Interactive REPL mode (multi-turn)");
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS:");
    let _ = writeln!(s, "    --provider <NAME>          LLM provider: ollama (default) | deepseek");
    let _ = writeln!(s, "    -m, --model <NAME>         Model name (provider default if empty)");
    let _ = writeln!(s, "    -u, --ollama <URL>         Ollama base URL");
    let _ = writeln!(s, "    --deepseek-url <URL>       DeepSeek base URL");
    let _ = writeln!(s, "    -k, --api-key <KEY>        DeepSeek API key (or DEEPSEEK_API_KEY env)");
    let _ = writeln!(s, "    -i, --max-iterations <N>   Cap the ReAct loop (default 10)");
    let _ = writeln!(s, "    -t, --max-tool-calls <N>   Cap tool executions (default 8)");
    let _ = writeln!(
        s,
        "    --timeout <SECS>           Wall-clock budget per run/turn (default off)"
    );
    let _ = writeln!(s, "    --max-messages <N>         Cap conversation history (default 32, 0=off)");
    let _ = writeln!(
        s,
        "    --tool-max-chars <N>       Cap each tool result content at N characters \
         (default 800, 0=off)"
    );
    let _ = writeln!(s, "    -p, --prompt <FILE>        Load a custom system prompt from a file");
    let _ = writeln!(s, "    --prompt-name <NAME>      Use a prompt stored via `set-prompt` (wins over --prompt)");
    let _ = writeln!(s, "    -q, --quiet                Suppress step-by-step output");
    let _ = writeln!(s, "    --mock                     Skip the LLM entirely (canned responses)");
    let _ = writeln!(s, "    --repl                     Interactive REPL (multi-turn; takes no <TASK>, rejects --json / --verify-signed)");
    let _ = writeln!(s, "    --probe-ollama             Probe LLM on every run() call; error if unreachable (incompatible with --mock)");
    let _ = writeln!(s, "    --temperature <F>          Sampling temperature (default 0.3)");
    let _ = writeln!(s, "    --num-predict <N>          LLM max_tokens (default 512)");
    let _ = writeln!(
        s,
        "    --save-summary <TOPIC>     Persist the head/tail window + LLM summary to the store"
    );
    let _ = writeln!(
        s,
        "    --save-summary-overwrite   When --save-summary is set, replace an existing record"
    );
    let _ = writeln!(
        s,
        "    --load-summary <TOPIC>     Inject a previous summary's window as a system note"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "WEB3 SIGNED ENVELOPES (requires --features web3_app):");
    let _ = writeln!(
        s,
        "    --sign                     Sign the run report with the agent's vault identity."
    );
    let _ = writeln!(
        s,
        "    --signer <NAME>            Vault identity name (env MAGENT_AGENT_IDENTITY)"
    );
    let _ = writeln!(
        s,
        "    --signed-output <PATH>     Where to write the signed envelope JSON"
    );
    let _ = writeln!(
        s,
        "    --passphrase-env <VAR>     Vault passphrase env var (default: MAGENT_WEB3_PASSPHRASE)"
    );
    let _ = writeln!(
        s,
        "    --not-after <UNIX-SECS>    Optional expiry window for the signed envelope"
    );
    let _ = writeln!(
        s,
        "    --not-before <UNIX-SECS>   Optional 'valid-from' window"
    );
    let _ = writeln!(
        s,
        "    --verify-signed <PATH>     Verify an existing signed envelope (skips run)"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "TOOL BACKENDS:");
    let _ = writeln!(
        s,
        "    --email-tools [PATH]       Enable email tools (IMAP/SMTP) via magent-email-mcp"
    );
    let _ = writeln!(
        s,
        "                              subprocess. PATH resolves via $PATH; empty = default"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "CONTEXT MANAGEMENT:");
    let _ = writeln!(s, "    Long sessions quickly blow through the model's context window.");
    let _ = writeln!(s, "    --max-messages / --tool-max-chars keep the live payload bounded by");
    let _ = writeln!(s, "    (a) dropping the oldest messages, keeping the system prompt and the");
    let _ = writeln!(s, "    original task, and (b) clipping oversized tool results to a head +");
    let _ = writeln!(s, "    tail window with a marker. See docs/CONTEXT_MANAGEMENT.md.");
    s
}

/// Help text for the `magent set-prompt` subcommand. Shown when the
/// user runs `magent set-prompt --help` or `magent help set-prompt`.
pub fn set_prompt_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent set-prompt");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent set-prompt <ACTION> [OPTIONS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "ACTIONS:");
    let _ = writeln!(s, "    set    <NAME> --prompt <TEXT|FILE> [--provider ...] [--model ...]");
    let _ = writeln!(s, "                  [--description ...] [--author ...] [--tag ...]");
    let _ = writeln!(s, "    show   <NAME>             Print the JSON record (pretty)");
    let _ = writeln!(s, "    list                       List every stored prompt");
    let _ = writeln!(s, "    delete <NAME>             Remove the file (no-op if missing)");
    let _ = writeln!(s, "    export <NAME>             Print just the prompt text (pipeable; full record under --json)");
    let _ = writeln!(s, "    import <PATH>             Read a JSON file and write it to the store");
    let _ = writeln!(s, "        [--name <NAME>]      Override the name field from the JSON");
    let _ = writeln!(s, "        [--force]            Overwrite an existing prompt with the same name");
    let _ = writeln!(s, "    template <NAME>          Render a stored prompt with {{{{VAR}}}} placeholders");
    let _ = writeln!(s, "        [--var KEY=VALUE]    Bind a variable (repeatable; later wins)");
    let _ = writeln!(s, "        [--vars-from PATH]   Load variable bindings from a JSON object");
    #[cfg(feature = "web3_app")]
    {
        let _ = writeln!(s, "    sign <NAME>             Sign the prompt with a vault identity");
        let _ = writeln!(s, "        [--signer <NAME>]        Vault identity name (default: \"default\")");
        let _ = writeln!(s, "        [--signed-output <PATH>] Output envelope path (default: <store>/<NAME>.signed.json)");
        let _ = writeln!(s, "        [--passphrase-env <VAR>] Passphrase env var (default: MAGENT_WEB3_PASSPHRASE)");
        let _ = writeln!(s, "        [--not-before <UNIX>]    Optional validity-window start");
        let _ = writeln!(s, "        [--not-after <UNIX>]     Optional validity-window end");
        let _ = writeln!(s, "    verify-signed <PATH>    Verify a signed-prompt envelope");
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS (for `set`):");
    let _ = writeln!(s, "    --prompt <TEXT|FILE>      The system prompt body (file or literal)");
    let _ = writeln!(s, "    --provider <NAME>         Provider hint (ollama | deepseek)");
    let _ = writeln!(s, "    --model <NAME>            Model hint (provider's default if empty)");
    let _ = writeln!(s, "    --description <TEXT>      Free-form description for audits");
    let _ = writeln!(s, "    --author <TEXT>           Author name / email");
    let _ = writeln!(s, "    --tag <TAG>               Repeatable. Adds a tag to the metadata");
    let _ = writeln!(s);
    let _ = writeln!(s, "STORAGE:");
    let _ = writeln!(s, "    Prompts are stored as JSON files under $MAGENT_PROMPTS_DIR, or");
    let _ = writeln!(s, "    $XDG_DATA_HOME/magent/prompts (or ~/.local/share/magent/prompts).");
    let _ = writeln!(s, "    The JSON shape is intentionally trivial so the files are easy to");
    let _ = writeln!(s, "    diff, audit, and hand-edit. See docs/PROMPT_STORE.md.");
    let _ = writeln!(s);
    let _ = writeln!(s, "OUTPUT STREAMS:");
    let _ = writeln!(s, "    `show` and `list` write their data to stdout, so");
    let _ = writeln!(
        s,
        "    `magent set-prompt show foo | jq .provider` works. Confirmations"
    );
    let _ = writeln!(s, "    and warnings go to stderr.");
    s
}

/// Help text for the `magent summary` subcommand. Shown when the
/// user runs `magent summary --help` or `magent help summary`.
pub fn summary_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent summary");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent summary <ACTION> [OPTIONS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "ACTIONS:");
    let _ = writeln!(
        s,
        "    save <TOPIC>               Persist a summary record (stdin or --from)"
    );
    let _ = writeln!(s, "    show <TOPIC>               Print a human summary (or --json raw)");
    let _ = writeln!(s, "    list                       List every stored topic");
    let _ = writeln!(s, "    delete <TOPIC>             Remove the file (idempotent)");
    let _ = writeln!(s, "    export <TOPIC>             Dump the raw JSON record to stdout");
    let _ = writeln!(
        s,
        "    load <TOPIC>               Dump the head_tail_window as a JSON array"
    );
    let _ = writeln!(
        s,
        "    rollback <TOPIC> <INDEX>   Promote history[INDEX] back to the active record"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS (for `save`):");
    let _ = writeln!(s, "    --from <FILE>              Read the JSON record from disk (default: stdin)");
    let _ = writeln!(
        s,
        "    --overwrite                Replace an existing record (default: refuse)"
    );
    let _ = writeln!(
        s,
        "    --dir <PATH>               Override the summaries directory for this invocation"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS (for `rollback`):");
    let _ = writeln!(
        s,
        "    --dir <PATH>               Override the summaries directory for this invocation"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "STORAGE:");
    let _ = writeln!(
        s,
        "    Summaries are stored as JSON files under $MAGENT_SUMMARIES_DIR, or"
    );
    let _ = writeln!(
        s,
        "    $XDG_DATA_HOME/magent/summaries (or ~/.local/share/magent/summaries)."
    );
    let _ = writeln!(s, "    Each topic is atomic-write (rename-after-fsync) and protected by a");
    let _ = writeln!(
        s,
        "    per-topic lock file so concurrent writers never produce a half-written JSON."
    );
    let _ = writeln!(s, "    See docs/SUMMARY_STORE.md.");
    s
}

/// Help text for the `magent scheduler` subcommand. Re-exported
/// from [`crate::scheduler`] so the dispatcher here can call it
/// without reaching into a sibling module.
pub fn scheduler_help_text() -> String {
    scheduler::scheduler_help_text()
}

/// Help text for the `magent web3` subcommand. Shown when the user
/// runs `magent web3 --help` or `magent help web3`.
#[cfg(feature = "web3")]
pub fn web3_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent web3");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent web3 <ACTION> [OPTIONS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "ACTIONS:");
    let _ = writeln!(
        s,
        "    new <NAME>                        Generate an Ed25519 identity and store it in the vault"
    );
    let _ = writeln!(
        s,
        "        [--passphrase-env <VAR>]      Read passphrase from $<VAR> (default: MAGENT_WEB3_PASSPHRASE)"
    );
    let _ = writeln!(
        s,
        "        [--force]                     Overwrite an existing entry with the same name"
    );
    let _ = writeln!(
        s,
        "        [--vault <PATH>]              Override vault location for this invocation"
    );
    let _ = writeln!(
        s,
        "    identity <NAME>                   Print DID + public key for a stored identity"
    );
    let _ = writeln!(
        s,
        "    did                               Derive a did:key (no vault access)"
    );
    let _ = writeln!(
        s,
        "        --from-seed <HEX>             Derive from a 32-byte Ed25519 seed"
    );
    let _ = writeln!(
        s,
        "        --from-pubkey <HEX>           Derive from a 32-byte public key"
    );
    let _ = writeln!(
        s,
        "    pubkey                            Derive a public key (hex) from a seed"
    );
    let _ = writeln!(
        s,
        "        --from-seed <HEX>             Required: 32-byte Ed25519 seed"
    );
    let _ = writeln!(
        s,
        "    sign <NAME>                       Sign a payload; emit a SignedMessage envelope"
    );
    let _ = writeln!(
        s,
        "        --payload <FILE|->            File path, or - for stdin (default: stdin)"
    );
    let _ = writeln!(
        s,
        "        [--output <FILE>]             Write envelope to a file (default: stdout)"
    );
    let _ = writeln!(
        s,
        "        [--passphrase-env <VAR>]      Read passphrase from $<VAR>"
    );
    let _ = writeln!(
        s,
        "        [--vault <PATH>]              Override vault location for this invocation"
    );
    let _ = writeln!(
        s,
        "    verify                            Verify a SignedMessage envelope"
    );
    let _ = writeln!(
        s,
        "        --payload <FILE|->            Same flag as `sign` (file or stdin)"
    );
    let _ = writeln!(
        s,
        "        --envelope <FILE>             Required: SignedMessage JSON file"
    );
    let _ = writeln!(
        s,
        "    list                              List every stored identity (DID + pubkey)"
    );
    let _ = writeln!(
        s,
        "    export <NAME>                     Dump the public-side JSON record to stdout"
    );
    let _ = writeln!(
        s,
        "    delete <NAME>                     Remove an identity from the vault"
    );
    #[cfg(feature = "blockchain")]
    {
        let _ = writeln!(
            s,
            "    chain list                        List the chains this build knows"
        );
        let _ = writeln!(
            s,
            "    chain info <CHAIN>                Show one chain's id, currency, RPC, explorer"
        );
        let _ = writeln!(
            s,
            "    balance <ADDRESS> [--chain <C>]   Query a native balance (needs network)"
        );
        let _ = writeln!(
            s,
            "    tx build <TO> --value <ETH> [--data <HEX>] [--chain <C>]"
        );
        let _ = writeln!(
            s,
            "                                      Build an unsigned transaction request"
        );
        let _ = writeln!(
            s,
            "    bind create <DID> --chain <C> --from-seed <HEX> [--expiry-days <N>]"
        );
        let _ = writeln!(
            s,
            "                                      Sign a DID-to-address binding proof"
        );
        let _ = writeln!(
            s,
            "    bind verify <FILE>                Verify a binding proof signature"
        );
        let _ = writeln!(
            s,
            "    vc issue <SUBJECT_DID> --type <TYPE> [--claim <KEY>=<JSON>]..."
        );
        let _ = writeln!(
            s,
            "        [--from-seed <HEX> | --issuer <NAME>] [--valid-until <ISO8601>]"
        );
        let _ = writeln!(
            s,
            "                                      Issue and sign a verifiable credential"
        );
        let _ = writeln!(
            s,
            "    vc verify <FILE>                  Check a credential's structure and expiry"
        );
        let _ = writeln!(
            s,
            "    vc present <FILE> --challenge <STR> [--from-seed <HEX> | --holder <NAME>]"
        );
        let _ = writeln!(
            s,
            "                                      Build a signed verifiable presentation"
        );
        let _ = writeln!(s, "    vc verify-presentation <FILE> [--challenge <STR>]");
        let _ = writeln!(
            s,
            "                                      Verify a presentation, its credentials, and"
        );
        let _ = writeln!(
            s,
            "                                      (with --challenge) that it answers your nonce"
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "STORAGE:");
    let _ = writeln!(s, "    The encrypted vault defaults to:");
    let _ = writeln!(
        s,
        "        $MAGENT_WEB3_KEYSTORE         full file path (highest priority)"
    );
    let _ = writeln!(s, "        $MAGENT_WEB3_KEYSTORE_DIR/keys.json");
    let _ = writeln!(s, "        $XDG_DATA_HOME/magent/web3/keys.json");
    let _ = writeln!(s, "        $HOME/.local/share/magent/web3/keys.json");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "        $MAGENT_WEB3_KEYSTORE_DIR/keys.json"
    );
    let _ = writeln!(
        s,
        "        $XDG_DATA_HOME/magent/web3/keys.json"
    );
    let _ = writeln!(
        s,
        "        $HOME/.local/share/magent/web3/keys.json"
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "    On-disk schema (base64-encoded salt / nonce / ciphertext):");
    let _ = writeln!(s, "        {{");
    let _ = writeln!(s, "          \"schema_version\": 1,");
    let _ = writeln!(s, "          \"kdf\": \"argon2id\",");
    let _ = writeln!(s, "          \"aead\": \"chacha20-poly1305\",");
    let _ = writeln!(s, "          \"kdf_params\": {{ ... }},");
    let _ = writeln!(s, "          \"identities\": {{");
    let _ = writeln!(s, "            \"<NAME>\": {{ \"public_key_hex\": ..., \"did\": ...,");
    let _ = writeln!(s, "                          \"salt_b64\": ..., \"nonce_b64\": ...,");
    let _ = writeln!(s, "                          \"ciphertext_b64\": ... }}");
    let _ = writeln!(s, "          }}");
    let _ = writeln!(s, "        }}");
    let _ = writeln!(s);
    let _ = writeln!(s, "SECURITY:");
    let _ = writeln!(
        s,
        "    The passphrase NEVER appears on the command line (it would land in shell history)."
    );
    let _ = writeln!(
        s,
        "    Pass it via --passphrase-env <VAR> and `export <VAR>=...`, or rely on the default"
    );
    let _ = writeln!(
        s,
        "    $MAGENT_WEB3_PASSPHRASE. Argon2id + ChaCha20-Poly1305 protect the on-disk file."
    );
    let _ = writeln!(s, "    Public-side keys (pubkey, DID) are stored in the clear so");
    let _ = writeln!(s, "    `identity` / `list` don't need the passphrase; the secret");
    let _ = writeln!(s, "    seed is always ciphertext.");
    s
}

/// Help text for the `magent config` subcommand. Shown when the user
/// runs `magent config --help` or `magent help config`.
pub fn config_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent config");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent config <ACTION> [ARGS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "ACTIONS:");
    let _ = writeln!(s, "    init                 Create the config file at the canonical path");
    let _ = writeln!(s, "    where                Print the resolved config file path");
    let _ = writeln!(s, "    show                 Print the full JSON record (pretty)");
    let _ = writeln!(s, "    list                 Flatten every key/value pair");
    let _ = writeln!(s, "    get <KEY>            Read a single key (e.g. provider.ollama.url)");
    let _ = writeln!(s, "    set <KEY> <VALUE>    Write a single key");
    let _ = writeln!(s, "    reset [--yes]        Delete the config file (refuses without --yes)");
    let _ = writeln!(s, "    validate             Re-load and verify every field (CI-friendly exit codes)");
    let _ = writeln!(s, "    format               Re-serialise the file with canonical key order");
    let _ = writeln!(s, "    export               Dump the JSON record to stdout (redirect-safe; both modes)");
    let _ = writeln!(s);
    let _ = writeln!(s, "OUTPUT STREAMS:");
    let _ = writeln!(s, "    Data (where/show/list/get) goes to stdout, so");
    let _ = writeln!(
        s,
        "    `magent config show > backup.json` works. Confirmations,"
    );
    let _ = writeln!(
        s,
        "    traces and `validate` verdicts go to stderr. `--json` puts"
    );
    let _ = writeln!(s, "    one envelope on stdout and nothing else.");
    let _ = writeln!(s);
    let _ = writeln!(s, "KEY PATHS (most useful first):");
    let _ = writeln!(s, "    provider.default                       \"ollama\" | \"deepseek\"");
    let _ = writeln!(s, "    provider.ollama.url                    Base URL for the local Ollama");
    let _ = writeln!(s, "    provider.ollama.model                  Default Ollama model");
    let _ = writeln!(s, "    provider.ollama.api_key_env            Env var that holds the API key");
    let _ = writeln!(s, "    provider.deepseek.url                  DeepSeek base URL");
    let _ = writeln!(s, "    provider.deepseek.model                DeepSeek model");
    let _ = writeln!(s, "    sampling.temperature                   0.0–2.0");
    let _ = writeln!(s, "    sampling.num_predict                   Max tokens per response");
    let _ = writeln!(s, "    sampling.top_p                         0.0–1.0");
    let _ = writeln!(s, "    sampling.top_k                         Integer");
    let _ = writeln!(s, "    runner.max_iterations                  ReAct loop cap");
    let _ = writeln!(s, "    runner.max_tool_calls                  Tool execution cap");
    let _ = writeln!(s, "    runner.probe_ollama_on_run             true | false");
    let _ = writeln!(s, "    compression.max_messages               Live conversation cap (0 = off)");
    let _ = writeln!(
        s,
        "    compression.tool_content_max_chars     Tool result cap, in characters (0 = off)"
    );
    let _ = writeln!(s, "    io.no_color / io.quiet_default / io.json_default");
    let _ = writeln!(s);
    let _ = writeln!(s, "STORAGE:");
    let _ = writeln!(s, "    $MAGENT_CONFIG_FILE  → exact file path (highest priority)");
    let _ = writeln!(s, "    $MAGENT_CONFIG_DIR   → directory holding <NAME>.json");
    let _ = writeln!(s, "    $XDG_CONFIG_HOME/magent/magent.json");
    let _ = writeln!(s, "    $HOME/.config/magent/magent.json (default)");
    let _ = writeln!(s);
    let _ = writeln!(s, "SECRETS:");
    let _ = writeln!(s, "    API keys are NEVER written to the config file. Only the env");
    let _ = writeln!(s, "    var *name* (e.g. `DEEPSEEK_API_KEY`) is stored — the actual");
    let _ = writeln!(s, "    secret lives in your shell environment.");
    s
}

/// Help text for the `magent doctor` subcommand. Shown when the user
/// runs `magent doctor --help` or `magent help doctor`.
pub fn doctor_help_text() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "mAgent doctor");
    let _ = writeln!(s);
    let _ = writeln!(s, "USAGE:");
    let _ = writeln!(s, "    magent doctor [OPTIONS]");
    let _ = writeln!(s);
    let _ = writeln!(s, "OPTIONS:");
    let _ = writeln!(s, "    (none) Doctor takes no flags — it just probes the env.");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "ENVIRONMENT:"
    );
    let _ = writeln!(s, "    MAGENT_PROVIDER     Override provider (ollama|deepseek).");
    let _ = writeln!(s, "    OLLAMA_HOST         Reach Ollama at this URL (default http://localhost:11434).");
    let _ = writeln!(s, "    OLLAMA_MODEL        Model name to check (default llama3.2).");
    let _ = writeln!(s, "    DEEPSEEK_HOST       Override DeepSeek base URL.");
    let _ = writeln!(s, "    DEEPSEEK_API_KEY    API key for DeepSeek checks.");
    let _ = writeln!(
        s,
        "    OLLAMA_API_KEY      Alias for DEEPSEEK_API_KEY (legacy)."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "CHECKS:");
    let _ = writeln!(s, "    1. Probe Ollama at the configured URL.");
    let _ = writeln!(s, "    2. Verify the configured model is available.");
    let _ = writeln!(s, "    3. Exercise the tool backend (SimulatorExecutor).");
    let _ = writeln!(s);
    let _ = writeln!(s, "Exit code is 0 when every check passes, 1 otherwise.");
    s
}

// ============================================================================
// Unit tests for the parser
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn argv<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rfc3339_offset_forms() {
        // Anchor on the parser's own UTC reading and state the offsets
        // relative to it, so the test asserts the *offset semantics* rather
        // than restating a hand-computed epoch second.
        let utc = parse_rfc3339("2026-08-11T09:00:00Z").unwrap();
        // A space separator is normalised to `T`.
        assert_eq!(parse_rfc3339("2026-08-11 09:00:00").unwrap(), utc);
        // The trailing `Z` is optional.
        assert_eq!(parse_rfc3339("2026-08-11T09:00:00").unwrap(), utc);
        // `+01:00` means local time is one hour ahead of UTC.
        assert_eq!(
            parse_rfc3339("2026-08-11T09:00:00+01:00").unwrap(),
            utc - 3600
        );
        assert_eq!(
            parse_rfc3339("2026-08-11T09:00:00-05:30").unwrap(),
            utc + 5 * 3600 + 1800
        );

        assert!(parse_rfc3339("2026-13-11T09:00:00").is_err(), "month 13");
        assert!(parse_rfc3339("2026-02-30T09:00:00").is_err(), "Feb 30");
        assert!(parse_rfc3339("short").is_err());
        assert!(
            parse_rfc3339("2026-08-11X09:00:00").is_err(),
            "bad separator"
        );
    }

    /// A multi-byte character in the seconds field used to abort the process:
    /// the separator check pins bytes 4/7/10/13/16 to ASCII but says nothing
    /// about 17–18, so `&normalized[17..19]` landed inside a code point and
    /// `&str` indexing panicked with "byte index 19 is not a char boundary".
    /// Reachable as `magent scheduler daemon --at 2026-08-11T09:00:日`.
    #[test]
    fn rfc3339_multibyte_in_time_fields_is_rejected_not_panicked() {
        assert!(parse_rfc3339("2026-08-11T09:00:日").is_err());
        assert!(parse_rfc3339("2026-08-11T09:00:00日").is_err());
    }

    /// `--model --mock` used to consume `--mock` as the model name, silently
    /// disabling the simulator and contacting the real backend.
    #[test]
    fn a_following_flag_is_not_swallowed_as_a_value() {
        match Args::parse(&argv(["magent", "run", "--model", "--mock", "hi"])) {
            Err(ParseError::MissingValue(flag)) => assert_eq!(flag, "model"),
            other => panic!("expected MissingValue(model), got {other:?}"),
        }
        match Args::parse(&argv(["magent", "run", "-m", "--mock", "hi"])) {
            Err(ParseError::MissingValue(flag)) => assert_eq!(flag, "-m"),
            other => panic!("expected MissingValue(-m), got {other:?}"),
        }
        // The attached forms remain valid: the value is explicit.
        assert!(Args::parse(&argv(["magent", "run", "--model=--mock", "hi"])).is_ok());
    }

    /// `--` must end option parsing so a dash-leading task is representable.
    #[test]
    fn double_dash_ends_option_parsing() {
        let a = Args::parse(&argv(["magent", "run", "--", "-5 degrees"])).unwrap();
        match a.command {
            Command::Run(opts) => {
                assert_eq!(opts.task, "-5 degrees");
                assert!(!opts.mock);
            }
            other => panic!("expected run, got {other:?}"),
        }
    }

    /// Schedule flags belong to `daemon`; `run-once` used to accept and drop
    /// them, so `run-once --cron …` looked like the schedule had applied.
    #[test]
    fn run_once_rejects_daemon_schedule_flags() {
        for flag in ["--interval", "--cron", "--at"] {
            let value = match flag {
                "--interval" => "5",
                "--cron" => "0 0 1 1 *",
                _ => "2026-08-11T09:00:00Z",
            };
            let r = Args::parse(&argv([
                "magent",
                "scheduler",
                "run-once",
                flag,
                value,
            ]));
            assert!(
                matches!(r, Err(ParseError::Usage(_))),
                "{flag} must be rejected on run-once, got {r:?}"
            );
        }
    }

    #[test]
    fn parses_simple_run() {
        let a = Args::parse(&argv(["magent", "run", "Read the temperature"])).unwrap();
        match a.command {
            Command::Run(opts) => {
                assert_eq!(opts.task, "Read the temperature");
                // Provider starts empty in the parser; the runner
                // fills in the config-file default (or "ollama" as
                // a last resort) inside `apply_config_overrides`.
                assert_eq!(opts.provider, "");
                // No --model flag → empty sentinel; the runner fills
                // in `llama3.2` later.
                assert_eq!(opts.model, "");
                // URLs start empty in the parser; the runner fills
                // them in from the config file in `apply_config_overrides`.
                assert_eq!(opts.ollama_url, "");
                assert_eq!(
                    opts.deepseek_url,
                    ""
                );
                assert!(opts.api_key.is_none());
                assert_eq!(opts.max_iterations, None);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_deepseek_provider() {
        let a = Args::parse(&argv([
            "magent",
            "run",
            "--provider",
            "deepseek",
            "--api-key",
            "sk-test",
            "--deepseek-url",
            "https://proxy.example/v1",
            "Hello",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.provider, "deepseek");
                assert_eq!(o.api_key.as_deref(), Some("sk-test"));
                assert_eq!(o.deepseek_url, "https://proxy.example/v1");
                assert_eq!(o.task, "Hello");
                // No --model flag → empty sentinel.
                assert_eq!(o.model, "");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn rejects_unknown_provider() {
        let err =
            Args::parse(&argv(["magent", "run", "--provider", "openai", "x"])).unwrap_err();
        match err {
            ParseError::InvalidValue { flag, expected, .. } => {
                assert_eq!(flag, "--provider");
                assert!(expected.contains("ollama"));
            }
            other => panic!("expected InvalidValue, got {:?}", other),
        }
    }

    #[test]
    fn parses_long_options() {
        let a = Args::parse(&argv([
            "magent",
            "run",
            "--model",
            "qwen2.5:7b",
            "--ollama",
            "http://gpu:11434",
            "--max-iterations",
            "20",
            "--max-tool-calls",
            "5",
            "--mock",
            "--quiet",
            "Hello",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.model, "qwen2.5:7b");
                assert_eq!(o.ollama_url, "http://gpu:11434");
                assert_eq!(o.max_iterations, Some(20));
                assert_eq!(o.max_tool_calls, Some(5));
                assert!(o.mock);
                assert!(o.quiet);
                assert_eq!(o.task, "Hello");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_equals_form() {
        let a = Args::parse(&argv([
            "magent",
            "run",
            "--model=qwen2.5:7b",
            "--max-iterations=42",
            "task",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.model, "qwen2.5:7b");
                assert_eq!(o.max_iterations, Some(42));
                assert_eq!(o.task, "task");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_attached_short_option() {
        let a =
            Args::parse(&argv(["magent", "run", "-m", "llama3.1", "-i5", "hello"])).unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.model, "llama3.1");
                assert_eq!(o.max_iterations, Some(5));
                assert_eq!(o.task, "hello");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn multibyte_short_flag_is_rejected_not_a_panic() {
        // Regression: the short-option split used `&stripped[..1]` /
        // `&stripped[1..]`, which panics with "byte index 1 is not a char
        // boundary" when the flag character is multi-byte. A bad argument
        // must produce a parse error, never a crash.
        let err = Args::parse(&argv(["magent", "run", "-温度", "hi"])).unwrap_err();
        assert!(
            matches!(err, ParseError::UnknownFlag(_)),
            "expected UnknownFlag, got {err:?}"
        );
    }

    /// A usage error is a complete sentence, so it must reach the user
    /// verbatim. Before the `Usage` variant existed these flowed through
    /// `UnknownFlag`, whose `Display` prepends `unknown flag: ` — turning
    /// `scheduler daemon: --interval, --cron, and --at are mutually
    /// exclusive` into the nonsense `unknown flag: scheduler daemon: …`.
    #[test]
    fn usage_errors_render_verbatim_without_an_unknown_flag_prefix() {
        let err = Args::parse(&argv([
            "magent",
            "scheduler",
            "daemon",
            "--interval",
            "60",
            "--at",
            "2026-08-11T09:00:00",
        ]))
        .unwrap_err();
        assert!(
            matches!(err, ParseError::Usage(_)),
            "expected Usage, got {err:?}"
        );
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "scheduler daemon: --interval, --cron, and --at are mutually exclusive; \
             pass exactly one"
        );
        assert!(
            !rendered.contains("unknown flag"),
            "usage messages must not be prefixed: {rendered}"
        );
    }

    /// A mistyped subcommand is a subcommand problem, not a flag one — the
    /// old rendering answered `magent frobnicate` with `unknown flag:
    /// frobnicate`, as if `frobnicate` were a mistyped `--flag`.
    #[test]
    fn unknown_subcommand_is_named_as_a_subcommand() {
        let err = Args::parse(&argv(["magent", "frobnicate"])).unwrap_err();
        assert!(
            matches!(err, ParseError::Usage(_)),
            "expected Usage, got {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.starts_with("unknown subcommand: frobnicate"),
            "got {rendered}"
        );
        assert!(!rendered.contains("unknown flag"), "got {rendered}");
    }

    /// `help <topic>` where the topic is neither a subcommand nor a known
    /// help topic. This used to render as `unknown flag: help --bogus`.
    #[test]
    fn help_for_an_unknown_topic_lists_the_known_ones() {
        let err = Args::parse(&argv(["magent", "help", "--bogus-xyz"])).unwrap_err();
        assert!(
            matches!(err, ParseError::Usage(_)),
            "expected Usage, got {err:?}"
        );
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "no help for `--bogus-xyz` — try `help run`, `help set-prompt`, `help summary`, \
             `help config`, `help scheduler`, or `help doctor`"
        );
    }

    /// On a build without the `web3` feature, `magent web3 …` is a
    /// subcommand that exists but was compiled out — the message should say
    /// so rather than pretend the user mistyped a flag.
    #[cfg(not(feature = "web3"))]
    #[test]
    fn web3_on_a_non_web3_build_explains_it_was_not_built_in() {
        let err = Args::parse(&argv(["magent", "web3", "status"])).unwrap_err();
        assert!(
            matches!(err, ParseError::Usage(_)),
            "expected Usage, got {err:?}"
        );
        assert_eq!(
            err.to_string(),
            "unknown subcommand: web3 (this binary was built without the `web3` feature)"
        );
    }

    /// `--timezone` used to map `utc`/`UTC` to `Utc` and *everything else*
    /// to `Local`, so `--timezone Bogus/Zone` ran happily and silently
    /// shifted the cron wall clock. There is no tz database (no new
    /// dependencies), so a bogus value must be rejected rather than guessed.
    #[test]
    fn scheduler_rejects_a_timezone_it_cannot_honour() {
        use crate::scheduler::SchedulerAction;

        let err = Args::parse(&argv([
            "magent",
            "scheduler",
            "daemon",
            "--interval",
            "60",
            "--timezone",
            "Bogus/Zone",
        ]))
        .unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "got {err:?}");
        assert_eq!(
            err.to_string(),
            "scheduler daemon: --timezone expects `utc` or `local`, got `Bogus/Zone`"
        );

        // The two accepted spellings, both casings.
        for (raw, want) in [
            ("utc", true),
            ("UTC", true),
            ("local", false),
            ("LOCAL", false),
        ] {
            let a = Args::parse(&argv([
                "magent",
                "scheduler",
                "daemon",
                "--interval",
                "60",
                "--timezone",
                raw,
            ]))
            .unwrap_or_else(|e| panic!("`{raw}` should be accepted: {e}"));
            match a.command {
                Command::Scheduler(SchedulerAction::Daemon { timezone, .. }) => {
                    assert_eq!(
                        timezone == crate::scheduler::SchedulerTimezone::Utc,
                        want,
                        "`{raw}`"
                    );
                }
                other => panic!("expected Daemon, got {other:?}"),
            }
        }

        // No flag at all → Local, preserving the documented default.
        let a = Args::parse(&argv(["magent", "scheduler", "daemon", "--interval", "60"])).unwrap();
        match a.command {
            Command::Scheduler(SchedulerAction::Daemon { timezone, .. }) => {
                assert_eq!(timezone, crate::scheduler::SchedulerTimezone::Local);
            }
            other => panic!("expected Daemon, got {other:?}"),
        }
    }

    #[test]
    fn rejects_run_with_no_task() {
        let err = Args::parse(&argv(["magent", "run"])).unwrap_err();
        assert!(matches!(err, ParseError::MissingTask));
    }

    #[test]
    fn repl_alone_parses_with_empty_task() {
        let a = Args::parse(&argv(["magent", "run", "--repl"])).unwrap();
        match a.command {
            Command::Run(o) => {
                assert!(o.repl_mode);
                assert_eq!(o.task, "");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn repl_with_a_stdin_prompt_is_rejected() {
        // Both consume stdin: the prompt reader drains it to EOF before the
        // session starts, so the REPL would see EOF and exit having run
        // nothing.
        for argv_ in [
            ["magent", "run", "--repl", "-p", "-"],
            ["magent", "run", "--repl", "--prompt", "-"],
        ] {
            let err = Args::parse(&argv(argv_)).unwrap_err();
            assert!(matches!(err, ParseError::ReplPromptStdin), "got {err:?}");
            assert!(
                err.to_string().contains("stdin"),
                "the message must name the conflict: {err}"
            );
        }
        // A file prompt is unaffected.
        assert!(Args::parse(&argv(["magent", "run", "--repl", "-p", "p.txt"])).is_ok());
    }

    #[test]
    fn repl_with_task_is_rejected() {
        // The task would otherwise be silently dropped: `execute()` enters
        // the REPL as soon as `repl_mode` is set.
        let err = Args::parse(&argv(["magent", "run", "--repl", "read the temp"])).unwrap_err();
        assert!(matches!(err, ParseError::ReplWithTask), "got {err:?}");
    }

    #[test]
    fn repl_with_json_is_rejected() {
        // The REPL writes its transcript straight to stdout, so `--json`
        // would put prose on stdout instead of a JSON envelope.
        let e1 = Args::parse(&argv(["magent", "--json", "run", "--repl"])).unwrap_err();
        assert!(matches!(e1, ParseError::ReplJsonIncompatible), "got {e1:?}");
        let e2 = Args::parse(&argv(["magent", "run", "--repl", "--json"])).unwrap_err();
        assert!(matches!(e2, ParseError::ReplJsonIncompatible), "got {e2:?}");
    }

    #[test]
    fn probe_ollama_with_mock_is_rejected() {
        // `--probe-ollama` requires the LLM to be reachable before the
        // agent answers; `--mock` never contacts one. Whichever we
        // honoured, the other would be dropped without a word — and
        // silently simulating is exactly what `--probe-ollama` exists
        // to prevent.
        let e1 =
            Args::parse(&argv(["magent", "run", "--probe-ollama", "--mock", "hi"])).unwrap_err();
        assert!(matches!(e1, ParseError::ProbeWithMock), "got {e1:?}");
        let e2 =
            Args::parse(&argv(["magent", "run", "--mock", "--probe-ollama", "hi"])).unwrap_err();
        assert!(matches!(e2, ParseError::ProbeWithMock), "got {e2:?}");
        // The REPL is not exempt.
        let e3 = Args::parse(&argv([
            "magent",
            "run",
            "--repl",
            "--probe-ollama",
            "--mock",
        ]))
        .unwrap_err();
        assert!(matches!(e3, ParseError::ProbeWithMock), "got {e3:?}");
    }

    #[test]
    fn probe_ollama_alone_is_accepted() {
        let args = Args::parse(&argv(["magent", "run", "--probe-ollama", "hi"])).unwrap();
        let Command::Run(opts) = args.command else {
            panic!("expected a run command");
        };
        assert!(opts.probe_ollama);
        assert!(!opts.mock);
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn repl_with_verify_signed_is_rejected() {
        // `execute()` returns from the verify branch before the REPL check,
        // so the combination used to drop `--repl` silently.
        let err = Args::parse(&argv([
            "magent",
            "run",
            "--repl",
            "--verify-signed",
            "/tmp/x.json",
        ]))
        .unwrap_err();
        assert!(matches!(err, ParseError::ReplVerifySigned), "got {err:?}");
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn run_verify_signed_does_not_require_a_task() {
        // `--verify-signed <PATH>` is a pure verification path (it never
        // executes the agent), so it must NOT trigger the MissingTask error.
        let a = Args::parse(&argv(["magent", "run", "--verify-signed", "/tmp/x.json"])).unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.task, "");
                assert!(!o.repl_mode);
                assert_eq!(
                    o.verify_signed_path.as_deref().map(|p| p.to_str().unwrap()),
                    Some("/tmp/x.json")
                );
            }
            _ => panic!("expected Run"),
        }
    }

    #[cfg(feature = "email-tools")]
    #[test]
    fn email_tools_followed_by_flag_does_not_swallow_the_flag() {
        // Regression: `--email-tools` used `iter.next()` to peek its optional
        // path, which consumed a following flag. So
        // `run --email-tools --provider deepseek <task>` lost `--provider`
        // (making `deepseek` the task and the real task an extra positional).
        let a = Args::parse(&argv([
            "magent",
            "run",
            "--email-tools",
            "--provider",
            "deepseek",
            "read temperature",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => {
                // --email-tools followed by a flag → default (empty) path.
                assert_eq!(o.email_tools.as_deref(), Some(""));
                assert_eq!(o.provider, "deepseek");
                assert_eq!(o.task, "read temperature");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn rejects_extra_positional() {
        let err = Args::parse(&argv(["magent", "run", "one", "two"])).unwrap_err();
        assert!(matches!(err, ParseError::TooManyPositional { .. }));
    }

    #[test]
    fn rejects_unknown_flag() {
        let err = Args::parse(&argv(["magent", "run", "--bogus", "1", "task"])).unwrap_err();
        assert!(matches!(err, ParseError::UnknownFlag(_)));
    }

    #[test]
    fn rejects_invalid_number() {
        let err =
            Args::parse(&argv(["magent", "run", "--max-iterations", "ten", "task"])).unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(
            Args::parse(&argv(["magent", "--help"])).unwrap().command,
            Command::Help
        ));
        assert!(matches!(
            Args::parse(&argv(["magent", "--version"])).unwrap().command,
            Command::Version
        ));
    }

    #[test]
    fn version_rejects_trailing_args() {
        // Global flags are stripped before the subcommand arm runs, so
        // `version --json` stays valid …
        let a = Args::parse(&argv(["magent", "version", "--json"])).unwrap();
        assert!(matches!(a.command, Command::Version));
        assert!(a.global.json);
        // … but a genuine typo must not be silently swallowed.
        assert!(Args::parse(&argv(["magent", "version", "--bogus"])).is_err());
        assert!(Args::parse(&argv(["magent", "version", "extra"])).is_err());
    }

    #[test]
    fn global_json_flag() {
        let a = Args::parse(&argv(["magent", "--json", "run", "task"])).unwrap();
        assert!(a.global.json);
    }

    #[test]
    fn global_json_flag_after_subcommand_is_accepted() {
        // Regression test for the previous behaviour where
        // `magent run --json task` was rejected with
        // `unknown flag: --json`. Other CLIs (cargo, kubectl, …)
        // accept this and users kept getting tripped up.
        let a = Args::parse(&argv(["magent", "run", "--json", "task"]))
            .expect("global flag after subcommand should be accepted");
        assert!(a.global.json);
        match a.command {
            Command::Run(o) => assert_eq!(o.task, "task"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn global_verbose_short_and_long_work_after_subcommand() {
        // Both `-v` and `--verbose` should still work after the
        // subcommand. We only check parsing here — the actual
        // env_logger init is verified in the `init_logger`
        // integration tests.
        let a = Args::parse(&argv(["magent", "run", "-v", "task"])).unwrap();
        assert!(a.global.verbose);
        let a = Args::parse(&argv(["magent", "run", "--verbose", "task"]))
            .unwrap();
        assert!(a.global.verbose);
    }

    #[test]
    fn global_log_level_after_subcommand_consumes_value() {
        // `--log-level <LEVEL>` needs to peek the next token even
        // when placed after the subcommand. The remaining argv
        // must still produce a valid Run command with the right
        // task.
        let a = Args::parse(&argv([
            "magent", "run", "--log-level", "debug", "task",
        ]))
        .unwrap();
        assert_eq!(a.global.log_level.as_deref(), Some("debug"));
        match a.command {
            Command::Run(o) => assert_eq!(o.task, "task"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn global_log_level_after_subcommand_without_value_is_kept() {
        // Missing value → the bare flag name is kept so the
        // downstream parser can produce the usual
        // `flag '--log-level' requires a value` error.
        let mut g = GlobalFlags::default();
        let kept =
            extract_global_flags(&argv(["--log-level"]), &mut g);
        assert!(kept.contains(&"--log-level".to_string()));
        assert!(g.log_level.is_none());
    }

    #[test]
    fn extract_global_flags_preserves_unknown_flags() {
        // Subcommand-specific flags (e.g. `--mock`) must NOT be
        // eaten by the global extractor — they belong to the
        // subcommand parser.
        let mut g = GlobalFlags::default();
        let kept = extract_global_flags(
            &argv(["--mock", "--json", "--provider", "ollama", "task"]),
            &mut g,
        );
        assert!(g.json);
        assert!(kept.iter().any(|s| s == "--mock"));
        assert!(kept.iter().any(|s| s == "--provider"));
        assert!(kept.iter().any(|s| s == "ollama"));
        assert!(kept.iter().any(|s| s == "task"));
    }

    #[test]
    fn global_no_color_after_subcommand_is_accepted() {
        let a = Args::parse(&argv(["magent", "run", "--no-color", "task"]))
            .unwrap();
        assert!(a.global.no_color);
    }

    #[test]
    fn no_subcommand_defaults_to_help() {
        let a = Args::parse(&argv(["magent"])).unwrap();
        assert!(matches!(a.command, Command::Help));
    }

    #[test]
    fn doctor_subcommand() {
        let a = Args::parse(&argv(["magent", "doctor"])).unwrap();
        assert!(matches!(a.command, Command::Doctor));
    }

    #[test]
    fn parses_max_messages_flag() {
        let a = Args::parse(&argv([
            "magent", "run", "--max-messages", "12", "task",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.max_messages, Some(12)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_max_messages_zero_disables() {
        let a = Args::parse(&argv([
            "magent", "run", "--max-messages", "0", "task",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.max_messages, Some(0)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_tool_max_chars_flag() {
        let a = Args::parse(&argv([
            "magent", "run", "--tool-max-chars", "200", "task",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.tool_max_chars, Some(200)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn rejects_invalid_max_messages() {
        let err = Args::parse(&argv([
            "magent", "run", "--max-messages", "twelve", "task",
        ]))
        .unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
    }

    #[test]
    fn rejects_zero_max_iterations() {
        // `0` caps the loop at zero steps — the run then fails with
        // "run ended without a final answer". Reject it at parse time.
        for flag in ["--max-iterations", "-i"] {
            let err = Args::parse(&argv(["magent", "run", flag, "0", "task"])).unwrap_err();
            assert!(
                matches!(err, ParseError::InvalidValue { .. }),
                "{flag} 0 should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_zero_max_tool_calls() {
        for flag in ["--max-tool-calls", "-t"] {
            let err = Args::parse(&argv(["magent", "run", flag, "0", "task"])).unwrap_err();
            assert!(
                matches!(err, ParseError::InvalidValue { .. }),
                "{flag} 0 should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn accepts_one_max_iterations() {
        let a = Args::parse(&argv(["magent", "run", "-i", "1", "task"])).unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.max_iterations, Some(1)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_timeout_seconds() {
        let a = Args::parse(&argv(["magent", "run", "--timeout", "90", "task"])).unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.timeout_secs, Some(90)),
            _ => panic!("expected Run"),
        }
        let a = Args::parse(&argv(["magent", "run", "--timeout=1", "task"])).unwrap();
        match a.command {
            Command::Run(o) => assert_eq!(o.timeout_secs, Some(1)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn rejects_a_timeout_that_cannot_bound_a_run() {
        // `0` would expire before the first step, and a non-number is not a
        // duration at all; both are usage errors rather than a silent
        // fallback to "no timeout".
        for value in ["0", "soon"] {
            let err = Args::parse(&argv(["magent", "run", "--timeout", value, "task"])).unwrap_err();
            assert!(
                matches!(err, ParseError::InvalidValue { .. }),
                "--timeout {value} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn compression_defaults_match_library() {
        // The CLI defaults must mirror `CompressionPolicy::default()`
        // so users get a sensible out-of-the-box experience without
        // having to set both flags.
        assert_eq!(RunOptions::default().max_messages, None);
        assert_eq!(RunOptions::default().tool_max_chars, None);
    }

    /// The `--repl` line is the only place `run --help` states the REPL's
    /// mode constraints, and each one the parser enforces (`ReplWithTask`,
    /// `ReplJsonIncompatible`, `ReplVerifySigned`) has to be named there —
    /// otherwise the flag looks combinable and the error is a surprise.
    /// `--verify-signed` was added to the parser without updating this line.
    #[test]
    fn run_help_text_documents_repl_constraints() {
        let h = run_help_text();
        let line = h
            .lines()
            .find(|l| l.trim_start().starts_with("--repl"))
            .unwrap_or_else(|| panic!("`--repl` is missing from run --help:\n{h}"));
        for needle in ["no <TASK>", "--json", "--verify-signed"] {
            assert!(
                line.contains(needle),
                "the `--repl` help line does not mention {needle:?}: {line}"
            );
        }
    }

    /// The exit codes are the only interface a CI script can rely on
    /// without parsing either stream, and they were documented nowhere.
    /// Pinned so a future change to `main`'s dispatch has to update the
    /// contract it prints.
    #[test]
    fn help_text_documents_every_exit_code() {
        let h = help_text("0.0.0-test");
        assert!(h.contains("EXIT CODES:"), "no EXIT CODES section:\n{h}");
        let section: String = h
            .lines()
            .skip_while(|l| !l.starts_with("EXIT CODES:"))
            .take_while(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        for code in ["0", "1", "2", "101", "130", "141"] {
            assert!(
                section.contains(&format!("\n    {code}")),
                "exit code {code} is not documented:\n{section}"
            );
        }
        // Code 1 is where the `--json` contract matters most, so the
        // section must say that stdout still carries an envelope.
        assert!(
            section.contains("ok\":false"),
            "the JSON-failure promise is missing:\n{section}"
        );
    }

    #[test]
    fn run_help_text_mentions_compression_flags() {
        let h = run_help_text();
        assert!(h.contains("--max-messages"), "missing --max-messages");
        assert!(h.contains("--tool-max-chars"), "missing --tool-max-chars");
        assert!(h.contains("CONTEXT MANAGEMENT"));
    }

    /// `--features web3_app`: the new signed-envelope flags must
    /// appear in the help text. Pinning the help-text contract so
    /// a future refactor that drops one of them is caught
    /// immediately.
    #[cfg(feature = "web3_app")]
    #[test]
    fn run_help_text_mentions_signed_envelope_flags() {
        let h = run_help_text();
        for needle in [
            "--sign",
            "--signer",
            "--signed-output",
            "--not-after",
            "--not-before",
            "--verify-signed",
            "WEB3 SIGNED ENVELOPES",
        ] {
            assert!(h.contains(needle), "missing {:?} in run help text", needle);
        }
    }

    /// `--features web3_app`: the parser must accept every
    /// signed-envelope flag without complaining. We don't run
    /// the agent (this is a unit test) — just check that
    /// `Args::parse` succeeds and the values land on the
    /// expected fields.
    #[cfg(feature = "web3_app")]
    #[test]
    fn parse_run_sign_envelope_flags() {
        let argv = argv([
            "magent", "run", "--sign",
            "--signer", "agent-1",
            "--signed-output", "/tmp/foo.json",
            "--not-before", "100",
            "--not-after", "200",
            "--verify-signed", "/tmp/in.json",
        ]);
        let a = Args::parse(&argv).unwrap();
        let Command::Run(opts) = a.command else {
            panic!("expected Run command, got {:?}", a.command);
        };
        assert_eq!(opts.sign_with_vault_identity.as_deref(), Some("agent-1"));
        assert_eq!(
            opts.signed_output.as_deref().map(|p| p.to_str().unwrap()),
            Some("/tmp/foo.json")
        );
        assert_eq!(opts.not_before_unix, Some(100));
        assert_eq!(opts.not_after_unix, Some(200));
        assert_eq!(
            opts.verify_signed_path.as_deref().map(|p| p.to_str().unwrap()),
            Some("/tmp/in.json")
        );
    }

    /// `--verify-signed` never runs the agent, so a task argument used to be
    /// silently discarded. Reject the combination instead.
    #[cfg(feature = "web3_app")]
    #[test]
    fn verify_signed_with_a_task_is_rejected() {
        let argv = argv([
            "magent", "run", "--verify-signed", "/tmp/in.json", "do something",
        ]);
        assert!(matches!(Args::parse(&argv), Err(ParseError::Usage(_))));
    }

    /// `--features web3_app`: bare `--sign` (no value) lands as
    /// the sentinel `"default"`, which the dispatcher recognises
    /// and falls back to `$MAGENT_AGENT_IDENTITY`. We pin the
    /// sentinel here so a future refactor that picks a different
    /// placeholder is forced to update the test (and the
    /// dispatcher).
    #[cfg(feature = "web3_app")]
    #[test]
    fn parse_run_bare_sign_uses_default_sentinel() {
        let argv = argv(["magent", "run", "--sign", "task"]);
        let a = Args::parse(&argv).unwrap();
        let Command::Run(opts) = a.command else {
            panic!();
        };
        assert_eq!(opts.sign_with_vault_identity.as_deref(), Some("default"));
    }

    #[test]
    fn run_subcommand_help_long_flag() {
        let a = Args::parse(&argv(["magent", "run", "--help"])).unwrap();
        assert!(matches!(a.command, Command::RunHelp));
    }

    #[test]
    fn run_subcommand_help_short_flag() {
        let a = Args::parse(&argv(["magent", "run", "-h"])).unwrap();
        assert!(matches!(a.command, Command::RunHelp));
    }

    #[test]
    fn run_subcommand_help_with_other_args() {
        // `--help` always wins, even when other valid flags are present.
        let a = Args::parse(&argv([
            "magent", "run", "--model", "llama3.2", "--help", "task",
        ]))
        .unwrap();
        assert!(matches!(a.command, Command::RunHelp));
    }

    #[test]
    fn doctor_subcommand_help_long_flag() {
        let a = Args::parse(&argv(["magent", "doctor", "--help"])).unwrap();
        assert!(matches!(a.command, Command::DoctorHelp));
    }

    #[test]
    fn doctor_subcommand_help_short_flag() {
        let a = Args::parse(&argv(["magent", "doctor", "-h"])).unwrap();
        assert!(matches!(a.command, Command::DoctorHelp));
    }

    #[test]
    fn help_subcommand_dispatches_to_subcommand_help() {
        // `magent help run` and `magent help doctor` should produce the
        // same output as `magent run --help` / `magent doctor --help`.
        assert!(matches!(
            Args::parse(&argv(["magent", "help", "run"])).unwrap().command,
            Command::RunHelp
        ));
        assert!(matches!(
            Args::parse(&argv(["magent", "help", "doctor"])).unwrap().command,
            Command::DoctorHelp
        ));
        assert!(matches!(
            Args::parse(&argv(["magent", "help"])).unwrap().command,
            Command::Help
        ));
    }

    #[test]
    fn help_subcommand_rejects_unknown_target() {
        let err = Args::parse(&argv(["magent", "help", "wat"])).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)));
    }

    #[test]
    fn doctor_help_text_lists_environment_variables() {
        let h = doctor_help_text();
        assert!(h.contains("OLLAMA_HOST"));
        assert!(h.contains("DEEPSEEK_API_KEY"));
        assert!(h.contains("CHECKS"));
    }

    // ------------------------------------------------------------------
    // `magent set-prompt` subcommand parser tests
    // ------------------------------------------------------------------

    #[test]
    fn set_prompt_set_minimal() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "set", "alpha", "--prompt", "Hello agent.",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Set(opts)) => {
                assert_eq!(opts.name, "alpha");
                assert_eq!(opts.prompt, "Hello agent.");
                assert_eq!(opts.provider, None);
                assert_eq!(opts.model, None);
                assert!(opts.tags.is_empty());
            }
            other => panic!("expected Set, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_set_with_all_flags() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "set", "alpha", "--prompt", "hi", "--provider", "deepseek",
            "--model", "deepseek-chat", "--description", "test prompt", "--author", "me",
            "--tag", "alpha", "--tag=beta",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Set(opts)) => {
                assert_eq!(opts.provider.as_deref(), Some("deepseek"));
                assert_eq!(opts.model.as_deref(), Some("deepseek-chat"));
                assert_eq!(opts.description.as_deref(), Some("test prompt"));
                assert_eq!(opts.author.as_deref(), Some("me"));
                assert_eq!(opts.tags, vec!["alpha", "beta"]);
            }
            other => panic!("expected Set, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_set_missing_prompt_arg() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "set", "alpha", "--prompt"])).unwrap_err();
        assert!(matches!(a, ParseError::MissingValue(_)));
    }

    #[test]
    fn set_prompt_set_missing_name() {
        let a = Args::parse(&argv(["magent", "set-prompt", "set"])).unwrap_err();
        // No NAME positional → "requires <NAME>" via UnknownFlag.
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_show_basic() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "show", "alpha"])).unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Show(name)) => {
                assert_eq!(name, "alpha");
            }
            other => panic!("expected Show, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_show_rejects_extra_args() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "show", "alpha", "extra",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_list_basic() {
        let a = Args::parse(&argv(["magent", "set-prompt", "list"])).unwrap();
        assert!(matches!(a.command, Command::SetPrompt(prompt::SetPromptAction::List)));
    }

    #[test]
    fn set_prompt_list_rejects_args() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "list", "extra"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_delete_basic() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "delete", "alpha"])).unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Delete(name)) => {
                assert_eq!(name, "alpha");
            }
            other => panic!("expected Delete, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_export_basic() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "export", "alpha"])).unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Export(name)) => {
                assert_eq!(name, "alpha");
            }
            other => panic!("expected Export, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_import_basic() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "import", "/tmp/health.json",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Import(opts)) => {
                assert_eq!(opts.path, PathBuf::from("/tmp/health.json"));
                assert!(opts.name.is_none());
                assert!(!opts.force);
            }
            other => panic!("expected Import, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_import_with_name_and_force() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "import", "/tmp/x.json",
            "--name", "renamed", "--force",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Import(opts)) => {
                assert_eq!(opts.name.as_deref(), Some("renamed"));
                assert!(opts.force);
            }
            other => panic!("expected Import, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_import_missing_path_is_an_error() {
        let a = Args::parse(&argv(["magent", "set-prompt", "import"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_import_unknown_flag_is_an_error() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "import", "/tmp/x.json", "--bogus", "v",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::UnknownFlag(_)));
    }

    #[test]
    fn set_prompt_template_basic() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "user=alice", "--var", "role=admin",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Template(opts)) => {
                assert_eq!(opts.name, "greet");
                assert_eq!(opts.vars, vec![
                    ("user".to_string(), "alice".to_string()),
                    ("role".to_string(), "admin".to_string()),
                ]);
                assert!(opts.vars_from.is_none());
            }
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_template_with_vars_from() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--vars-from", "/tmp/vars.json",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Template(opts)) => {
                assert_eq!(opts.vars_from, Some(PathBuf::from("/tmp/vars.json")));
            }
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_template_var_with_empty_key_is_an_error() {
        // `--var =value` would silently create an empty-key
        // variable that never matches in `render_template`. We
        // reject it at the parser level so the typo is loud.
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "=value",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::InvalidValue { .. }));
    }

    #[test]
    fn set_prompt_template_var_without_equals_is_an_error() {
        // `--var plain` (no `=`) is malformed.
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "plain",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::InvalidValue { .. }));
    }

    #[test]
    fn set_prompt_template_missing_name_is_an_error() {
        let a = Args::parse(&argv(["magent", "set-prompt", "template"]))
            .unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_template_value_with_equals_preserves_trailing() {
        // `--var url=http://x?y=1` — values may legitimately
        // contain `=`. The parser splits at the *first* `=`.
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "url=http://example.com?token=abc",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Template(opts)) => {
                assert_eq!(opts.vars.len(), 1);
                assert_eq!(opts.vars[0].0, "url");
                assert_eq!(opts.vars[0].1, "http://example.com?token=abc");
            }
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_template_unknown_flag_is_an_error() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--bogus", "v",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::UnknownFlag(_)));
    }

    #[test]
    fn set_prompt_template_var_without_value_errors() {
        // `--var name` (no `=value`, next token is a flag) must
        // error out rather than silently binding `name` to the
        // literal string "--vars-from". The exact error variant
        // doesn't matter (we get `InvalidValue` because `name`
        // lacks `=`, but the key property is: not silently accepted).
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "name", "--vars-from", "/tmp/v.json",
        ]))
        .unwrap_err();
        // Either error is acceptable; the failure mode we want to
        // avoid is silent success.
        assert!(a.to_string().contains("--var"),
            "expected the error to mention --var; got {:?}", a);
    }

    #[test]
    fn set_prompt_template_inline_var_then_vars_from() {
        // Happy path: --var KEY=VALUE followed by --vars-from PATH.
        // The fix in `take_inline_or_next` (refusing to consume a
        // flag as a value) shouldn't break this sequence.
        let a = Args::parse(&argv([
            "magent", "set-prompt", "template", "greet",
            "--var", "name=Alice", "--vars-from", "/tmp/v.json",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Template(opts)) => {
                assert_eq!(opts.vars, vec![("name".to_string(), "Alice".to_string())]);
                assert_eq!(opts.vars_from, Some(PathBuf::from("/tmp/v.json")));
            }
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn set_prompt_no_action_shows_help() {
        // `magent set-prompt` with no subcommand → SetPromptHelp.
        let a = Args::parse(&argv(["magent", "set-prompt"])).unwrap();
        assert!(matches!(a.command, Command::SetPromptHelp));
    }

    #[test]
    fn set_prompt_help_flag_shows_help() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "--help"])).unwrap();
        assert!(matches!(a.command, Command::SetPromptHelp));
    }

    #[test]
    fn help_set_prompt_dispatches_to_subcommand_help() {
        let a = Args::parse(&argv(["magent", "help", "set-prompt"])).unwrap();
        assert!(matches!(a.command, Command::SetPromptHelp));
    }

    #[test]
    fn set_prompt_unknown_action_is_an_error() {
        let a =
            Args::parse(&argv(["magent", "set-prompt", "frobnicate"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn set_prompt_set_unknown_flag_is_an_error() {
        let a = Args::parse(&argv([
            "magent", "set-prompt", "set", "alpha", "--bogus", "x", "--prompt", "hi",
        ]))
        .unwrap_err();
        assert!(matches!(a, ParseError::UnknownFlag(_)));
    }

    #[test]
    fn set_prompt_help_text_mentions_actions() {
        let h = set_prompt_help_text();
        assert!(h.contains("USAGE"));
        assert!(h.contains("set"));
        assert!(h.contains("show"));
        assert!(h.contains("list"));
        assert!(h.contains("delete"));
        assert!(h.contains("export"));
        assert!(h.contains("MAGENT_PROMPTS_DIR"));
    }

    #[test]
    fn run_accepts_prompt_name_flag() {
        let a = Args::parse(&argv([
            "magent", "run", "--prompt-name", "alpha", "task",
        ]))
        .unwrap();
        match a.command {
            Command::Run(o) => {
                assert_eq!(o.prompt_name.as_deref(), Some("alpha"));
                assert!(o.prompt_file.is_none());
            }
            _ => panic!("expected Run"),
        }
    }

    // ------------------------------------------------------------------
    // `magent config` subcommand parser tests
    // ------------------------------------------------------------------

    #[test]
    fn config_no_action_shows_help() {
        let a = Args::parse(&argv(["magent", "config"])).unwrap();
        assert!(matches!(a.command, Command::ConfigHelp));
    }

    #[test]
    fn config_help_flag_shows_help() {
        let a = Args::parse(&argv(["magent", "config", "--help"])).unwrap();
        assert!(matches!(a.command, Command::ConfigHelp));
    }

    #[test]
    fn help_config_dispatches_to_subcommand_help() {
        let a = Args::parse(&argv(["magent", "help", "config"])).unwrap();
        assert!(matches!(a.command, Command::ConfigHelp));
    }

    #[test]
    fn config_init() {
        let a = Args::parse(&argv(["magent", "config", "init"])).unwrap();
        assert!(matches!(a.command, Command::Config(config::ConfigAction::Init)));
    }

    #[test]
    fn config_init_rejects_args() {
        let a = Args::parse(&argv(["magent", "config", "init", "extra"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn config_where() {
        let a = Args::parse(&argv(["magent", "config", "where"])).unwrap();
        assert!(matches!(a.command, Command::Config(config::ConfigAction::Where)));
    }

    #[test]
    fn config_show() {
        let a = Args::parse(&argv(["magent", "config", "show"])).unwrap();
        assert!(matches!(a.command, Command::Config(config::ConfigAction::Show)));
    }

    #[test]
    fn config_list() {
        let a = Args::parse(&argv(["magent", "config", "list"])).unwrap();
        assert!(matches!(a.command, Command::Config(config::ConfigAction::List)));
    }

    #[test]
    fn config_get_single_key() {
        let a = Args::parse(&argv([
            "magent", "config", "get", "provider.ollama.model",
        ]))
        .unwrap();
        match a.command {
            Command::Config(config::ConfigAction::Get(k)) => {
                assert_eq!(k, "provider.ollama.model");
            }
            other => panic!("expected Get, got {:?}", other),
        }
    }

    #[test]
    fn config_get_requires_key() {
        let a = Args::parse(&argv(["magent", "config", "get"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn config_set_key_value() {
        let a = Args::parse(&argv([
            "magent", "config", "set", "sampling.temperature", "0.7",
        ]))
        .unwrap();
        match a.command {
            Command::Config(config::ConfigAction::Set { key, value }) => {
                assert_eq!(key, "sampling.temperature");
                assert_eq!(value, "0.7");
            }
            other => panic!("expected Set, got {:?}", other),
        }
    }

    #[test]
    fn config_set_requires_key_and_value() {
        let a = Args::parse(&argv(["magent", "config", "set", "sampling.temperature"]))
            .unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn config_reset_requires_yes_flag() {
        let a = Args::parse(&argv(["magent", "config", "reset"])).unwrap();
        match a.command {
            Command::Config(config::ConfigAction::Reset { yes }) => assert!(!yes),
            other => panic!("expected Reset, got {:?}", other),
        }
        let a = Args::parse(&argv(["magent", "config", "reset", "--yes"])).unwrap();
        match a.command {
            Command::Config(config::ConfigAction::Reset { yes }) => assert!(yes),
            other => panic!("expected Reset, got {:?}", other),
        }
    }

    #[test]
    fn config_reset_rejects_unknown_flags() {
        let a = Args::parse(&argv(["magent", "config", "reset", "--force"]))
            .unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn config_format() {
        let a = Args::parse(&argv(["magent", "config", "format"])).unwrap();
        assert!(matches!(a.command, Command::Config(config::ConfigAction::Format)));
    }

    #[test]
    fn config_export() {
        let a = Args::parse(&argv(["magent", "config", "export"])).unwrap();
        assert!(matches!(
            a.command,
            Command::Config(config::ConfigAction::Export)
        ));
    }

    #[test]
    fn config_export_rejects_arguments() {
        let e = Args::parse(&argv(["magent", "config", "export", "extra"])).unwrap_err();
        assert!(matches!(e, ParseError::Usage(_)));
    }

    #[test]
    fn config_unknown_action_is_an_error() {
        let a = Args::parse(&argv(["magent", "config", "frobnicate"])).unwrap_err();
        assert!(matches!(a, ParseError::Usage(_)));
    }

    #[test]
    fn config_help_text_mentions_actions() {
        let h = config_help_text();
        assert!(h.contains("USAGE"));
        assert!(h.contains("init"));
        assert!(h.contains("where"));
        assert!(h.contains("show"));
        assert!(h.contains("list"));
        assert!(h.contains("get"));
        assert!(h.contains("set"));
        assert!(h.contains("reset"));
        assert!(h.contains("format"));
        assert!(h.contains("export"));
        assert!(h.contains("validate"));
        assert!(h.contains("MAGENT_CONFIG_FILE"));
        assert!(h.contains("SECRETS"));
    }

    // -----------------------------------------------------------------------
    // Doc-drift guards
    //
    // The user-facing docs (`README.md` and `docs/SUMMARY_STORE.md`)
    // reference specific subcommands and flags by name. If anyone
    // renames or removes one without updating the docs, the help
    // text below stops matching the prose and users follow broken
    // instructions. These tests parse `docs/SUMMARY_STORE.md` once
    // and grep for each subcommand/flag, refusing to compile if a
    // reference goes stale.
    // -----------------------------------------------------------------------

    /// Read `docs/SUMMARY_STORE.md` into a `String`. Tests call
    /// this directly so a missing file produces a clearer panic
    /// message than a `None` later.
    fn summary_doc_text() -> String {
        // `CARGO_MANIFEST_DIR` points at the crate root — i.e.
        // `cli/` — and the doc lives one level up at
        // `docs/SUMMARY_STORE.md`. Using a relative path keeps the
        // test portable across `cargo test` working directories.
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let doc_path = manifest_dir
            .parent()
            .expect("cli/ has a parent (workspace root)")
            .join("docs")
            .join("SUMMARY_STORE.md");
        std::fs::read_to_string(&doc_path).unwrap_or_else(|e| {
            panic!(
                "could not read {}: {} (tests rely on this file existing)",
                doc_path.display(),
                e
            )
        })
    }

    #[test]
    fn summary_doc_lists_every_subcommand() {
        let h = summary_help_text();
        let doc = summary_doc_text();
        // Every action in `SummaryAction` should appear in both
        // the help text and the doc. We list them explicitly so a
        // new subcommand forces an explicit decision about whether
        // to document it.
        for action in ["save", "show", "list", "delete", "export", "load", "rollback"] {
            assert!(
                h.contains(action),
                "summary --help is missing action {:?}",
                action
            );
            assert!(
                doc.contains(action),
                "docs/SUMMARY_STORE.md is missing action {:?}",
                action
            );
        }
        // More pointed check: the doc must list each action
        // inside the prose subcommand list (not just as a passing
        // mention). `rollback` is rare enough that this catches
        // accidental removal from the subcommand section even
        // when the prose elsewhere still mentions "history".
        let subcommands = "magent summary save \
                           magent summary show \
                           magent summary list \
                           magent summary delete \
                           magent summary export \
                           magent summary load \
                           magent summary rollback";
        for token in subcommands.split_whitespace() {
            // Skip the prefix words; we only want to check that
            // the action tokens appear *somewhere* in the
            // subcommand list block. The simple `contains`
            // already covers that, but this loop exists so the
            // intent is documented in one place.
            if matches!(token, "magent" | "summary") {
                continue;
            }
            assert!(
                doc.contains(token),
                "subcommand list in SUMMARY_STORE.md is missing {:?}",
                token
            );
        }
    }

    #[test]
    fn run_help_documents_save_and_load_summary_flags() {
        let h = run_help_text();
        for flag in ["--save-summary", "--save-summary-overwrite", "--load-summary"] {
            assert!(
                h.contains(flag),
                "magent run --help is missing flag {:?}",
                flag
            );
        }
        // The doc references these flags too — see
        // `Integration with magent run` section.
        let doc = summary_doc_text();
        assert!(
            doc.contains("--save-summary"),
            "docs/SUMMARY_STORE.md is missing --save-summary"
        );
        assert!(
            doc.contains("--load-summary"),
            "docs/SUMMARY_STORE.md is missing --load-summary"
        );
    }

    #[test]
    fn top_level_help_documents_summary_subcommand() {
        let h = help_text("test");
        assert!(
            h.contains("summary"),
            "top-level `magent --help` is missing the `summary` subcommand listing"
        );
    }

    #[test]
    fn readme_documents_summary_subcommand() {
        // README lives at the workspace root, not inside `cli/`.
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let readme = manifest_dir
            .parent()
            .expect("cli/ has a parent (workspace root)")
            .join("README.md");
        let txt = std::fs::read_to_string(&readme)
            .unwrap_or_else(|e| panic!("could not read README.md: {}", e));
        // Should reference the new subcommand and the dedicated
        // doc file at minimum.
        assert!(
            txt.contains("magent summary"),
            "README.md is missing `magent summary` example"
        );
        assert!(
            txt.contains("SUMMARY_STORE.md"),
            "README.md is missing the link to docs/SUMMARY_STORE.md"
        );
        assert!(
            txt.contains("--save-summary"),
            "README.md is missing --save-summary flag example"
        );
        assert!(
            txt.contains("--load-summary"),
            "README.md is missing --load-summary flag example"
        );
    }

    // ---------------------------------------------------------------
    // `magent web3 …` parser tests
    // ---------------------------------------------------------------
    #[test]
    fn config_doc_lists_every_action() {
        // Same doc-drift guard as `summary_doc_lists_every_subcommand`:
        // every action in `ConfigAction` must appear in both
        // `config --help` and `docs/CONFIG.md`, so adding one forces an
        // explicit decision about the doc. `export` was the action that
        // motivated this — it shipped in the help text and `CONFIG.md`
        // together, and this test keeps the two in lockstep.
        let h = config_help_text();
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let doc_path = manifest_dir
            .parent()
            .expect("cli/ has a parent (workspace root)")
            .join("docs")
            .join("CONFIG.md");
        let doc = std::fs::read_to_string(&doc_path)
            .unwrap_or_else(|e| panic!("could not read {}: {}", doc_path.display(), e));
        for action in [
            "init", "where", "show", "list", "get", "set", "reset", "validate", "format",
            "export",
        ] {
            assert!(
                h.contains(action),
                "config --help is missing action {:?}",
                action
            );
            assert!(
                doc.contains(action),
                "docs/CONFIG.md is missing action {:?}",
                action
            );
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_help_routes() {
        // `magent web3 --help` and `magent help web3` both
        // resolve to the dedicated help variant.
        let a = Args::parse(&argv(["magent", "web3", "--help"])).unwrap();
        assert!(matches!(a.command, Command::Web3Help));
        let a = Args::parse(&argv(["magent", "help", "web3"])).unwrap();
        assert!(matches!(a.command, Command::Web3Help));
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_new_parses_name_and_flags() {
        let a = Args::parse(&argv([
            "magent", "web3", "new", "alice", "--force", "--passphrase-env", "MY_PW",
            "--vault", "/tmp/vault.json",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::New(opts)) => {
                assert_eq!(opts.name, "alice");
                assert_eq!(opts.passphrase_env.as_deref(), Some("MY_PW"));
                assert!(opts.force);
                assert_eq!(opts.vault_override, Some(PathBuf::from("/tmp/vault.json")));
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_sign_parses_payload_and_output() {
        // Default payload is stdin; --payload <FILE> overrides it.
        let a = Args::parse(&argv([
            "magent", "web3", "sign", "alice", "--payload", "msg.bin",
            "--output", "env.json",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Sign(opts)) => {
                assert_eq!(opts.name, "alice");
                assert_eq!(opts.payload, PayloadSource::File(PathBuf::from("msg.bin")));
                assert_eq!(opts.output, Some(PathBuf::from("env.json")));
            }
            other => panic!("expected Sign, got {:?}", other),
        }
        // Without --payload, the parser defaults to stdin.
        let a = Args::parse(&argv(["magent", "web3", "sign", "alice"])).unwrap();
        match a.command {
            Command::Web3(Web3Action::Sign(opts)) => {
                assert_eq!(opts.payload, PayloadSource::Stdin);
                assert_eq!(opts.output, None);
            }
            other => panic!("expected Sign, got {:?}", other),
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_verify_requires_envelope_and_payload() {
        let a = Args::parse(&argv([
            "magent", "web3", "verify", "--payload", "msg.bin", "--envelope", "env.json",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Verify(opts)) => {
                assert_eq!(opts.payload, PayloadSource::File(PathBuf::from("msg.bin")));
                assert_eq!(opts.envelope, PathBuf::from("env.json"));
            }
            other => panic!("expected Verify, got {:?}", other),
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_did_and_pubkey_parse_inputs() {
        let a = Args::parse(&argv([
            "magent", "web3", "did", "--from-seed", "deadbeef",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Did(opts)) => {
                assert_eq!(opts.from_seed_hex.as_deref(), Some("deadbeef"));
                assert!(opts.from_pubkey_hex.is_none());
            }
            other => panic!("expected Did, got {:?}", other),
        }
        let a = Args::parse(&argv([
            "magent", "web3", "pubkey", "--from-seed", "deadbeef",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Pubkey(opts)) => {
                assert_eq!(opts.from_seed_hex.as_deref(), Some("deadbeef"));
            }
            other => panic!("expected Pubkey, got {:?}", other),
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_list_export_delete_round_trip() {
        let a = Args::parse(&argv(["magent", "web3", "list"])).unwrap();
        assert!(matches!(a.command, Command::Web3(Web3Action::List)));
        let a = Args::parse(&argv(["magent", "web3", "export", "alice"])).unwrap();
        match a.command {
            Command::Web3(Web3Action::Export(name)) => assert_eq!(name, "alice"),
            other => panic!("expected Export, got {:?}", other),
        }
        let a = Args::parse(&argv(["magent", "web3", "delete", "alice"])).unwrap();
        match a.command {
            Command::Web3(Web3Action::Delete(name)) => assert_eq!(name, "alice"),
            other => panic!("expected Delete, got {:?}", other),
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_rejects_unknown_subcommand() {
        let err = Args::parse(&argv(["magent", "web3", "frobnicate"])).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("web3 frobnicate"),
            "error must name the unknown subcommand: {}",
            msg
        );
    }

    #[cfg(feature = "web3")]
    #[test]
    fn web3_rejects_unknown_flag() {
        let err = Args::parse(&argv(["magent", "web3", "new", "alice", "--bogus"]))
            .unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("unknown flag") && msg.contains("--bogus"),
            "error must mention the bogus flag: {}",
            msg
        );
    }

    // -----------------------------------------------------------------------
    // `magent web3 chain|balance|tx|bind|vc` parser tests.
    //
    // These subcommands were listed in `web3 --help` while the parser still
    // answered `unknown subcommand`, so every documented invocation was
    // unreachable. Each test below drives the real `Args::parse` entry point
    // and asserts the action comes back typed, which is exactly the assertion
    // that failed before the parser arms existed.
    // -----------------------------------------------------------------------

    #[cfg(all(feature = "web3", feature = "blockchain"))]
    #[test]
    fn web3_blockchain_subcommands_route() {
        // Every name advertised by `web3 --help` must parse to a blockchain
        // action. Before the parser arms existed each of these returned
        // "web3 <name>" as an unknown subcommand.
        let a = Args::parse(&argv(["magent", "web3", "chain", "list"])).unwrap();
        assert!(matches!(
            a.command,
            Command::Web3(Web3Action::Blockchain(BlockchainAction::ChainList))
        ));

        let a = Args::parse(&argv(["magent", "web3", "chain", "info", "polygon"])).unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::ChainInfo(name))) => {
                assert_eq!(name, "polygon")
            }
            other => panic!("expected ChainInfo, got {:?}", other),
        }

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "balance",
            "0x0000000000000000000000000000000000000001",
            "--chain",
            "sepolia",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::Balance(opts))) => {
                assert_eq!(opts.address, "0x0000000000000000000000000000000000000001");
                assert_eq!(opts.chain.as_deref(), Some("sepolia"));
            }
            other => panic!("expected Balance, got {:?}", other),
        }

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "tx",
            "build",
            "0x0000000000000000000000000000000000000002",
            "--value",
            "1.5",
            "--data",
            "0xdeadbeef",
            "--chain",
            "sepolia",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::TxBuild(opts))) => {
                assert_eq!(opts.to, "0x0000000000000000000000000000000000000002");
                assert_eq!(opts.value.as_deref(), Some("1.5"));
                assert_eq!(opts.data.as_deref(), Some("0xdeadbeef"));
                assert_eq!(opts.chain, "sepolia");
            }
            other => panic!("expected TxBuild, got {:?}", other),
        }

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "bind",
            "create",
            "did:key:z6Mk",
            "--chain",
            "sepolia",
            "--from-seed",
            "0000000000000000000000000000000000000000000000000000000000000001",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::BindCreate(opts))) => {
                assert_eq!(opts.did, "did:key:z6Mk");
                assert_eq!(opts.chain, "sepolia");
                assert_eq!(opts.from_seed.len(), 64);
            }
            other => panic!("expected BindCreate, got {:?}", other),
        }

        let a = Args::parse(&argv(["magent", "web3", "bind", "verify", "proof.json"])).unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::BindVerify(path))) => {
                assert_eq!(path, PathBuf::from("proof.json"))
            }
            other => panic!("expected BindVerify, got {:?}", other),
        }

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "issue",
            "did:key:z6MkSubject",
            "--type",
            "Role",
            "--claim",
            "role=admin",
            "--from-seed",
            "0000000000000000000000000000000000000000000000000000000000000002",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::VcIssue(opts))) => {
                assert_eq!(opts.subject_did, "did:key:z6MkSubject");
                assert_eq!(opts.credential_type, "Role");
                assert_eq!(opts.claims.len(), 1);
                assert_eq!(opts.claims[0].0, "role");
                assert_eq!(opts.claims[0].1, serde_json::json!("admin"));
            }
            other => panic!("expected VcIssue, got {:?}", other),
        }

        let a = Args::parse(&argv(["magent", "web3", "vc", "verify", "vc.json"])).unwrap();
        assert!(matches!(
            a.command,
            Command::Web3(Web3Action::Blockchain(BlockchainAction::VcVerify(_)))
        ));

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "verify-presentation",
            "vp.json",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::VcVerifyPresentation {
                path,
                challenge,
            })) => {
                assert_eq!(path, PathBuf::from("vp.json"));
                assert!(challenge.is_none(), "no --challenge given");
            }
            other => panic!("expected VcVerifyPresentation, got {other:?}"),
        }

        // `--challenge` makes the verifier require its own nonce.
        let a = Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "verify-presentation",
            "vp.json",
            "--challenge",
            "nonce-7",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::VcVerifyPresentation {
                path,
                challenge,
            })) => {
                assert_eq!(path, PathBuf::from("vp.json"));
                assert_eq!(challenge.as_deref(), Some("nonce-7"));
            }
            other => panic!("expected VcVerifyPresentation, got {other:?}"),
        }

        // The subcommand takes <FILE> plus at most `--challenge <STR>`.
        assert!(Args::parse(&argv(["magent", "web3", "vc", "verify-presentation"])).is_err());
        assert!(Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "verify-presentation",
            "vp.json",
            "extra",
        ]))
        .is_err());
        assert!(Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "verify-presentation",
            "vp.json",
            "--challenge",
        ]))
        .is_err());

        let a = Args::parse(&argv([
            "magent",
            "web3",
            "vc",
            "present",
            "vc.json",
            "--challenge",
            "nonce-1",
            "--domain",
            "example.com",
            "--from-seed",
            "0000000000000000000000000000000000000000000000000000000000000003",
        ]))
        .unwrap();
        match a.command {
            Command::Web3(Web3Action::Blockchain(BlockchainAction::VcPresent(opts))) => {
                assert_eq!(opts.vc_file, PathBuf::from("vc.json"));
                assert_eq!(opts.challenge, "nonce-1");
                assert_eq!(opts.domain.as_deref(), Some("example.com"));
            }
            other => panic!("expected VcPresent, got {:?}", other),
        }
    }

    #[cfg(all(feature = "web3", feature = "blockchain"))]
    #[test]
    fn web3_blockchain_requires_arguments() {
        // The advertised commands must reject missing required arguments with
        // a usage error rather than falling through to the unknown-subcommand
        // path (which is what "not implemented" looked like).
        let cases: [Vec<String>; 5] = [
            argv(["magent", "web3", "balance"]),
            argv(["magent", "web3", "tx", "build"]),
            argv(["magent", "web3", "bind", "create", "did:key:z6Mk"]),
            argv(["magent", "web3", "vc", "issue", "did:key:z6Mk"]),
            argv(["magent", "web3", "vc", "present", "vc.json"]),
        ];
        for args in cases {
            let msg = format!("{:?}", Args::parse(&args).unwrap_err());
            assert!(
                msg.contains("Usage") && !msg.contains("unknown subcommand"),
                "missing args must be a usage error, got: {msg}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // `magent set-prompt sign` / `verify-signed` parser tests.
    //
    // The runner-side tests live in `tests/set_prompt_sign.rs` so they
    // get feature-gated on `web3_app`. Here we just exercise the
    // arg-parsing layer.
    // -----------------------------------------------------------------------

    #[cfg(feature = "web3_app")]
    #[test]
    fn parses_set_prompt_sign_minimal() {
        let a = Args::parse(&argv(["magent", "set-prompt", "sign", "agent-1"])).unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Sign(opts)) => {
                assert_eq!(opts.name, "agent-1");
                assert_eq!(opts.signer, "default");
                assert!(opts.signed_output.is_none());
                assert!(opts.passphrase_env.is_none());
                assert!(opts.not_before_unix.is_none());
                assert!(opts.not_after_unix.is_none());
            }
            other => panic!("expected Sign, got {:?}", other),
        }
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn parses_set_prompt_sign_with_all_flags() {
        let a = Args::parse(&argv([
            "magent",
            "set-prompt",
            "sign",
            "agent-1",
            "--signer",
            "ci-bot",
            "--signed-output",
            "/tmp/agent-1.signed.json",
            "--passphrase-env",
            "MAGENT_TEST_PASS",
            "--not-before",
            "100",
            "--not-after",
            "200",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::Sign(opts)) => {
                assert_eq!(opts.name, "agent-1");
                assert_eq!(opts.signer, "ci-bot");
                assert_eq!(
                    opts.signed_output.as_ref().unwrap().to_str().unwrap(),
                    "/tmp/agent-1.signed.json"
                );
                assert_eq!(opts.passphrase_env.as_deref(), Some("MAGENT_TEST_PASS"));
                assert_eq!(opts.not_before_unix, Some(100));
                assert_eq!(opts.not_after_unix, Some(200));
            }
            other => panic!("expected Sign, got {:?}", other),
        }
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn parses_set_prompt_sign_rejects_bad_timestamp() {
        let err = Args::parse(&argv([
            "magent",
            "set-prompt",
            "sign",
            "agent-1",
            "--not-before",
            "not-a-number",
        ]))
        .unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("--not-before"),
            "error must mention --not-before, got: {}",
            msg
        );
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn parses_set_prompt_verify_signed() {
        let a = Args::parse(&argv([
            "magent",
            "set-prompt",
            "verify-signed",
            "/tmp/agent-1.signed.json",
        ]))
        .unwrap();
        match a.command {
            Command::SetPrompt(prompt::SetPromptAction::VerifySigned(opts)) => {
                assert_eq!(
                    opts.path.to_str().unwrap(),
                    "/tmp/agent-1.signed.json"
                );
            }
            other => panic!("expected VerifySigned, got {:?}", other),
        }
    }

    #[cfg(feature = "web3_app")]
    #[test]
    fn parses_set_prompt_verify_signed_requires_path() {
        let err =
            Args::parse(&argv(["magent", "set-prompt", "verify-signed"])).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("verify-signed"),
            "error must mention the action, got: {}",
            msg
        );
    }
}
