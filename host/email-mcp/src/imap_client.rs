//! IMAP client wrapper around `async-imap`.
//!
//! We expose a thin surface (`list_inbox`, `get_email`,
//! `search_emails`, `mark_read`) and keep the raw `async-imap`
//! types out of the tool layer. Errors are mapped to `anyhow` so
//! the JSON-RPC layer can render them as a single message.
//!
//! ## async-imap 0.11 API notes
//!
//! Unlike older versions, `async-imap` 0.11 dropped the
//! `FetchAttributes` builder and now accepts a free-form query
//! string (the same shape used on the IMAP wire, e.g.
//! `"UID FLAGS ENVELOPE"` or `"BODY[]"`). We mirror that
//! decision and pass query strings directly.

use std::collections::HashMap;

use async_imap::imap_proto::types::Envelope;
use async_imap::types::{Fetch, Flag};
use futures_util::StreamExt;
use mailparse::parse_mail;
use serde_json::json;
use std::borrow::Cow;
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::config::Config;

/// A summary view of a single inbox message, suitable for
/// returning to the LLM as part of `list_inbox` output.
#[derive(Debug, Clone)]
pub struct MessageSummary {
    /// IMAP UID (stable across the session).
    pub uid: u32,
    /// Sender `From:` header (raw).
    pub from: Option<String>,
    /// Subject.
    pub subject: String,
    /// Stringified `Date:` header.
    pub date: Option<String>,
    /// Whether the `\Seen` flag is set.
    pub seen: bool,
}

impl MessageSummary {
    /// Render this summary as a JSON object.
    fn to_json(&self) -> serde_json::Value {
        json!({
            "uid": self.uid,
            "from": self.from,
            "subject": self.subject,
            "date": self.date,
            "seen": self.seen,
        })
    }
}

/// Full message body, returned by `get_email`.
pub struct FullMessage {
    /// Parsed headers.
    pub headers: HashMap<String, String>,
    /// Best-effort plain-text body (first `text/plain` part).
    pub body: String,
}

impl FullMessage {
    /// Render as a single JSON object. The body is truncated to
    /// [`MAX_BODY_CHARS`] so we don't blow past LLM context windows.
    fn to_json(&self) -> serde_json::Value {
        let truncated = if self.body.len() > MAX_BODY_CHARS {
            let mut s = self.body[..MAX_BODY_CHARS].to_string();
            s.push_str("\n... [truncated]");
            s
        } else {
            self.body.clone()
        };
        json!({
            "headers": self.headers,
            "body": truncated,
        })
    }
}

/// Cap for body content returned to the LLM.
const MAX_BODY_CHARS: usize = 8 * 1024;

/// Lazily-opened IMAP session. We hold the post-login
/// `Session` so the destructor sends LOGOUT when the session
/// is dropped (which is also when stdin closes).
///
/// The stream type is `tokio_native_tls::TlsStream<tokio::net::TcpStream>`
/// wrapped with `tokio_util::compat` to convert tokio's
/// `AsyncRead`/`AsyncWrite` into the `futures` traits that
/// `async-imap` requires.
pub struct ImapSession {
    client: async_imap::Session<
        tokio_util::compat::Compat<tokio_native_tls::TlsStream<tokio::net::TcpStream>>,
    >,
}

impl ImapSession {
    /// Connect & log in. Caller is responsible for keeping the
    /// returned session alive (drop = logout).
    ///
    /// `async-imap` 0.10 / 0.11 dropped the top-level `connect()`
    /// helper. We build the TLS stream ourselves with
    /// `async-native-tls` and hand the resulting stream to
    /// `Client::new`.
    pub async fn connect(config: &Config) -> Result<Self, anyhow::Error> {
        let tcp = TcpStream::connect((config.imap_host.as_str(), config.imap_port)).await?;
        let cx = tokio_native_tls::TlsConnector::from(
            native_tls::TlsConnector::new().map_err(|e| anyhow::anyhow!("tls connector: {e}"))?,
        );
        let tls_stream = cx.connect(&config.imap_host, tcp).await?;
        // async-imap is built on `futures::AsyncRead`; tokio's
        // I/O uses its own traits. `compat()` bridges them.
        let compat = tls_stream.compat();
        let client = async_imap::Client::new(compat);
        let mut session = client
            .login(&config.user, &config.password)
            .await
            .map_err(|(e, _)| e)?;
        session.select("INBOX").await?;
        Ok(Self { client: session })
    }

    /// List the most recent `limit` messages in `INBOX`, newest first.
    pub async fn list_inbox(&mut self, limit: u32) -> Result<Vec<MessageSummary>, anyhow::Error> {
        let seqs: std::collections::HashSet<u32> = self.client.search("ALL").await?;
        let total = seqs.len();
        if total == 0 {
            return Ok(Vec::new());
        }
        // Build a comma-separated "1,5,8,11" range — simpler than
        // sorting u32s ourselves.
        let mut wanted: Vec<u32> = seqs.iter().copied().collect();
        wanted.sort_unstable();
        let start = total.saturating_sub(limit as usize);
        let slice = &wanted[start..];
        let sequence_set: String = slice
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");

        let query = "UID FLAGS ENVELOPE";
        let mut stream = self.client.fetch::<_, _>(sequence_set, query).await?;
        let mut out = Vec::new();
        while let Some(msg) = stream.next().await {
            let msg = msg?;
            let summary = summarize_fetch(&msg)?;
            out.push(summary);
        }
        // `FETCH` returns ascending order; reverse so newest first.
        out.reverse();
        Ok(out)
    }

