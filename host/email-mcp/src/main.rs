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

use tokio::io::{self, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::imap_client::{error_is_connection_loss, ImapSession};
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

    /// Run `f` against the IMAP session, reconnecting at most once and only
    /// when the **connection** is what failed.
    ///
    /// A mail server closes an idle IMAP connection (Gmail after ~29
    /// minutes). This process is long-lived and the session used to be
    /// cached forever: the first call after the server hung up failed, and
    /// every later call reused the same dead session — email stayed broken
    /// until the MCP subprocess was restarted.
    ///
    /// The first fix retried on *any* error, which was its own defect: a
    /// `get_email` for an absent UID, or a `BAD` from a malformed search,
    /// tore down a healthy session and re-ran TCP + TLS + LOGIN + SELECT
    /// before returning the same error. `async-imap::Session` has no `Drop`,
    /// so each of those rebuilds also leaked a socket until the server timed
    /// it out.
    ///
    /// Reconnection is now driven by two things only: an error the client
    /// marked as [`crate::imap_client::ConnectionLost`], or a `NOOP` probe
    /// that fails before the call. Everything else is returned to the caller
    /// with the session intact.
    async fn with_imap<F, T>(&self, mut f: F) -> Result<T, anyhow::Error>
    where
        F: for<'a> FnMut(
            &'a mut ImapSession,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, anyhow::Error>> + 'a>,
        >,
    {
        self.ensure_imap().await?;

        // Probe the cached session before using it. A server that hung up on
        // an idle connection is otherwise only discovered by the operation
        // itself, and its error is indistinguishable from a request error
        // without this.
        if self.session_is_dead().await {
            self.drop_session().await;
            self.ensure_imap().await?;
        }

        let mut guard = self.imap.lock().await;
        let Some(session) = guard.as_mut() else {
            return Err(anyhow::anyhow!("imap not connected"));
        };
        match f(session).await {
            Ok(v) => Ok(v),
            Err(e) => {
                drop(guard);
                // Retry once, and only for a transport failure.
                if !error_is_connection_loss(&e) {
                    return Err(e);
                }
                log::warn!("imap connection lost ({e}); reconnecting once");
                self.drop_session().await;
                self.ensure_imap().await?;
                let mut guard = self.imap.lock().await;
                let Some(session) = guard.as_mut() else {
                    return Err(anyhow::anyhow!("imap not connected"));
                };
                f(session).await
            }
        }
    }

    /// Whether the cached IMAP session is known to be dead.
    ///
    /// A `NOOP` is the cheapest way to ask the server whether the connection
    /// still exists. A transport error means it does not; a server `NO`/`BAD`
    /// answer means it does (and the session is left for the caller).
    async fn session_is_dead(&self) -> bool {
        let mut guard = self.imap.lock().await;
        let Some(session) = guard.as_mut() else {
            return true;
        };
        match session.noop().await {
            Ok(()) => false,
            // Only a *transport* failure means the session is gone. A server
            // that answers NO/BAD to NOOP in some state is still connected;
            // treating that as death rebuilt the whole session on every tool
            // call, forever — the reconnect loop the `ConnectionLost` marker
            // exists to prevent.
            Err(e) if error_is_connection_loss(&e) => {
                log::warn!("imap NOOP lost the connection ({e}); reconnecting");
                true
            }
            Err(e) => {
                log::warn!("imap NOOP was refused ({e}); keeping the session");
                false
            }
        }
    }

    /// Discard the cached session, sending LOGOUT first.
    ///
    /// `async-imap::Session` has **no `Drop` implementation**, so simply
    /// dropping it closes the socket without a `LOGOUT`; the server keeps the
    /// session (and the connection) until its own timeout. Sending it is best
    /// effort — if the connection is already gone there is nothing to say.
    async fn drop_session(&self) {
        let mut guard = self.imap.lock().await;
        if let Some(mut session) = guard.take() {
            if let Err(e) = session.logout().await {
                // The connection being gone is exactly why we are dropping it.
                log::debug!("imap LOGOUT failed ({e}); dropping anyway");
            }
        }
    }

    /// [`Self::with_imap`] for SMTP — with one crucial difference: **no
    /// retry**.
    ///
    /// An IMAP read can safely be repeated; a send cannot. SMTP is not
    /// idempotent: if the connection drops after `DATA` was accepted, the
    /// server may already have queued the message while `send` still
    /// returns an error. Retrying would deliver it a second time — a real
    /// duplicate mail in the user's recipients' inboxes, caused by the
    /// recovery path itself.
    ///
    /// So a failure clears the cached transport (the next call reconnects)
    /// and reports the original error once. The user decides whether to
    /// resend, because only they can tell whether the mail went out.
    async fn with_smtp<F, T>(&self, mut f: F) -> Result<T, anyhow::Error>
    where
        F: for<'a> FnMut(
            &'a mut SmtpSession,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, anyhow::Error>> + 'a>,
        >,
    {
        self.ensure_smtp().await?;
        let mut guard = self.smtp.lock().await;
        let Some(session) = guard.as_mut() else {
            return Err(anyhow::anyhow!("smtp not connected"));
        };
        match f(session).await {
            Ok(v) => Ok(v),
            Err(e) => {
                drop(guard);
                // Drop the transport so the *next* call starts from a fresh
                // connection. This is the liveness half of the reconnect
                // logic, without the duplicate-send half.
                let mut guard = self.smtp.lock().await;
                *guard = None;
                log::warn!("smtp operation failed: {e}; transport dropped for reconnect");
                Err(e)
            }
        }
    }
}

