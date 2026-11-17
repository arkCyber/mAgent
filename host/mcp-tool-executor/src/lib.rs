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
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Maximum length of a tool result string, **in characters**, before we
/// truncate it. (The name and docs always said characters; the comparison
/// used `str::len`, so a CJK response was cut at 4096 bytes — roughly 1365
/// characters.)
const MAX_RESULT_CHARS: usize = 4096;

/// Byte index just past the first `max_chars` characters of `s`.
///
/// Returns `s.len()` when the string is shorter. Slicing at the result is
/// always a valid UTF-8 boundary.
fn byte_index_after_chars(s: &str, max_chars: usize) -> usize {
    s.char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Per-request timeout for the JSON-RPC round-trip.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Largest JSON-RPC response line accepted from the MCP subprocess.
///
/// Large enough for a sizeable `list_inbox` (a few hundred KB of JSON) and
/// small enough that a runaway child cannot make the host buffer gigabytes
/// in one line. The result is truncated to `MAX_RESULT_CHARS` afterwards;
/// this cap protects the buffering step that happens first — so it is
/// enforced *while* reading ([`read_line_bounded`]), not by measuring the
/// finished string after the allocation has already been paid for.
const MAX_RESPONSE_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Bytes of an over-long line we will discard while resynchronising on the
/// next newline before giving up on the child.
const MAX_OVERLONG_LINE_DRAIN_BYTES: usize = 16 * 1024 * 1024;

/// One bounded line read from the child's stdout.
enum LineRead {
    Eof,
    Line(String),
    TooLong,
}

/// Read one newline-terminated line, buffering at most `max` bytes.
///
/// `lines()` / `read_until` have no ceiling: they grow the buffer until the
/// delimiter arrives, so an over-long line is only detectable after it has
/// already been held in memory. This copies at most `max` bytes and drains
/// the remainder of an over-long line without storing it.
async fn read_line_bounded<R>(reader: &mut R, max: usize) -> std::io::Result<LineRead>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;

    let mut buf: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    let mut overflow = false;

    loop {
        let (consumed, has_newline) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                if total == 0 && !overflow {
                    return Ok(LineRead::Eof);
                }
                break;
            }
            let consumed = match available.iter().position(|&b| b == b'\n') {
                Some(pos) => pos + 1,
                None => available.len(),
            };
            let has_newline = available.get(consumed - 1) == Some(&b'\n');
            if !overflow {
                if buf.len() + consumed > max {
                    overflow = true;
                    buf = Vec::new();
                } else {
                    buf.extend_from_slice(&available[..consumed]);
                }
            }
            total = total.saturating_add(consumed);
            (consumed, has_newline)
        };
        reader.consume(consumed);
        if has_newline {
            break;
        }
        if overflow && total > max.saturating_add(MAX_OVERLONG_LINE_DRAIN_BYTES) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP response line has no newline within the discard budget",
            ));
        }
    }

    if overflow {
        return Ok(LineRead::TooLong);
    }
    String::from_utf8(buf).map(LineRead::Line).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "response line is not UTF-8",
        )
    })
}

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

    /// The response stream could not be kept line-aligned — an over-long
    /// line, a non-UTF-8 line, or a discard budget exhausted mid-line.
    ///
    /// Distinct from [`Self::ParseError`] because only this variant means
    /// the *pipe* is unusable: the remainder of the offending line is still
    /// buffered, so every later call would read fragments of it. A
    /// `ParseError` from a well-formed but unexpected reply says nothing
    /// about the stream, and killing the child for it would discard a
    /// healthy IMAP session.
    #[error("MCP response stream is desynchronised: {0}")]
    StreamDesync(String),

    /// The server's response didn't match the expected schema.
    #[error("JSON parse error: {0}")]
    ParseError(String),

    /// The tool itself reported a failure via MCP's `isError: true`.
    ///
    /// MCP carries *tool* failures inside a **successful** JSON-RPC
    /// response: `{"result": {"content": [...], "isError": true}}`. Ignoring
    /// that flag — which this executor did — turned a rejected
    /// `send_email` into `Ok("SMTP server rejected message …")`, so the ReAct
    /// loop recorded a successful call and the model told the user the mail
    /// had been sent.
    #[error("MCP tool failed: {0}")]
    ToolError(String),

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
                description: "List recent messages in the INBOX (up to `limit`, default 20)."
                    .into(),
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
                description:
                    "Fetch a single email by IMAP UID, returning headers and plain-text body."
                        .into(),
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
                description:
                    "Search INBOX for messages whose subject OR from-header contains `query`."
                        .into(),
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
    pub async fn execute(&self, tool: &str, args: &str) -> std::result::Result<String, String> {
        // ── 1. Fast-path: not an email tool ──────────────────────────────
        let Some(stripped) = tool.strip_prefix("mcp__email__") else {
            return Err(format!("unknown tool: {tool}"));
        };

        // ── 1b. Validate the arguments before touching the subprocess. ────
        //
        // Malformed argument JSON is the *caller's* error, not "no
        // arguments". `serde_json::from_str(args).unwrap_or(Value::Null)`
        // sent the tool a null payload, so the server ran with its defaults
        // and reported success — a truncated `{"limit":` became `limit=20`
        // and the model was told the limit it asked for took effect.
        // `ble_send` had the same defect (fixed separately); the direct path
        // kept it. Checking here also avoids spawning a subprocess to answer
        // a request that cannot succeed.
        let args_json: Value = match serde_json::from_str(args) {
            Ok(value) => value,
            Err(e) => {
                return Err(format!(
                    "invalid arguments for {tool}: {e} (arguments must be a JSON object)"
                ))
            }
        };
        if !args_json.is_object() && !args_json.is_null() {
            return Err(format!(
                "invalid arguments for {tool}: expected a JSON object, got {}",
                json_type_name(&args_json)
            ));
        }

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
            child.call(stripped, args_json).await
        };
        let content = result.map_err(|e| match e {
            McpError::ChildExited(_) => {
                // Process died mid-call: clear the slot so the next call
                // re-spawns.
                *guard = None;
                format!("MCP subprocess crashed: {e}")
            }
            // A timeout means the request was written but no matching reply
            // arrived. A serial MCP server (email-mcp) may be stuck in a
            // blocking IMAP call and will never read stdin again, so keeping
            // the child would make every later call time out too. Drop it;
            // `kill_on_drop` reaps it and the next call spawns a fresh one.
            McpError::Timeout(_) => {
                *guard = None;
                format!("MCP subprocess timed out and was restarted: {e}")
            }
            // A stream that is no longer line-aligned cannot be recovered by
            // reading further: the oversized line's remainder, or a partial
            // UTF-8 sequence, is still in the pipe, so every later call would
            // parse fragments of it — and could re-trip the same budget —
            // until the timeout. Restart the child, as the timeout path does.
            McpError::StreamDesync(_) | McpError::Io(_) => {
                *guard = None;
                format!("MCP subprocess stream was desynchronised and was restarted: {e}")
            }
            // A well-formed reply the client did not expect says nothing
            // about the stream, so the child (and its session) is kept.
            McpError::ParseError(_) => e.to_string(),
            other => other.to_string(),
        })?;

        // ── 4. Truncate if needed. ─────────────────────────────────────
        if content.chars().count() > MAX_RESULT_CHARS {
            let cut = byte_index_after_chars(&content, MAX_RESULT_CHARS);
            let mut truncated = content[..cut].to_string();
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
    /// Buffered stdout handle.
    ///
    /// This used to be the raw `ChildStdout`, with a fresh
    /// `BufReader::with_capacity(65536, &mut self.stdout)` created inside every
    /// `rpc()` call. When the matching reply line was found the function
    /// returned and that `BufReader` was dropped — **discarding any bytes it
    /// had already read from the pipe but not consumed**. A child that writes
    /// its reply and then a notification (MCP allows `notifications/message`)
    /// in one burst, or that splits two JSON lines across one 64 KiB read,
    /// left the second line half-consumed: the next `rpc()` began mid-JSON,
    /// skipped the fragment as "non-JSON noise", and eventually timed out and
    /// killed a healthy process. Holding the reader for the life of the child
    /// keeps the buffer across calls, which is what it is for.
    stdout: BufReader<tokio::process::ChildStdout>,
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
        let program = tokens.next().ok_or_else(|| McpError::SpawnFailed {
            binary: binary.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "binary path is empty"),
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
            // One buffer for the child's whole life; see the field doc.
            stdout: BufReader::with_capacity(65536, stdout),
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
        // `rpc()` matches on `id` only — it does not distinguish a `result`
        // from an `error`. A server that refused the handshake (incompatible
        // protocol version, bad clientInfo) answers with an error envelope,
        // and treating that as success told the supervisor a usable child was
        // up; every later tool call then failed with an unrelated symptom.
        // The rejection is the handshake failing, so report it as such.
        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("(no message)");
            return Err(McpError::HandshakeFailed(format!(
                "server rejected initialize: {message} (code {code})"
            )));
        }
        // A response that carries neither field is not a successful
        // handshake either; an empty `{}` would otherwise pass silently.
        if resp.get("result").is_none() {
            return Err(McpError::HandshakeFailed(format!(
                "initialize response carried no result: {resp}"
            )));
        }
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

        // MCP reports a *tool* failure with `isError: true` inside an
        // otherwise successful response. Read the text first (it is the
        // diagnostic) and then surface it as a failure.
        let content = resp
            .pointer("/result/content")
            .and_then(|arr| arr.as_array())
            .and_then(|arr| arr.first())
            .and_then(|obj| obj.get("text"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::ParseError("response missing result.content[0].text".to_string())
            })?
            .to_string();

        if resp
            .pointer("/result/isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(McpError::ToolError(content));
        }

        Ok(content)
    }

    /// Allocate the next monotonically-increasing request ID.
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send one JSON-RPC request and read one JSON-RPC response.
    async fn rpc(&mut self, req: Value) -> std::result::Result<Value, McpError> {
        use tokio::io::AsyncWriteExt;

        let line = serde_json::to_string(&req).map_err(|e| McpError::ParseError(e.to_string()))?;

        // ── WRITE ─────────────────────────────────────────────────────
        // Borrow the writer through the mutex instead of `take()`-ing it.
        // The old take/put-back sequence left `stdin` permanently `None` if
        // the future was cancelled between the two, and the next call then
        // panicked on `expect("stdin already taken")`. `rpc` is invoked
        // serially on `&mut self`, so holding the guard for the write is the
        // same serialization the child already has.
        let write_result: std::io::Result<()> = {
            let mut stdin_guard = self.stdin.lock().await;
            let Some(stdin) = stdin_guard.as_mut() else {
                return Err(McpError::ParseError(
                    "MCP subprocess stdin is unavailable".to_string(),
                ));
            };
            async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
                Ok::<(), std::io::Error>(())
            }
            .await
        };
        if let Err(e) = write_result {
            return Err(self.classify_pipe_error(e));
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

        // The line buffer is bounded *during* the read (see
        // `read_line_bounded`): `lines()` would grow it to whatever the
        // child sends before any check could reject the line.
        // The reader is persistent (see the `stdout` field doc), so bytes a
        // previous call prefetched are still here — they are not re-read from
        // the pipe, and not lost.
        let lines = &mut self.stdout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let line = match tokio::time::timeout(
                remaining,
                read_line_bounded(lines, MAX_RESPONSE_LINE_BYTES),
            )
            .await
            {
                Ok(Ok(LineRead::Line(l))) => l,
                Ok(Ok(LineRead::Eof)) => {
                    // Stream closed.
                    let status = self.child.try_wait().ok().flatten();
                    return Err(McpError::ChildExited(status));
                }
                Ok(Ok(LineRead::TooLong)) => {
                    return Err(McpError::StreamDesync(format!(
                        "MCP response line exceeds {} bytes; refusing to buffer it",
                        MAX_RESPONSE_LINE_BYTES
                    )));
                }
                Ok(Err(e)) => {
                    // An `InvalidData` from the line reader is the reader
                    // itself saying the stream cannot be resynchronised
                    // (non-UTF-8, or the discard budget ran out mid-line);
                    // any other I/O error is classified as before.
                    return Err(if e.kind() == std::io::ErrorKind::InvalidData {
                        McpError::StreamDesync(e.to_string())
                    } else {
                        self.classify_pipe_error(e)
                    });
                }
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

    /// Classify an IO error raised on one of the child's pipes.
    ///
    /// A write to a child that has already died surfaces as EPIPE
    /// ("Broken pipe"), which names the symptom rather than the cause. The
    /// obvious fix — consult `try_wait()` — is not sufficient on its own,
    /// because the process can be gone (its fds closed, hence the EPIPE) while
    /// the exit has not been reaped yet, so `try_wait()` still answers `None`.
    /// That is a genuine race, and it made this classification dependent on
    /// scheduling.
    ///
    /// So: prefer a reaped status when there is one, and otherwise treat a
    /// broken pipe as the child being unreachable — its stdin read end is
    /// closed, so it cannot serve another request regardless. Anything else is
    /// a real IO error and passes through unchanged.
    fn classify_pipe_error(&mut self, e: std::io::Error) -> McpError {
        if let Some(status) = self.child.try_wait().ok().flatten() {
            return McpError::ChildExited(Some(status));
        }
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            return McpError::ChildExited(None);
        }
        McpError::Io(e)
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

    /// Kill the underlying subprocess, if any (synchronous).
    ///
    /// Best-effort: failures are dropped, because the child is spawned with
    /// `kill_on_drop` and dies with the process regardless. Safe to call from
    /// an async context (the future is driven on the dedicated runtime, via a
    /// helper thread when the caller is already inside one), but it blocks the
    /// caller; async callers should prefer [`Self::shutdown_async`].
    pub fn shutdown(&self) {
        let _ = block_on_mcp(async {
            self.inner.shutdown().await;
            Ok::<(), String>(())
        });
    }

    /// Async shutdown for callers that already hold a runtime.
    pub async fn shutdown_async(&self) {
        self.inner.shutdown().await;
    }

    /// Synchronous one-shot: forward `tool` to the MCP subprocess.
    #[allow(dead_code)]
    fn execute_async_blocking(
        &self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        block_on_mcp(self.inner.execute(tool, args))
    }
}

impl Default for McpToolExecutor {
    fn default() -> Self {
        // Bare name, resolved via `$PATH` (works for `cargo install`).
        // Developers running from the repo root should pass an explicit
        // `target/release/magent-email-mcp` path instead.
        Self::new("magent-email-mcp")
    }
}

#[cfg(feature = "std")]
impl magent_core::agent_runner::ToolExecutor for McpToolExecutor {
    fn execute(&mut self, tool: &str, args: &str) -> std::result::Result<String, String> {
        // ── ble_send: the embedded-actor email convention ──────────────
        //
        // The embedded agent prompt uses `ble_send` with
        // `data=<email_tool_name>,characteristic=email` to request
        // email operations. When we see this pattern we strip the
        // prefix and delegate to the MCP subprocess.
        if tool == "ble_send" {
            // `ble_send` is the embedded prompt's convention for reaching
            // the email tools: `data=<tool_name>,characteristic=email`.
            //
            // Both fallbacks here used to be silent: malformed JSON became
            // an empty map, and a missing `data` became `list_inbox`. So
            // `ble_send {"characteristic":"email"}` (no `data`) actually
            // fetched the inbox and returned it as the *result of a send*,
            // and a broken payload did the same. That is an unrequested
            // side effect reported as the caller's intended action.
            let args_parsed: serde_json::Map<String, Value> = match serde_json::from_str(args) {
                Ok(map) => map,
                Err(e) => {
                    return Err(format!(
                            "ble_send: arguments are not a JSON object ({e}); \
                             expected {{\"data\": \"<email tool>\", \"characteristic\": \"email\"}}"
                        ))
                }
            };

            let email_tool = match args_parsed.get("data").and_then(|v| v.as_str()) {
                Some(t) if !t.trim().is_empty() => t,
                Some(_) => {
                    return Err(
                        "ble_send: \"data\" is empty; name the email tool to run".to_string()
                    )
                }
                None => {
                    return Err(
                        "ble_send: missing \"data\" — it must name the email tool to \
                                run (e.g. \"list_inbox\"); this call has no default action"
                            .to_string(),
                    )
                }
            };

            let namespaced = format!("mcp__email__{email_tool}");

            // Forward all args EXCEPT the two bookkeeping fields (`data`
            // and `characteristic`) that only the host-side convention uses.
            // Any real tool parameters (`limit`, `query`, `to`, etc.)
            // are preserved and forwarded to the MCP server.
            let clean_args: serde_json::Map<String, Value> = args_parsed
                .into_iter()
                .filter(|(k, _)| k != "data" && k != "characteristic")
                .collect();
            let clean_json =
                serde_json::to_string(&clean_args).unwrap_or_else(|_| "{}".to_string());

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
// The executor always runs on its own process-wide runtime. The previous
// implementation returned the *current* runtime's handle when one existed and
// then called `Handle::block_on` — which panics with "Cannot start a runtime
// from within a runtime" in exactly that situation (every async caller). When
// the caller is already inside a runtime, the future is driven on a scoped
// helper thread running the dedicated runtime; otherwise it is driven inline.

use std::sync::OnceLock;

fn dedicated_runtime_handle() -> tokio::runtime::Handle {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("magent-mcp")
            .build()
            .unwrap_or_else(|e| {
                // Building the only runtime is a startup invariant; failing
                // here would leave every MCP call without a driver. There is
                // no error channel from a `OnceLock` initializer, so report
                // through the panic hook rather than silently retrying.
                panic!("failed to build tokio runtime for mcp-tool-executor: {e}")
            })
    });
    runtime.handle().clone()
}

/// Drive an MCP future to completion from synchronous code.
///
/// Returns `Err` (rather than propagating a worker-thread panic) when the
/// helper thread unwinds.
fn block_on_mcp<F, T>(fut: F) -> std::result::Result<T, String>
where
    F: std::future::Future<Output = std::result::Result<T, String>> + Send,
    T: Send,
{
    let handle = dedicated_runtime_handle();
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| scope.spawn(move || handle.block_on(fut)).join())
            .map_err(|_| "MCP executor worker thread panicked".to_string())?
    } else {
        handle.block_on(fut)
    }
}

