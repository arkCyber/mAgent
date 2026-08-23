//! `magent` CLI entry point.
//!
//! Tiny on purpose: parse argv, dispatch to the subcommand, write a
//! human-readable result to stdout. All the actual logic lives in the
//! sibling modules — see `lib.rs` for the layout.

use std::io::Write as _;
use std::process::ExitCode;

use magent::{
    cli::{
        config_help_text, doctor_help_text, help_text, run_help_text, scheduler_help_text,
        set_prompt_help_text, summary_help_text, Args, Command, GlobalFlags,
    },
    config::ConfigCmd,
    doctor::DoctorCmd,
    output::{Output, OutputKind},
    prompt::SetPromptCmd,
    runner::RunCmd,
    scheduler::SchedulerCmd,
    summary::SummaryCmd,
};
#[cfg(feature = "web3")]
use magent::cli::web3_help_text;
#[cfg(feature = "web3")]
use magent::web3::{Web3Action, Web3Cmd};

/// Version string baked into the binary. Kept in lockstep with the
/// workspace `Cargo.toml` package version; bump both together.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialise the `log` facade + `env_logger` backend from CLI
/// flags. Idempotent — calling it twice (which shouldn't happen but
/// `cargo test` can hit if a test process spawns another instance)
/// silently no-ops on the second call so the global logger state
/// stays consistent.
fn init_logger(global: &GlobalFlags) {
    use std::sync::Once;
    static START: Once = Once::new();
    START.call_once(|| {
        let mut builder =
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off"));
        if let Some(level) = &global.log_level {
            // Parse the user-supplied level. Anything other than
            // the documented set is rejected with a clear error so
            // we never silently fall back to a wrong filter.
            match level.to_ascii_lowercase().as_str() {
                "off" => {
                    builder.filter_level(log::LevelFilter::Off);
                }
                "error" => {
                    builder.filter_level(log::LevelFilter::Error);
                }
                "warn" => {
                    builder.filter_level(log::LevelFilter::Warn);
                }
                "info" => {
                    builder.filter_level(log::LevelFilter::Info);
                }
                "debug" => {
                    builder.filter_level(log::LevelFilter::Debug);
                }
                "trace" => {
                    builder.filter_level(log::LevelFilter::Trace);
                }
                other => {
                    eprintln!(
                        "warning: invalid --log-level '{}', expected one of \
                         off|error|warn|info|debug|trace (falling back to env / default)",
                        other
                    );
                }
            }
        } else if global.verbose {
            builder.filter_level(log::LevelFilter::Debug);
        }
        // Always include the target module so the user can tell
        // which subsystem emitted the line.
        builder.format_module_path(true);
        let _ = builder.try_init();
    });
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();

    let args = match Args::parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            // Parser errors are user errors, not bugs — print them as
            // a usage hint and exit non-zero. We deliberately do NOT
            // honour `--json` here because the user couldn't have
            // parsed it successfully anyway.
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "error: {}", e);
            let _ = writeln!(stderr);
            let _ = writeln!(stderr, "{}", help_text(VERSION));
            return ExitCode::from(2);
        }
    };

    init_logger(&args.global);

    // Load the config file (if any) and apply its `io.*` defaults to
    // the global flags. CLI flags always win over the config file:
    // `--json` once parsed is a hard override, and so is `--no-color`.
    // The config's `io.no_color` and `io.json_default` only kick in
    // when the user didn't pass the corresponding flag.
    //
    // We deliberately load the config here rather than later in
    // `RunnerCmd::execute` because the resulting `Output` is used
    // by the *dispatch* path too (e.g. `magent config show` reads
    // its data through the same `out` writer). Loading the config
    // twice is fine — `with_defaults()` is free and the file is
    // tiny.
    let config_for_io = magent::config::load()
        .unwrap_or_else(|_| magent::config::ConfigRecord::with_defaults());
    let io_config = &config_for_io.io;
    // `--json` CLI flag and `io.json_default = true` both yield JSON.
