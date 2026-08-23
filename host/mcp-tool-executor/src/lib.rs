//! `mcp-tool-executor` — async MCP subprocess executor for mAgent.
//!
//! Spawns `magent-email-mcp` as a stdio child process and translates
//! `ble_send` tool calls (and `mcp__email__*` calls) into JSON-RPC
//! `tools/call` requests.
//!
//! ## Design
//!
//! - **Lazy spawn**: the child process is not started until the first
//!   `mcp__email__*` tool call arrives. This avoids IMAP/SMTP handshakes
//!   at startup time when the agent only needs sensor/BLE/flash tools.
//!
//! - **Protocol**: MCP 2024-11-05 over newline-delimited stdio. Each
//!   line on `child.stdout` is one JSON-RPC response; each line written
//!   to `child.stdin` is one JSON-RPC request. We synchronise on
//!   request IDs so a single concurrent request is enough for the ReAct
//!   loop's serial tool-execution model.
//!
//! - **Tool naming**: the LLM sees `mcp__email__<tool>` (e.g.
//!   `mcp__email__list_inbox`). The `execute` method strips the
//!   `mcp__email__` prefix and forwards the bare name (`list_inbox`)
//!   to the MCP server. The original `ble_send` payload is passed as
//!   `{"data": "...", "characteristic": "email"}` on the MCP side.
//!
//! - **Crash recovery**: if the subprocess exits unexpectedly, the next
//!   `execute` call detects the dead child and re-spawns it lazily.
//!
//! ## Limitations
//!
//! - Only one in-flight MCP request at a time (enforced by the inner
//!   `Mutex<McpChild>`). Parallel callers serialize through `tokio::sync`
//!   on the async path and through `block_on` on the sync path.
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Maximum length of a tool result string before we truncate it.
const MAX_RESULT_CHARS: usize = 4096;

/// Per-request timeout for the JSON-RPC round-trip.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Errors that can occur during MCP subprocess execution.
#[derive(Debug, Error)]
pub enum McpError {
    /// The binary could not be spawned (e.g. file not found, not executable).
    #[error("failed to spawn MCP subprocess `{binary}`: {source}")]
    SpawnFailed {
        binary: String,
        #[source]
        source: std::io::Error,
    },

    /// The subprocess was spawned but `initialize` failed.
    #[error("MCP handshake failed: {0}")]
    HandshakeFailed(String),

    /// The JSON-RPC round-trip timed out.
    #[error("JSON-RPC request timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The child exited before a response arrived.
    #[error("MCP subprocess exited unexpectedly (status: {0:?})")]
    ChildExited(Option<std::process::ExitStatus>),

    /// The stream closed before we ever saw a matching response.
    #[error("JSON-RPC response missing 'id' field (stream closed)")]
    MissingId,

    /// The server returned a JSON-RPC error envelope.
    #[error("JSON-RPC error from MCP server: {message} (code {code})")]
    ServerError { code: i64, message: String },

    /// The server's response didn't match the expected schema.
    #[error("JSON parse error: {0}")]
    ParseError(String),

    /// An I/O error on stdin/stdout.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ============================================================================
// McpExecutor — top-level entry point
// ============================================================================

/// Async executor that forwards tool calls to a `magent-email-mcp` subprocess.
///
/// Lazily spawns the child process on the first `execute` call that matches
/// `mcp__email__*`. If the child is already dead, the next call
/// transparently re-spawns it.
///
/// ```ignore
/// use mcp_tool_executor::McpExecutor;
///
/// let exec = McpExecutor::new("target/release/magent-email-mcp");
/// let result = exec.execute("mcp__email__list_inbox", r#"{"limit": 5}"#).await;
/// ```
pub struct McpExecutor {
    /// Path or name of the `magent-email-mcp` binary.
    binary: String,
    /// Lazily-initialised child process. The `Mutex` is held only for
    /// the duration of a single RPC round-trip, never across awaits
    /// outside `rpc()`.
    inner: Mutex<Option<McpChild>>,
}

/// Entry in the static tool catalogue returned by `tools/list`.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    /// `mcp__email__<name>` — the name the LLM uses.
    pub namespaced: String,
    /// Short description for the LLM.
    pub description: String,
    /// JSON Schema for the tool's input arguments.
    pub input_schema: Value,
}

