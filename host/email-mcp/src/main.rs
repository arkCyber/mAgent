//! mAgent Email MCP Server
//!
//! A Model Context Protocol (MCP) server that exposes email tools
//! over the stdio transport. Designed to plug into `magent-core`
//! (via the host/gateway) or any MCP-compatible client (Claude
//! Desktop, Cursor, VS Code Copilot, etc.).
//!
//! ## Tools exposed
//!
//! | Tool             | Purpose                              |
//! |------------------|--------------------------------------|
//! | `list_inbox`     | List recent inbox messages           |
//! | `get_email`      | Fetch a single message by UID        |
//! | `search_emails`  | Subject/from substring search        |
//! | `send_email`     | Send a plain-text email via SMTP     |
//! | `mark_read`      | Mark a message as seen               |
//!
//! ## Configuration
//!
//! Credentials are read from environment variables (preferred) or
//! from `~/.config/magent/email-mcp.toml`:
//!
//! ```text
//! IMAP_HOST=imap.gmail.com
//! IMAP_PORT=993
//! IMAP_USER=user@example.com
//! IMAP_PASSWORD=...
//! SMTP_HOST=smtp.gmail.com
//! SMTP_PORT=587
//! SMTP_USER=user@example.com
//! SMTP_PASSWORD=...
//! ```
//!
//! ## Transport
//!
//! JSON-RPC 2.0 over newline-delimited stdio. Each line on stdin is
//! one JSON-RPC request; each line on stdout is one JSON-RPC
//! response (or notification). This is the standard MCP stdio
//! transport described in the Model Context Protocol spec.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod config;
mod imap_client;
mod jsonrpc;
mod smtp_client;
mod tools;

use std::sync::Arc;

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::imap_client::ImapSession;
use crate::smtp_client::SmtpSession;
use crate::tools::ToolRegistry;

/// Shared, mutable state passed into every tool call.
///
/// We hold the IMAP and SMTP sessions behind a mutex so the
/// stdio-driven request loop can serve tools serially. Most MCP
/// clients issue one request at a time, so contention is low.
struct AppState {
    /// Resolved configuration (credentials, hosts, ports).
    config: Config,
    /// Lazily-opened IMAP session. Opened on first `list_inbox` /
    /// `get_email` / `search_emails` / `mark_read` call.
    imap: Mutex<Option<ImapSession>>,
    /// Lazily-opened SMTP transport. Opened on first `send_email`.
    smtp: Mutex<Option<SmtpSession>>,
}

impl AppState {
    /// Construct a new `AppState` from a resolved config. Sessions
    /// start as `None` and are opened on demand.
    fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config,
            imap: Mutex::new(None),
            smtp: Mutex::new(None),
        })
    }

    /// Ensure the IMAP session is connected. Idempotent.
    async fn ensure_imap(&self) -> Result<(), anyhow::Error> {
        let mut guard = self.imap.lock().await;
        if guard.is_none() {
            let session = ImapSession::connect(&self.config).await?;
            *guard = Some(session);
        }
        Ok(())
    }

    /// Ensure the SMTP transport is connected. Idempotent.
    async fn ensure_smtp(&self) -> Result<(), anyhow::Error> {
        let mut guard = self.smtp.lock().await;
        if guard.is_none() {
            let session = SmtpSession::connect(&self.config).await?;
            *guard = Some(session);
        }
        Ok(())
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), anyhow::Error> {
    // Initialise logging — MCP clients ignore stderr, so log lines
    // are visible to operators without polluting the protocol
    // stream on stdout.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Load config from env / config file.
    let config = Config::load()?;
    log::info!(
        "magent-email-mcp starting: imap={}:{}, smtp={}:{}, user={}",
        config.imap_host,
        config.imap_port,
        config.smtp_host,
        config.smtp_port,
        config.user,
    );

    let state = AppState::new(config);
    let registry = ToolRegistry::new(state.clone());

    let stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();
    let mut lines = stdin.lines();

    // Announce capabilities. MCP `initialize` → `tools/list` is a
    // handshake the client drives; we only need to answer requests
    // here. The handshake is handled by `jsonrpc::dispatch`.
    log::info!("waiting for JSON-RPC requests on stdin");

    while let Some(line) = lines.next_line().await? {
        let trimmed: &str = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match jsonrpc::dispatch(trimmed, &registry).await {
            Ok(Some(response)) => {
                let payload = serde_json::to_string(&response)?;
                stdout.write_all(payload.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            Ok(None) => {
                // Notification (no `id` field) — no reply expected.
            }
            Err(err) => {
                log::error!("dispatch error: {err:#}");
                // We still need to write *something* so the client
                // doesn't hang. Emit a generic JSON-RPC parse error
                // when we couldn't even parse the request id.
                let err_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": "Parse error" }
                });
                let payload = serde_json::to_string(&err_response)?;
                stdout.write_all(payload.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    log::info!("stdin closed, shutting down");
    Ok(())
}