    /// Fetch a single message by UID and return the parsed body.
    pub async fn get_email(&mut self, uid: u32) -> Result<FullMessage, anyhow::Error> {
        let mut stream = self.client.uid_fetch(uid.to_string(), "BODY[]").await?;
        let msg = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("uid {uid} not found"))??;
        let body_bytes = msg.body().ok_or_else(|| anyhow::anyhow!("missing body"))?;
        let parsed = parse_mail(body_bytes)?;

        let mut headers = HashMap::new();
        for header in &parsed.headers {
            headers.insert(header.get_key().to_string(), header.get_value());
        }
        let body = extract_text_body(&parsed).unwrap_or_default();
        Ok(FullMessage { headers, body })
    }

    /// Search INBOX for messages whose subject OR from-header
    /// contains `query` (case-insensitive). Returns up to `limit`
    /// summaries, newest first.
    pub async fn search_emails(
        &mut self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<MessageSummary>, anyhow::Error> {
        let q = escape_imap_string(query);
        let criteria = format!("OR SUBJECT \"{q}\" FROM \"{q}\"");
        let seqs: std::collections::HashSet<u32> = self.client.search(&criteria).await?;
        let total = seqs.len();
        if total == 0 {
            return Ok(Vec::new());
        }
        let mut wanted: Vec<u32> = seqs.iter().copied().collect();
        wanted.sort_unstable();
        let start = total.saturating_sub(limit as usize);
        let slice = &wanted[start..];
        let sequence_set: String = slice
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");

        let mut stream = self
            .client
            .fetch::<_, _>(sequence_set, "UID FLAGS ENVELOPE")
            .await?;
        let mut out = Vec::new();
        while let Some(msg) = stream.next().await {
            let msg = msg?;
            out.push(summarize_fetch(&msg)?);
        }
        out.reverse();
        Ok(out)
    }

    /// Mark a message as seen (set `\Seen` flag) by UID.
    pub async fn mark_read(&mut self, uid: u32) -> Result<(), anyhow::Error> {
        let mut stream = self
            .client
            .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
            .await?;
        // Drain the response stream so the server actually
        // processes the STORE. Without this, the warning
        // "unused implementer of Stream" fires and the
        // server-side state never gets updated.
        while let Some(_resp) = stream.next().await {}
        Ok(())
    }

    /// Render a list of [`MessageSummary`] as a JSON array.
    pub fn summaries_to_json(summaries: &[MessageSummary]) -> serde_json::Value {
        serde_json::Value::Array(summaries.iter().map(MessageSummary::to_json).collect())
    }

    /// Render a [`FullMessage`] as JSON.
    pub fn full_to_json(msg: &FullMessage) -> serde_json::Value {
        msg.to_json()
    }
}

/// Build a [`MessageSummary`] from a single `FETCH` response.
fn summarize_fetch(msg: &Fetch) -> Result<MessageSummary, anyhow::Error> {
    let envelope: Option<&Envelope<'_>> = msg.envelope();
    let (from, subject, date) = match envelope {
        Some(env) => (
            from_address(env),
            cow_to_string(env.subject.as_ref()).unwrap_or_default(),
            cow_to_string(env.date.as_ref()),
        ),
        None => (None, String::new(), None),
    };

    let seen = msg.flags().any(|f| matches!(f, Flag::Seen));

    Ok(MessageSummary {
        uid: msg.uid.unwrap_or(0),
        from,
        subject,
        date,
        seen,
    })
}

/// Decode a `Cow<[u8]>` to `String`, returning `None` if it isn't
/// valid UTF-8.
fn cow_to_string(cow: Option<&Cow<'_, [u8]>>) -> Option<String> {
    cow.and_then(|c| std::str::from_utf8(c).ok().map(ToString::to_string))
}

/// Extract a human-readable `Name <addr@host>` string from the
/// envelope's first `from` address.
fn from_address(env: &Envelope<'_>) -> Option<String> {
    let addr = env.from.as_ref()?.first()?;
    let name = addr
        .name
        .as_ref()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();
    let mbox = String::from_utf8_lossy(addr.mailbox.as_ref()?).into_owned();
    let host = String::from_utf8_lossy(addr.host.as_ref()?).into_owned();
    let email = format!("{mbox}@{host}");
    if name.is_empty() {
        Some(email)
    } else {
        Some(format!("{name} <{email}>"))
    }
}

/// Escape an arbitrary string for inclusion in an IMAP SEARCH
/// quoted-string. We strip characters that would otherwise let
/// the user inject extra criteria.
fn escape_imap_string(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n'))
        .collect()
}

/// Walk a parsed RFC 822 message and return the first
/// `text/plain` part, decoded. Returns `None` if no plain-text
/// part exists.
fn extract_text_body(parsed: &mailparse::ParsedMail<'_>) -> Option<String> {
    fn walk(m: &mailparse::ParsedMail<'_>) -> Option<String> {
        let ct = m.ctype.mimetype.to_lowercase();
        if ct == "text/plain" {
            let body = m.get_body_raw().ok()?;
            return Some(String::from_utf8_lossy(&body).to_string());
        }
        if ct.starts_with("multipart/") {
            for sub in &m.subparts {
                if let Some(text) = walk(sub) {
                    return Some(text);
                }
            }
        }
        None
    }
    walk(parsed)
}