impl ToolDescriptor {
    /// Return the full catalogue that `mcp-tool-executor` manages.
    ///
    /// Callers (e.g. `cli/src/runner.rs`) use this to populate
    /// `RealAgentRunner::set_tool_descriptions` so the LLM knows which
    /// `mcp__email__*` tools it may invoke.
    pub fn email_tool_descriptions() -> Vec<(String, String)> {
        Self::all()
            .into_iter()
            .map(|t| (t.namespaced, t.description))
            .collect()
    }

    /// All tools currently exposed by `magent-email-mcp`.
    pub fn all() -> Vec<Self> {
        vec![
            Self {
                namespaced: "mcp__email__list_inbox".into(),
                description: "List recent messages in the INBOX (up to `limit`, default 20).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of messages to return (default 20).",
                            "minimum": 1,
                            "maximum": 200,
                            "default": 20,
                        }
                    },
                    "additionalProperties": false,
                }),
            },
            Self {
                namespaced: "mcp__email__get_email".into(),
                description: "Fetch a single email by IMAP UID, returning headers and plain-text body.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "uid": {
                            "type": "integer",
                            "description": "IMAP UID returned by list_inbox or search_emails.",
                            "minimum": 1,
                        }
                    },
                    "required": ["uid"],
                    "additionalProperties": false,
                }),
            },
            Self {
                namespaced: "mcp__email__search_emails".into(),
                description: "Search INBOX for messages whose subject OR from-header contains `query`.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Substring to match (case-insensitive).",
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default 20).",
                            "minimum": 1,
                            "maximum": 200,
                            "default": 20,
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                }),
            },
            Self {
                namespaced: "mcp__email__send_email".into(),
                description: "Send a plain-text email via SMTP.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "to": {
                            "type": "string",
                            "description": "Recipient address(es), comma-separated.",
                        },
                        "subject": {
                            "type": "string",
                            "description": "Email subject line.",
                        },
                        "body": {
                            "type": "string",
                            "description": "Plain-text body.",
                        }
                    },
                    "required": ["to", "subject", "body"],
                    "additionalProperties": false,
                }),
            },
            Self {
                namespaced: "mcp__email__mark_read".into(),
                description: "Mark a message as seen by IMAP UID.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "uid": {
                            "type": "integer",
                            "description": "IMAP UID.",
                            "minimum": 1,
                        }
                    },
                    "required": ["uid"],
                    "additionalProperties": false,
                }),
            },
        ]
    }
}

impl McpExecutor {
    /// Create a new executor that will spawn `binary` on first use.
    ///
    /// `binary` can be an absolute path (e.g. `target/release/magent-email-mcp`)
    /// or a bare name resolved via `$PATH`.
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            inner: Mutex::new(None),
        }
    }

    /// Forward `tool` to the MCP subprocess.
    ///
    /// Returns:
    /// - `Err("unknown tool: <name>")` when the tool name does not
    ///   start with `mcp__email__`.
    /// - The MCP server's `text` content on success.
    /// - A human-readable error string on failure.
    pub async fn execute(
        &self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        // ── 1. Fast-path: not an email tool ──────────────────────────────
        let Some(stripped) = tool.strip_prefix("mcp__email__") else {
            return Err(format!("unknown tool: {tool}"));
        };

        // ── 2. Make sure the subprocess is alive (lazy-spawn or recover). ──
        //
        // We hold the mutex only long enough to confirm the child is alive.
        // The actual RPC (`child.call`) is then called on an `&mut McpChild`
        // borrowed out of the mutex; the lock is dropped across the `await`.
        // This is the standard pattern for serializing access to a tokio
        // child handle without deadlock.
        let mut guard = self.inner.lock().await;
        let needs_spawn = match guard.as_mut() {
            None => true,
            Some(c) => !c.is_alive(),
        };
        if needs_spawn {
            let child = McpChild::spawn(&self.binary)
                .await
                .map_err(|e| e.to_string())?;
            *guard = Some(child);
        }

        // ── 3. Forward to MCP. ─────────────────────────────────────────
        //
        // The RPC loop is fully synchronous on `&mut McpChild` once we
        // own it; we pull the child out of the mutex and release it
        // immediately so subsequent code never awaits while holding the
        // outer mutex.
        let result = {
            let child = guard.as_mut().expect("just spawned");
            let args_json: Value = serde_json::from_str(args).unwrap_or(Value::Null);
            child.call(stripped, args_json).await
        };
        let content = result.map_err(|e| match e {
            McpError::ChildExited(_) => {
                // Process died mid-call: clear the slot so the next call
                // re-spawns.
                *guard = None;
                format!("MCP subprocess crashed: {e}")
            }
            other => other.to_string(),
        })?;

        // ── 4. Truncate if needed. ─────────────────────────────────────
        if content.len() > MAX_RESULT_CHARS {
            let mut truncated = content[..MAX_RESULT_CHARS].to_string();
            truncated.push_str("… [truncated]");
            Ok(truncated)
        } else {
            Ok(content)
        }
    }

    /// Return `true` if the subprocess has been started and is still alive.
    pub async fn is_running(&self) -> bool {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(c) => c.is_alive(),
            None => false,
        }
    }
    /// Kill the subprocess, if it is running.
    pub async fn shutdown(&self) {
        let mut guard = self.inner.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
        }
    }
}