// Collapsing the two identical arms is intentional: any future
// divergence (e.g. "CLI wins over config, but config enables a
// richer JSON variant") would surface as a semantic change here.
    let output_kind = if args.global.json || io_config.json_default {
        OutputKind::Json
    } else {
        OutputKind::Human
    };
    let no_color = args.global.no_color || io_config.no_color;
    let mut out = Output::new(output_kind, no_color);

    match args.command {
        Command::Help => {
            print!("{}", help_text(VERSION));
            ExitCode::SUCCESS
        }
        Command::RunHelp => {
            // `magent run --help` or `magent help run`.
            print!("{}", run_help_text());
            ExitCode::SUCCESS
        }
        Command::DoctorHelp => {
            // `magent doctor --help` or `magent help doctor`.
            print!("{}", doctor_help_text());
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("magent {}", VERSION);
            ExitCode::SUCCESS
        }
        Command::Run(mut opts) => match RunCmd::new(&mut opts).execute(&mut out) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                let _ = out.error(&e.to_string());
                ExitCode::FAILURE
            }
        },
        Command::Doctor => {
            // Doctor mirrors the same defaults `run` uses. Provider-
            // specific URLs come from the same env vars so users can
            // switch providers without re-typing the URL.
            let provider = std::env::var("MAGENT_PROVIDER")
                .unwrap_or_else(|_| "ollama".to_string());
            let url = std::env::var("OLLAMA_HOST")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
            let model = std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| "llama3.2".to_string());
            let deepseek_url = std::env::var("DEEPSEEK_HOST")
                .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string());
            let api_key = std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .or_else(|| std::env::var("OLLAMA_API_KEY").ok());
            let cmd = DoctorCmd::new(
                &provider,
                &url,
                &model,
                &deepseek_url,
                api_key.as_deref(),
            );
            if cmd.execute(&mut out) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::SetPromptHelp => {
            // `magent set-prompt --help` or `magent help set-prompt`.
            print!("{}", set_prompt_help_text());
            ExitCode::SUCCESS
        }
        Command::SetPrompt(action) => match SetPromptCmd::new(&action).execute(&mut out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // All prompt-store errors are user errors (bad name,
                // missing file, IO failure creating the dir, …). Print
                // the diagnostic and exit non-zero so CI can pick it up.
                let _ = out.error(&e.to_string());
                ExitCode::from(2)
            }
        },
        Command::SummaryHelp => {
            // `magent summary --help` or `magent help summary`.
            print!("{}", summary_help_text());
            ExitCode::SUCCESS
        }
        Command::Summary(action) => match SummaryCmd::new(&action).execute(&mut out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // Same error reporting pattern as `SetPromptCmd` —
                // summary-store errors are always user-facing
                // diagnostics (bad topic, IO failure, invalid
                // JSON in --from) so print and exit non-zero.
                let _ = out.error(&e.to_string());
                ExitCode::from(2)
            }
        },
        Command::ConfigHelp => {
            // `magent config --help` or `magent help config`.
            print!("{}", config_help_text());
            ExitCode::SUCCESS
        }
        Command::Config(action) => match ConfigCmd::new(&action).execute(&mut out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // Same error reporting pattern as `SetPromptCmd` —
                // config errors are always user-facing diagnostics
                // (no permission to write, bad key, etc.) so print
                // and exit non-zero.
                let _ = out.error(&e.to_string());
                ExitCode::from(2)
            }
        },
        Command::SchedulerHelp => {
            // `magent scheduler --help` or `magent help scheduler`.
            print!("{}", scheduler_help_text());
            ExitCode::SUCCESS
        }
        Command::Scheduler(action) => match SchedulerCmd::new(action).execute(&mut out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // `SchedulerError::Interrupted` is the clean
                // shutdown path (SIGINT / SIGTERM) — print a
                // short notice and exit 130, the conventional
                // "killed by SIGINT" code, so a CI script can
                // tell a clean stop from a real failure.
                let code = match &e {
                    magent::scheduler::SchedulerError::Interrupted => 130,
                    _ => 2,
                };
                let _ = out.error(&e.to_string());
                ExitCode::from(code)
            }
        },
        #[cfg(feature = "web3")]
        Command::Web3(action) => {
            // The web3 subcommand reads the passphrase through a
            // closure so `main.rs` doesn't have to commit to a
            // specific prompting strategy. `passphrase_env` is the
            // usual case: read from `$<env_var>`. If the user
            // didn't pick an env var, fall back to the canonical
            // `MAGENT_WEB3_PASSPHRASE` so a single secret can drive
            // multiple invocations without re-typing. An empty
            // value is treated as "ask the operator" — the prompt
            // path lives in a future iteration; for now we surface
            // a friendly error so the caller knows what to do.
            // Resolve the passphrase env var up-front (outside the closure)
            // so the closure can own its `String` and the action's
            // lifetime doesn't bleed into the FnMut capture.
            let env_var: String = match &action {
                Web3Action::New(opts) => opts
                    .passphrase_env
                    .clone()
                    .unwrap_or_else(|| "MAGENT_WEB3_PASSPHRASE".to_string()),
                Web3Action::Sign(opts) => opts
                    .passphrase_env
                    .clone()
                    .unwrap_or_else(|| "MAGENT_WEB3_PASSPHRASE".to_string()),
                _ => "MAGENT_WEB3_PASSPHRASE".to_string(),
            };
            let mut cmd = Web3Cmd::new(&action, Box::new(move |var| {
                let key = if var.is_empty() { env_var.as_str() } else { var };
                std::env::var(key).map_err(|_| {
                    magent::web3::Web3CliError::Aead(format!(
                        "passphrase env var ${} is not set (use --passphrase-env or export the variable)",
                        key
                    ))
                })
            }));
            match cmd.execute(&mut out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    let _ = out.error(&e.to_string());
                    ExitCode::from(2)
                }
            }
        }
        #[cfg(feature = "web3")]
        Command::Web3Help => {
            print!("{}", web3_help_text());
            ExitCode::SUCCESS
        }
    }
}