/// Largest JSON-RPC request line we will accept from the MCP client.
///
/// The stdio transport is newline-delimited, and a whole line is buffered
/// before it can be validated — so an unbounded read lets the peer choose
/// our peak memory. MCP requests are small (tool arguments are a few KB at
/// most); 1 MiB is a generous ceiling.
///
/// The limit is enforced *while* reading (see [`read_line_bounded`]), not
/// by measuring the finished string: by then the allocation has already
/// happened, which is the memory a hostile peer set out to consume.
const MAX_REQUEST_LINE_BYTES: usize = 1_048_576;

/// How many further bytes of an over-long line we are willing to consume
/// to resynchronise on the next newline. Past this the peer is not
/// speaking the line protocol at all and the server exits with an error
/// rather than reading forever.
const MAX_OVERLONG_LINE_DRAIN_BYTES: usize = 16 * 1_048_576;

/// Result of one bounded line read.
enum LineRead {
    /// End of input.
    Eof,
    /// One line, including its trailing `\n` (callers trim).
    Line(String),
    /// The line exceeded the limit. Its remaining bytes were consumed up
    /// to the next newline so the stream stays line-aligned.
    TooLong { bytes: usize },
}

/// Read one newline-terminated line, buffering at most `max` bytes of it.
///
/// `AsyncBufReadExt::read_until` / `lines()` cannot express a ceiling: they
/// keep growing the buffer until the delimiter arrives, so a caller only
/// discovers an over-long line after paying for it. This reads through
/// `fill_buf`, copies at most `max` bytes, and then drains to the newline
/// without storing anything.
async fn read_line_bounded<R>(reader: &mut R, max: usize) -> std::io::Result<LineRead>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    read_line_bounded_with_drain_budget(reader, max, MAX_OVERLONG_LINE_DRAIN_BYTES).await
}