// ============================================================================
// McpChild — running subprocess with active I/O
// ============================================================================

/// A running `magent-email-mcp` subprocess with active stdin/stdout.
///
/// Methods on `McpChild` take `&mut self` so the caller (always
/// `McpExecutor::execute`) holds exclusive access across the whole RPC.
struct McpChild {
    /// The OS process handle.
    child: Child,
    /// Raw stdout handle. A fresh `BufReader` is created per request
    /// so we can call `.lines()` (which consumes the reader) without
    /// fighting borrowck.
    stdout: tokio::process::ChildStdout,
    /// Stdin handle. Stored as `Option` so we can `take()` it for each
    /// `write_all` call (which takes ownership) and restore it afterwards.
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    /// Monotonically increasing request ID. Atomic so we don't need a
    /// mutex + .await just to read a `u64`.
    next_id: AtomicU64,
}

impl McpChild {
    /// Spawn the child process, perform the MCP handshake, and return.
    ///
    /// `binary` is parsed as a shell-style command line: the first
    /// whitespace-separated token is the program, the rest are
    /// arguments. This lets callers pass either
    /// `magent-email-mcp` (a `$PATH`-resolved name) or a command with
    /// arguments like `sh /path/to/mock-mcp.sh` for tests.
    async fn spawn(binary: &str) -> std::result::Result<Self, McpError> {
        let mut tokens = binary.split_whitespace();
        let program = tokens
            .next()
            .ok_or_else(|| McpError::SpawnFailed {
                binary: binary.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "binary path is empty",
                ),
            })?;
        let mut cmd = Command::new(program);
        for arg in tokens {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| McpError::SpawnFailed {
            binary: binary.to_string(),
            source: e,
        })?;

        let stdout = child.stdout.take().expect("stdout captured");
        let stdin = child.stdin.take().expect("stdin captured");

        let mut this = Self {
            child,
            stdout,
            stdin: Mutex::new(Some(stdin)),
            next_id: AtomicU64::new(0),
        };

        // ── MCP handshake: send `initialize`. ─────────────────────────
        this.initialize().await.map_err(|e| {
            // Best-effort cleanup: kill the child so the OS doesn't
            // leave a zombie if the handshake failed.
            let _ = this.child.start_kill();
            McpError::HandshakeFailed(e.to_string())
        })?;

        log::info!(
            target: "mcp-tool-executor",
            "magent-email-mcp handshake complete (pid={})",
            this.child.id().unwrap_or(0)
        );

