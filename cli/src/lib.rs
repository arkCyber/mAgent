//! mAgent command-line tool — library crate.
//!
//! The CLI is split into a thin library (`lib.rs`) and a tiny `main.rs`
//! that calls into it. This lets us write unit tests for the argument
//! parser and command dispatcher without spawning a subprocess, and
//! gives `cargo doc` something to render.
//!
//! ## Layout
//!
//! ```text
//! cli/src/
//! ├── lib.rs         ← re-exports the public API
//! ├── main.rs        ← `fn main()` — argument parsing + dispatch
//! ├── cli.rs         ← argv parsing (no external deps)
//! ├── output.rs      ← human-readable / JSON output formatting
//! ├── runner.rs      ← `RunCmd` — wires the agent runner + executor
//! ├── email_executor.rs ← `CompositeExecutor`: SimulatorExecutor + optional MCP email backend
//! ├── prompt.rs      ← `SetPromptCmd` — manages stored system prompts
//! ├── scheduler.rs   ← concurrency-limited task queue
//! ├── doctor.rs      ← `doctor` subcommand — environment sanity checks
//! └── web3.rs        ← `Web3Cmd` — Web3 identity / sign / verify (gated on `web3`)
//! ```
//!
//! The `run` subcommand is the headline feature: it builds a
//! `RealAgentRunner<CompositeExecutor>`, optionally wires in the
//! `mcp-tool-executor` email backend via `--email-tools`, and drives
//! the ReAct loop against an Ollama or DeepSeek backend.

pub mod cli;
pub mod config;
pub mod doctor;
pub mod email_executor;
pub mod output;
pub mod prompt;
pub mod runner;
pub mod scheduler;
pub mod summary;
#[cfg(feature = "web3")]
pub mod web3;
#[cfg(feature = "web3")]
pub mod blockchain_executor;
#[cfg(feature = "blockchain")]
pub mod web3_blockchain;

pub use cli::{Args, Command, GlobalFlags, RunOptions};
pub use config::{ConfigAction, ConfigCmd, ConfigRecord};
#[cfg(feature = "web3")]
pub use web3::{Web3Action, Web3Cmd, Web3NewOptions, Web3SignOptions, Web3VerifyOptions};
pub use prompt::{
    render_template, render_template_with_warnings, SetPromptAction, SetPromptCmd,
    SetPromptImportOptions, SetPromptSetOptions, SetPromptTemplateOptions,
};
pub use runner::RunError;