// ============================================================================
// Tests
// ============================================================================

/// A human-readable JSON type name for an error message.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

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
        let names: Vec<&str> = tools.iter().map(|t| t.namespaced.as_str()).collect();

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
            let names: Vec<&str> = tools.iter().map(|t| t.namespaced.as_str()).collect();

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
            let args =
                r#"{"data":"search_emails","characteristic":"email","query":"invoice","limit":10}"#;
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
            let r = exec.execute(
                "ble_send",
                r#"{"data":"does_not_exist","characteristic":"email"}"#,
            );
            assert!(r.is_err());
            let err = r.unwrap_err();
            // Must NOT say "unknown tool" — that's McpExecutor's fast-path
            // for non-email tool names. `ble_send` is always an email tool.
            assert!(!err.contains("unknown tool"), "got: {err}");
        }
    }

    #[test]
    fn shutdown_outside_a_tokio_runtime_does_not_panic() {
        // The crate's documented use case is a plain `fn main()` with no
        // `#[tokio::main]`, so `shutdown()` must tolerate the absence of a
        // current runtime handle — `Handle::current()` panics in that case.
        let exec = McpToolExecutor::new("true");
        exec.shutdown();
    }

    /// The sync `ToolExecutor::execute` used to call `Handle::block_on`
    /// while already inside a runtime, which panics ("Cannot start a runtime
    /// from within a runtime"). It must work from async callers too.
    #[cfg(feature = "std")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_execute_inside_a_runtime_does_not_panic() {
        use magent_core::agent_runner::ToolExecutor as _;
        let mut exec = McpToolExecutor::new("true");
        // `true` exits before the handshake; the call still has to travel
        // through the dedicated runtime and return an error rather than
        // unwinding.
        let err = exec
            .execute("mcp__email__list_inbox", "{}")
            .expect_err("a bare `true` cannot complete the MCP handshake");
        assert!(!err.is_empty());
        // Both forms must be callable from async code.
        exec.shutdown();
        exec.shutdown_async().await;
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

    /// Malformed argument JSON must be refused, not sent as `null`.
    ///
    /// `serde_json::from_str(args).unwrap_or(Value::Null)` turned a truncated
    /// `{"limit":` into a null argument object: the server ran the tool with
    /// its defaults and reported success, so the model believed the limit it
    /// asked for had been applied.
    #[tokio::test]
    async fn malformed_argument_json_is_rejected_before_the_server_sees_it() {
        // `true` never gets spawned: the argument check runs first.
        let exec = McpExecutor::new("true");

        for bad in [r#"{"limit":"#, "", "not json", "   "] {
            let err = exec
                .execute("mcp__email__list_inbox", bad)
                .await
                .expect_err("malformed arguments must not be forwarded");
            assert!(
                err.contains("invalid arguments"),
                "expected an argument error for {bad:?}, got {err}"
            );
        }

        // A non-object is also refused rather than coerced.
        let err = exec
            .execute("mcp__email__list_inbox", "[1,2,3]")
            .await
            .expect_err("an array is not an argument object");
        assert!(err.contains("expected a JSON object"), "got {err}");

        // `null` is a legitimate "no arguments": it must get past the
        // argument check and fail later (here, at the `true` fake server).
        let err = exec
            .execute("mcp__email__list_inbox", "null")
            .await
            .expect_err("`true` is not an MCP server");
        assert!(
            !err.contains("invalid arguments"),
            "null must reach the server, got {err}"
        );
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

    /// MCP carries a *tool* failure inside a successful JSON-RPC response,
    /// flagged `isError: true`. Ignoring it made a rejected email send look
    /// like a completed one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tool_level_is_error_is_a_failure() {
        let script = r#"
            read -r line
            echo '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock","version":"0"},"capabilities":{"tools":{}}}}'
            read -r line
            echo '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"SMTP server rejected message"}],"isError":true}}'
        "#;
        let dir = tempdir();
        let path = dir.join("mock-mcp-iserr.sh");
        std::fs::write(&path, script).expect("write mock script");

        let exec = McpExecutor::new(format!("sh {}", path.display()));
        let err = exec
            .execute("mcp__email__send_email", "{}")
            .await
            .expect_err("a tool-level failure must not read as success");
        assert!(
            err.contains("SMTP server rejected"),
            "the server's diagnostic must survive: {err}"
        );
    }

    /// `ble_send` without a `data` argument used to run `list_inbox` — a
    /// side effect nobody asked for, reported as the result of the send.
    /// The `ToolExecutor` bridge is compiled only with the `std` feature, so
    /// this test is too — the validation it covers lives in that impl.
    #[cfg(feature = "std")]
    #[test]
    fn ble_send_without_data_is_an_error_not_a_default_action() {
        use magent_core::agent_runner::ToolExecutor as _;
        // The validation runs before anything touches the subprocess, so the
        // binary path is irrelevant — `true` is never executed.
        let mut exec = McpToolExecutor::new("true");
        for bad in [
            r#"{"characteristic":"email"}"#,
            r#"{"data":"","characteristic":"email"}"#,
            r#"not json"#,
        ] {
            let err = exec
                .execute("ble_send", bad)
                .expect_err(&format!("{bad} must be rejected"));
            assert!(
                err.contains("ble_send"),
                "the error must name the tool for {bad}: {err}"
            );
        }
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
        assert!(
            err.contains("crashed") || err.contains("exited"),
            "got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn long_non_ascii_result_is_truncated_without_panicking() {
        // The budget is in *characters*: 5000 CJK characters (15 KB) must
        // keep 4096 characters, not 4096 bytes (~1365 characters).
        let payload = "天".repeat(5000);
        assert_eq!(payload.chars().count(), 5000);

        let script = format!(
            r#"
            read -r line
            echo '{{"jsonrpc":"2.0","id":0,"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":"mock","version":"0"}},"capabilities":{{"tools":{{}}}}}}}}'
            read -r line
            echo '{{"jsonrpc":"2.0","id":1,"result":{{"content":[{{"type":"text","text":"{payload}"}}],"isError":false}}}}'
        "#
        );
        let dir = tempdir();
        let path = dir.join("mock-mcp-cjk.sh");
        std::fs::write(&path, script).expect("write mock script");

        let exec = McpExecutor::new(format!("sh {}", path.display()));
        let out = exec
            .execute("mcp__email__anything", "{}")
            .await
            .expect("long non-ASCII result should not panic");

        assert!(out.ends_with("… [truncated]"), "not truncated: {out:.40}");
        // Exactly MAX_RESULT_CHARS characters kept, plus the marker.
        let marker_chars = "… [truncated]".chars().count();
        assert_eq!(out.chars().count(), MAX_RESULT_CHARS + marker_chars);

        exec.shutdown().await;
    }

    #[test]
    fn byte_index_after_chars_lands_on_a_boundary() {
        let s = "天".repeat(10); // 3 bytes per character
        assert_eq!(byte_index_after_chars(&s, 1), 3);
        assert_eq!(byte_index_after_chars(&s, 4), 12);
        // Past the end clamps to the length rather than panicking.
        assert_eq!(byte_index_after_chars(&s, 9999), s.len());
        assert_eq!(byte_index_after_chars("", 5), 0);
        assert_eq!(byte_index_after_chars("ascii", 3), 3);
    }

    // ── Bounded response-line reads ────────────────────────────────────
    //
    // The cap has to be applied *during* the read; checking a finished
    // string means the runaway allocation already happened. These exercise
    // the reader directly, with no subprocess.

    #[tokio::test]
    async fn bounded_read_returns_lines_in_order() {
        let mut r = BufReader::new(&b"one\ntwo\n"[..]);
        assert!(matches!(
            read_line_bounded(&mut r, 100).await.unwrap(),
            LineRead::Line(s) if s == "one\n"
        ));
        assert!(matches!(
            read_line_bounded(&mut r, 100).await.unwrap(),
            LineRead::Line(s) if s == "two\n"
        ));
        assert!(matches!(
            read_line_bounded(&mut r, 100).await.unwrap(),
            LineRead::Eof
        ));
    }

    #[tokio::test]
    async fn bounded_read_rejects_an_overlong_line_and_resynchronises() {
        let mut input = vec![b'z'; 5000];
        input.push(b'\n');
        input.extend_from_slice(b"next\n");
        let mut r = BufReader::new(&input[..]);

        assert!(matches!(
            read_line_bounded(&mut r, 1024).await.unwrap(),
            LineRead::TooLong
        ));
        // The following request must still be readable: a rejected line
        // cannot desynchronise the stream.
        assert!(matches!(
            read_line_bounded(&mut r, 1024).await.unwrap(),
            LineRead::Line(s) if s == "next\n"
        ));
    }

    /// A child that writes without ever sending a newline must not be read
    /// forever: past the drain budget the read errors out so the caller can
    /// report a broken server instead of buffering indefinitely.
    #[tokio::test]
    async fn bounded_read_gives_up_on_an_endless_line() {
        // A reader that always yields more bytes and never a newline.
        struct Flood;
        impl tokio::io::AsyncRead for Flood {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let n = buf.remaining().min(64 * 1024);
                let chunk = vec![b'f'; n];
                buf.put_slice(&chunk);
                std::task::Poll::Ready(Ok(()))
            }
        }
        let mut r = BufReader::new(Flood);
        assert!(
            read_line_bounded(&mut r, 1024).await.is_err(),
            "an endless line must terminate in an error, not buffer forever"
        );
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