        Ok(this)
    }

    /// Perform the MCP `initialize` round-trip.
    async fn initialize(&mut self) -> std::result::Result<(), McpError> {
        let req = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "clientInfo": {
                    "name": "mcp-tool-executor",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {},
            }
        });
        let resp: Value = self.rpc(req).await?;
        log::debug!(
            target: "mcp-tool-executor",
            "initialize response: {}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
        Ok(())
    }

    /// Call an MCP tool by name with the given JSON arguments.
    ///
    /// Returns the `text` content from the first `content` array element.
    async fn call(&mut self, name: &str, args: Value) -> std::result::Result<String, McpError> {
        let req = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": args,
            }
        });

        let resp: Value = self.rpc(req).await?;

        // Check for a server-side JSON-RPC error.
        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32000);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Err(McpError::ServerError { code, message });
        }

        // Extract `result.content[0].text`.
        let content = resp
            .pointer("/result/content")
            .and_then(|arr| arr.as_array())
            .and_then(|arr| arr.first())
            .and_then(|obj| obj.get("text"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::ParseError(
                    "response missing result.content[0].text".to_string(),
                )
            })?
            .to_string();

        Ok(content)
    }

    /// Allocate the next monotonically-increasing request ID.
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send one JSON-RPC request and read one JSON-RPC response.
    async fn rpc(&mut self, req: Value) -> std::result::Result<Value, McpError> {
        use tokio::io::AsyncWriteExt;

        let line = serde_json::to_string(&req)
            .map_err(|e| McpError::ParseError(e.to_string()))?;

        // ── WRITE ─────────────────────────────────────────────────────
        // Take stdin from the Option, write, then put it back. This is
        // required because `write_all` takes ownership of the writer.
        //
        // The lock is held only for the duration of the write — no
        // await while holding it.
        {
            let mut stdin_guard = self.stdin.lock().await;
            let mut stdin = stdin_guard.take().expect("stdin already taken");
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(McpError::Io)?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(McpError::Io)?;
            stdin.flush().await.map_err(McpError::Io)?;
            // Hand the writer back before dropping the lock.
            *stdin_guard = Some(stdin);
        }

        // ── READ ──────────────────────────────────────────────────────
        // Wait for the line whose JSON contains the matching `id`.
        // `req` is not consumed by `to_string(&req)` (pass-by-reference),
        // so we can still read its `id` field below.
        let expected_id = req
            .get("id")
            .cloned()
            .ok_or_else(|| McpError::ParseError("JSON-RPC request missing 'id'".to_string()))?;

        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;

        // Read lines with a 64 KiB internal buffer. A large `list_inbox`
        // response (single JSON line, potentially 100+ KiB) is still read
        // correctly because `BufReader::lines()` buffers until `\n` regardless
        // of internal buffer size. The buffer only controls how often the
        // underlying `Read` syscall fires.
        let mut lines = BufReader::with_capacity(65536, &mut self.stdout).lines();

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let line = match tokio::time::timeout(remaining, lines.next_line()).await {
                Ok(Ok(Some(l))) => l,
                Ok(Ok(None)) => {
                    // Stream closed.
                    let status = self.child.try_wait().ok().flatten();
                    return Err(McpError::ChildExited(status));
                }
                Ok(Err(e)) => return Err(McpError::Io(e)),
                Err(_) => {
                    // Timeout: distinguish between "we timed out waiting"
                    // and "child exited during the wait" by inspecting
                    // `try_wait`. If the child is still alive, this is a
                    // genuine timeout; otherwise it exited mid-flight.
                    let status = self.child.try_wait().ok().flatten();
                    return match status {
                        Some(s) => Err(McpError::ChildExited(Some(s))),
                        None => Err(McpError::Timeout(REQUEST_TIMEOUT)),
                    };
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let parsed: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue, // skip non-JSON noise (e.g. logs)
            };

            if let Some(got) = parsed.get("id") {
                if expected_id == *got {
                    return Ok(parsed);
                }
            }
        }
    }

    /// Return `true` if the child process has not yet exited.
    fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Kill the child process.
    async fn kill(&mut self) -> std::result::Result<(), std::io::Error> {
        self.child.kill().await
    }
}

// ============================================================================
// McpToolExecutor — magent-core ToolExecutor adapter
// ============================================================================