/// [`read_line_bounded`] with an explicit resynchronisation budget.
///
/// Split out so the "give up on a stream that never sends a newline" path is
/// testable without pushing 16 MiB through a test.
async fn read_line_bounded_with_drain_budget<R>(
    reader: &mut R,
    max: usize,
    drain_budget: usize,
) -> std::io::Result<LineRead>
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
                    // Drop what we had and stop storing; the rest of the
                    // line is consumed but never held.
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
        if overflow && total > max.saturating_add(drain_budget) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "request line exceeds {max} bytes and no newline was found in the \
                     first {} discarded bytes",
                    drain_budget
                ),
            ));
        }
    }

    if overflow {
        return Ok(LineRead::TooLong { bytes: total });
    }
    String::from_utf8(buf).map(LineRead::Line).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request line is not valid UTF-8",
        )
    })
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

    let mut stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    // Announce capabilities. MCP `initialize` → `tools/list` is a
    // handshake the client drives; we only need to answer requests
    // here. The handshake is handled by `jsonrpc::dispatch`.
    log::info!("waiting for JSON-RPC requests on stdin");

    loop {
        // The ceiling is applied while reading, so an over-long line never
        // becomes an allocation this process has to pay for.
        let line = match read_line_bounded(&mut stdin, MAX_REQUEST_LINE_BYTES).await {
            // A read that produced bytes this process cannot interpret — a
            // non-UTF-8 line, or an over-long one whose discard budget ran
            // out — is a *bad request*, not a reason to end the session.
            // `?` here killed the server on one stray byte with no reply at
            // all; the stream is already line-aligned (`read_line_bounded`
            // consumed through the newline, or gave up at a bounded point),
            // so answering -32700 and carrying on is both correct and safe.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                log::error!("magent-email-mcp: unreadable request line: {e}");
                let err_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("request line is not valid UTF-8 or is unreadable: {e}"),
                    }
                });
                let payload = serde_json::to_string(&err_response)?;
                stdout.write_all(payload.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
            // Any other transport failure (EOF races on a closed pipe) is
            // genuinely fatal.
            Err(e) => return Err(e.into()),
            Ok(LineRead::Eof) => break,
            Ok(LineRead::Line(line)) => line,
            Ok(LineRead::TooLong { bytes }) => {
                log::error!(
                    "magent-email-mcp: request line is {bytes} bytes (limit {}); rejecting",
                    MAX_REQUEST_LINE_BYTES
                );
                let err_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!(
                            "request exceeds the {} byte limit",
                            MAX_REQUEST_LINE_BYTES
                        ),
                    }
                });
                let payload = serde_json::to_string(&err_response)?;
                stdout.write_all(payload.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `read_line_bounded` over a byte slice.
    async fn read_all(input: &[u8], max: usize) -> Vec<LineRead> {
        let mut reader = BufReader::new(input);
        let mut out = Vec::new();
        loop {
            match read_line_bounded(&mut reader, max)
                .await
                .expect("no io error")
            {
                LineRead::Eof => break,
                other => out.push(other),
            }
        }
        out
    }

    /// The ceiling must be applied while reading, and the stream must stay
    /// line-aligned afterwards: the *next* request has to be served
    /// normally even though the previous one was rejected.
    #[tokio::test]
    async fn an_overlong_line_is_rejected_without_losing_the_next_one() {
        let mut input = vec![b'x'; 40];
        input.push(b'\n');
        // 9 bytes: inside the 10-byte ceiling, so it must be delivered.
        input.extend_from_slice(b"{\"ok\":1}\n");

        let reads = read_all(&input, 10).await;
        assert_eq!(reads.len(), 2);
        assert!(
            matches!(reads[0], LineRead::TooLong { bytes } if bytes == 41),
            "the over-long line must be reported with its true length"
        );
        assert!(
            matches!(&reads[1], LineRead::Line(s) if s.trim() == "{\"ok\":1}"),
            "the next line must survive the rejected one"
        );
    }

    /// A multi-byte character split across a `fill_buf` boundary must not be
    /// corrupted.
    ///
    /// The reader accumulates raw bytes and decodes once at the end, so a
    /// `BufReader` refill that lands in the middle of a CJK code point is
    /// harmless. An implementation that decoded per-chunk would turn
    /// `{"city":"北京"}` into U+FFFD. The line is sized to straddle the
    /// reader's 8 KiB buffer.
    #[tokio::test]
    async fn a_multibyte_character_split_across_a_buffer_refill_survives() {
        // 8191 ASCII bytes, then a 3-byte character that starts at offset
        // 8191 and so spans the 8192-byte refill boundary.
        let mut input = vec![b'a'; 8191];
        input.extend_from_slice("北".as_bytes());
        input.extend_from_slice(b"\n");

        let mut reader = BufReader::new(&input[..]);
        match read_line_bounded(&mut reader, 1 << 20).await.expect("read") {
            LineRead::Line(line) => {
                let trimmed = line.trim_end();
                assert!(
                    trimmed.ends_with('北'),
                    "the split character must survive intact, got tail {:?}",
                    &trimmed[trimmed.len().saturating_sub(8)..]
                );
                assert_eq!(trimmed.len(), 8194);
            }
            _ => panic!("a line within the limit must be returned"),
        }
    }

    /// A line exactly at the limit is accepted; one byte over is not. The
    /// newline counts toward the read, matching the old `len()` check.
    #[tokio::test]
    async fn the_limit_is_inclusive() {
        let reads = read_all(b"123456789\n", 10).await;
        assert!(matches!(&reads[0], LineRead::Line(s) if s.trim() == "123456789"));

        let reads = read_all(b"1234567890\n", 10).await;
        assert!(matches!(reads[0], LineRead::TooLong { .. }));
    }

    /// An over-long line with no newline must not be drained forever: past
    /// the discard budget the read fails so the loop can exit rather than
    /// consuming an endless flood.
    #[tokio::test]
    async fn a_never_terminated_overlong_stream_errors_out() {
        let mut reader = BufReader::new(&b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"[..]);
        let err = read_line_bounded_with_drain_budget(&mut reader, 2, 8).await;
        assert!(
            err.is_err(),
            "an unterminated flood has to end in an error, not in unbounded reading"
        );

        // Within the budget a terminated line is still a clean rejection.
        let mut reader = BufReader::new(&b"xxxxxxxxxx\n"[..]);
        assert!(matches!(
            read_line_bounded_with_drain_budget(&mut reader, 2, 8).await,
            Ok(LineRead::TooLong { bytes }) if bytes == 11
        ));
    }

    /// EOF after a partial (non-terminated) line yields that line, not a
    /// hang or a loss.
    #[tokio::test]
    async fn eof_after_a_partial_line_returns_it() {
        let reads = read_all(b"partial", 100).await;
        assert!(matches!(&reads[0], LineRead::Line(s) if s == "partial"));
    }

    /// A non-UTF-8 line is an error rather than silently mangled bytes.
    #[tokio::test]
    async fn invalid_utf8_is_rejected() {
        let mut reader = BufReader::new(&b"\xff\xfe\n"[..]);
        assert!(read_line_bounded(&mut reader, 100).await.is_err());
    }

    /// The helper itself must not allocate the whole line: reading a
    /// 60 KiB flood with a 1 KiB cap keeps only the cap.
    #[tokio::test]
    async fn a_flood_is_not_buffered_in_full() {
        let mut input = vec![b'y'; 60 * 1024];
        input.push(b'\n');
        let mut reader = BufReader::new(&input[..]);
        match read_line_bounded(&mut reader, 1024).await.expect("read") {
            LineRead::TooLong { bytes } => assert_eq!(bytes, 60 * 1024 + 1),
            _ => panic!("a 60 KiB line must be over a 1 KiB limit"),
        }
        // A plain `lines()` would have held all 60 KiB here; the bounded
        // read drops it as soon as the cap is crossed.
        let _ = reader.into_inner();
    }
}