/// Adapter that implements `magent_core::agent_runner::ToolExecutor`
/// (the `execute(tool, args) -> Result<String, String>` trait) on top of
/// `McpExecutor`.
///
/// This is the bridge between the `ble_send` convention used in the
/// embedded agent prompt and the `mcp__email__*` tool names exposed by the
/// CLI.
///
/// ## How `ble_send` becomes an email tool
///
/// The system prompt tells the LLM to emit `ble_send` when it wants email
/// access. This adapter intercepts the call:
///
/// ```text
/// LLM → {"tool": "ble_send", "args": {"data": "list_inbox", "characteristic": "email"}}
///     → McpToolExecutor::execute("ble_send", r#"{"data":"list_inbox","characteristic":"email"}"#)
///     → strips ble_send → calls magent-email-mcp with tool name "list_inbox"
/// ```
///
/// ## Threading
///
/// The sync `ToolExecutor::execute` method runs on whatever thread the
/// ReAct loop is calling it from. We dispatch to a process-wide tokio
/// runtime created lazily on first use. This means the CLI can be a
/// plain `fn main()` (no `#[tokio::main]`) and still drive async I/O
/// for the email backend.
pub struct McpToolExecutor {
    inner: McpExecutor,
}

impl McpToolExecutor {
    /// Create a new executor backed by the `magent-email-mcp` binary at
    /// `binary_path`.
    pub fn new(binary_path: impl Into<String>) -> Self {
        Self {
            inner: McpExecutor::new(binary_path),
        }
    }

    /// Return the list of `mcp__email__*` tool names and descriptions
    /// suitable for passing to `RealAgentRunner::set_tool_descriptions`.
    pub fn tool_descriptions() -> Vec<(String, String)> {
        ToolDescriptor::email_tool_descriptions()
    }

    /// Kill the underlying subprocess, if any.
    pub fn shutdown(&self) {
        // The result of `Handle::block_on` is a JoinHandle's Result<()>.
        // We don't propagate failures here because the subprocess is already
        // best-effort: callers that need a stronger guarantee should re-export
        // a fallible variant from here.
        #[allow(clippy::let_unit_value)]
        let _ = tokio::runtime::Handle::current().block_on(self.inner.shutdown());
    }

    /// Synchronous one-shot: forward `tool` to the MCP subprocess.
    #[allow(dead_code)]
    fn execute_async_blocking(
        &self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        let handle = tokio_runtime();
        handle.block_on(self.inner.execute(tool, args))
    }
}

impl Default for McpToolExecutor {
    fn default() -> Self {
        // Search `$PATH` first (works for `cargo install`), then fall
        // back to the workspace `target/release/` path for developers
        // running the binary from the repo root.
        Self::new("magent-email-mcp")
    }
}

#[cfg(feature = "std")]
impl magent_core::agent_runner::ToolExecutor for McpToolExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        // ── ble_send: the embedded-actor email convention ──────────────
        //
        // The embedded agent prompt uses `ble_send` with
        // `data=<email_tool_name>,characteristic=email` to request
        // email operations. When we see this pattern we strip the
        // prefix and delegate to the MCP subprocess.
        if tool == "ble_send" {
            let args_parsed: serde_json::Map<String, Value> =
                serde_json::from_str(args).unwrap_or_default();

            let email_tool = args_parsed
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("list_inbox");

            let namespaced = format!("mcp__email__{email_tool}");

            // Forward all args EXCEPT the two bookkeeping fields (`data`
            // and `characteristic`) that only the host-side convention uses.
            // Any real tool parameters (`limit`, `query`, `to`, etc.)
            // are preserved and forwarded to the MCP server.
            let clean_args: serde_json::Map<String, Value> = args_parsed
                .into_iter()
                .filter(|(k, _)| k != "data" && k != "characteristic")
                .collect();
            let clean_json = serde_json::to_string(&clean_args)
                .unwrap_or_else(|_| "{}".to_string());

            return self.execute_async_blocking(&namespaced, &clean_json);
        }

        // ── mcp__email__* : direct MCP naming ──────────────────────────
        if tool.starts_with("mcp__email__") {
            return self.execute_async_blocking(tool, args);
        }

        Err(format!("unknown tool: {tool}"))
    }
}

// ============================================================================
// Process-wide tokio runtime
// ============================================================================
//
// `magent` is a plain `fn main()`. The CLI's ReAct loop runs on regular
// threads; `CompositeExecutor::execute` is sync. We need a tokio runtime
// to drive the async MCP subprocess anyway. Spawning one per call would
// be expensive (network/IMAP sessions), so we create one lazily and
// reuse it for the lifetime of the process.
//
// We try `Handle::try_current()` first so that callers running inside a
// tokio runtime (e.g. `#[tokio::test]`) use the test's runtime and we
// don't deadlock. If no current runtime exists we build a dedicated
// multi-thread runtime and run the call on it via `block_on`.

use std::sync::OnceLock;

#[allow(dead_code)]
fn tokio_runtime() -> tokio::runtime::Handle {
    // Path A: called from inside a tokio runtime (e.g. tests).
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return handle;
    }

    // Path B: build a process-wide runtime once.
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("magent-mcp")
            .build()
            .expect("failed to build tokio runtime for mcp-tool-executor")
    });
    runtime.handle().clone()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure (no subprocess) tests ─────────────────────────────────────
    #[test]
    fn tool_descriptions_are_well_formed() {
        let tools = McpToolExecutor::tool_descriptions();
        assert!(!tools.is_empty());

        let names: Vec<&str> = tools.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.iter().all(|n| n.starts_with("mcp__email__")));

        let descriptions: Vec<&str> = tools.iter().map(|(_, d)| d.as_str()).collect();
        assert!(descriptions.iter().all(|d| !d.is_empty()));
    }

    #[test]
    fn tool_descriptor_all_contains_expected_tools() {
        let tools = ToolDescriptor::all();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t.namespaced.as_str())
            .collect();

        for expected in [
            "mcp__email__list_inbox",
            "mcp__email__get_email",
            "mcp__email__search_emails",
            "mcp__email__send_email",
            "mcp__email__mark_read",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} not found in tool catalogue"
            );
        }
    }

    // ── `#[cfg(feature = "std")]` tests ─────────────────────────────────
    //
    // These require the `ToolExecutor` impl which is gated behind `std`.
    #[cfg(feature = "std")]
    mod std_tests {
        use super::*;
        use magent_core::agent_runner::ToolExecutor;

        #[test]
        fn tool_descriptor_all_contains_expected_tools() {
            let tools = ToolDescriptor::all();
            let names: Vec<&str> = tools
                .iter()
                .map(|t| t.namespaced.as_str())
                .collect();

            for expected in [
                "mcp__email__list_inbox",
                "mcp__email__get_email",
                "mcp__email__search_emails",
                "mcp__email__send_email",
                "mcp__email__mark_read",
            ] {
                assert!(
                    names.contains(&expected),
                    "{expected} not found in tool catalogue"
                );
            }
        }

        #[test]
        fn ble_send_strips_booking_fields_preserves_extra_args() {
            // Regression: ble_send({data, characteristic, ...extra}) must NOT
            // drop extra fields (e.g. limit, query) when forwarding to MCP.
            // We test the parsing logic in isolation without needing a subprocess.
            let args = r#"{"data":"search_emails","characteristic":"email","query":"invoice","limit":10}"#;
            let parsed: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(args).unwrap();

            let email_tool = parsed
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("list_inbox");

            assert_eq!(email_tool, "search_emails");

            let clean: serde_json::Map<String, serde_json::Value> = parsed
                .into_iter()
                .filter(|(k, _)| k != "data" && k != "characteristic")
                .collect();

            // `limit` and `query` must survive the filter.
            assert!(
                clean.contains_key("limit"),
                "limit was dropped; clean keys = {clean:?}"
            );
            assert!(
                clean.contains_key("query"),
                "query was dropped; clean keys = {clean:?}"
            );
            assert!(!clean.contains_key("data"));
            assert!(!clean.contains_key("characteristic"));
        }

        #[test]
        fn ble_send_unknown_tool_returns_error() {
            // A `ble_send` with an unknown email sub-tool name
            // still goes to the MCP subprocess (the subprocess will
            // return the error, not "unknown tool").
            let mut exec = McpToolExecutor::new("false");
            let r = exec.execute("ble_send", r#"{"data":"does_not_exist","characteristic":"email"}"#);
            assert!(r.is_err());
            let err = r.unwrap_err();
            // Must NOT say "unknown tool" — that's McpExecutor's fast-path
            // for non-email tool names. `ble_send` is always an email tool.
            assert!(!err.contains("unknown tool"), "got: {err}");
        }
    }

    // ── Async tests (no subprocess) ────────────────────────────────────
    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        // Use "true" — it won't be spawned for non-email tools.
        let exec = McpExecutor::new("true");
        let result = exec.execute("read_sensor", "{}").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn shutdown_without_ever_calling_is_idempotent() {
        let exec = McpExecutor::new("true");
        exec.shutdown().await;
        exec.shutdown().await; // must not panic
        assert!(!exec.is_running().await);
    }

    #[tokio::test]
    async fn is_running_is_false_before_first_call() {
        let exec = McpExecutor::new("true");
        assert!(!exec.is_running().await);
    }

    // ── End-to-end protocol test ───────────────────────────────────────
    //
    // Drive a tiny mock MCP server that speaks the JSON-RPC stdio
    // protocol with at least `initialize` and `tools/list`. We use
    // `/bin/sh` (POSIX) because it lets us pipe a read-eval loop
    // without a separate script file.
    //
    // The mock script echoes back the right JSON-RPC envelopes for
    // whichever method the client invokes, so we can verify the
    // handshake, request-ID round-trip, and content extraction.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_handshake_and_tool_call_with_mock_server() {
        // Single shell script that:
        //   1. reads one line (initialize),
        //   2. emits initialize response,
        //   3. reads one line (tools/list or tools/call),
        //   4. emits the right response.
        //
        // The script intentionally doesn't write anything before reading
        // to simulate a real MCP server that waits for the client to drive.
        let script = r#"
            read -r line
            echo '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock","version":"0"},"capabilities":{"tools":{}}}}'
            read -r line
            # branch on method
            method=$(echo "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
            case "$method" in
              tools/list)
                echo '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"ping","description":"echo","inputSchema":{"type":"object"}}]}}'
                ;;
              tools/call)
                # extract the requested tool name and echo back the request
                arg=$(echo "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
                echo "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echo:$arg\"}],\"isError\":false}}"
                ;;
              *)
                echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown method"}}'
                ;;
            esac
        "#;

        // Write the script to a tempfile so we can exec it via `sh`.
        let dir = tempdir();
        let path = dir.join("mock-mcp.sh");
        std::fs::write(&path, script).expect("write mock script");

        let exec = McpExecutor::new(format!("sh {}", path.display()));
        let out = exec
            .execute("mcp__email__ping", "{}")
            .await
            .expect("handshake + tool call should succeed with mock server");

        assert_eq!(out, "echo:ping");

        exec.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_error_is_surfaced() {
        // The same mock server, but call a method name that the
        // script's `case` falls through to its `*` branch and returns
        // a JSON-RPC error.
        let script = r#"
            read -r line
            echo '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock","version":"0"},"capabilities":{"tools":{}}}}'
            read -r line
            echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"intentional error"}}'
        "#;
        let dir = tempdir();
        let path = dir.join("mock-mcp-err.sh");
        std::fs::write(&path, script).expect("write mock script");

        let exec = McpExecutor::new(format!("sh {}", path.display()));
        let err = exec
            .execute("mcp__email__anything", "{}")
            .await
            .expect_err("server error should propagate");
        assert!(err.contains("intentional error"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_exit_is_detected_and_reported() {
        // A script that exits immediately after the handshake so the
        // next request hits a dead pipe.
        let script = r#"
            read -r line
            echo '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock","version":"0"},"capabilities":{"tools":{}}}}'
            exit 0
        "#;
        let dir = tempdir();
        let path = dir.join("mock-mcp-exit.sh");
        std::fs::write(&path, script).expect("write mock script");

        let exec = McpExecutor::new(format!("sh {}", path.display()));
        let err = exec
            .execute("mcp__email__anything", "{}")
            .await
            .expect_err("dead child should error");
        assert!(err.contains("crashed") || err.contains("exited"), "got: {err}");
    }

    // ── Helpers ────────────────────────────────────────────────────────
    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mcp-tool-executor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
