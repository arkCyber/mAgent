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

/// Marker for an IMAP failure caused by the *connection*, not the request.
///
/// The generic retry in `AppState::with_imap` used to reconnect on **any**
/// error, so `get_email(<absent uid>)` or a `BAD` from a malformed search
/// tore down a perfectly healthy session and re-ran TCP + TLS + LOGIN +
/// SELECT — then returned the same error. That churns connections (and, since
/// `async-imap::Session` has no `Drop`, leaks a socket per retry) without ever
/// recovering. Only errors wrapped in this type are retried.
#[derive(Debug)]
pub struct ConnectionLost(pub String);

impl std::fmt::Display for ConnectionLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConnectionLost {}

/// Whether `error` is a transport failure that a reconnect could fix.
///
/// Walks the `anyhow` chain because the marker is attached at the deepest
/// layer that knows the cause.
pub fn error_is_connection_loss(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<ConnectionLost>().is_some())
}

/// Wrap a transport failure so `with_imap` will reconnect.
fn connection_lost(context: &str, error: &async_imap::error::Error) -> anyhow::Error {
    anyhow::Error::new(ConnectionLost(format!("{context}: {error}")))
}

/// Map an `async_imap` error, marking only transport failures as retryable.
///
/// `Io` and `ConnectionLost` mean the socket is gone. `Bad` / `No` are the
/// server answering, and `Parse` / `Validate` / `Append` are request problems:
/// a retry cannot change any of them, so they are returned unmarked.
fn map_imap_error(context: &str, error: async_imap::error::Error) -> anyhow::Error {
    match error {
        async_imap::error::Error::Io(_)
        | async_imap::error::Error::ConnectionLost => connection_lost(context, &error),
        other => anyhow::anyhow!("{context}: {other}"),
    }
}

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
            let mut s = self.body[..char_boundary_at(&self.body, MAX_BODY_CHARS)].to_string();
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

/// Largest byte index `<= max` that is a UTF-8 character boundary.
///
/// `MAX_BODY_CHARS` is a byte budget, but `&body[..MAX_BODY_CHARS]` panics
/// when the cut lands inside a multi-byte code point. Any non-English email
/// (Chinese, Cyrillic, an emoji, an accented name) hits this, so the plain
/// slice turned "message too long" into "connection died".
fn char_boundary_at(s: &str, max: usize) -> usize {
    let mut i = max.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

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

    /// Ask the server whether this connection is still alive.
    ///
    /// `NOOP` is the RFC 3501 keep-alive: it produces no data but proves the
    /// session works. Used by `with_imap` to detect an idle-disconnected
    /// session *before* an operation, so a request error is not mistaken for
    /// a dead connection (and vice versa).
    pub async fn noop(&mut self) -> Result<(), anyhow::Error> {
        self.client
            .noop()
            .await
            .map_err(|e| map_imap_error("NOOP", e))
    }

    /// Send `LOGOUT` and let the server end the session cleanly.
    ///
    /// `async-imap::Session` has no `Drop`, so a session that is merely
    /// dropped leaves the server holding the connection until its timeout.
    pub async fn logout(&mut self) -> Result<(), anyhow::Error> {
        self.client
            .logout()
            .await
            .map_err(|e| map_imap_error("LOGOUT", e))
    }

    /// List the most recent `limit` messages in `INBOX`, newest first.
    pub async fn list_inbox(&mut self, limit: u32) -> Result<Vec<MessageSummary>, anyhow::Error> {
        let seqs: std::collections::HashSet<u32> = self
            .client
            .search("ALL")
            .await
            .map_err(|e| map_imap_error("SEARCH ALL", e))?;
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
        let mut stream = self
            .client
            .fetch::<_, _>(sequence_set, query)
            .await
            .map_err(|e| map_imap_error("FETCH summaries", e))?;
        let mut out = Vec::new();
        while let Some(msg) = stream.next().await {
            let msg = msg.map_err(|e| map_imap_error("FETCH summaries", e))?;
            let summary = summarize_fetch(&msg)?;
            out.push(summary);
        }
        // `FETCH` returns ascending order; reverse so newest first.
        out.reverse();
        Ok(out)
    }

    /// Fetch a single message by UID and return the parsed body.
    pub async fn get_email(&mut self, uid: u32) -> Result<FullMessage, anyhow::Error> {
        // `BODY.PEEK[]`, not `BODY[]`: async-imap documents that a plain
        // `BODY` fetch implicitly sets `\Seen` server-side. Reading a
        // message through the agent is a read-only operation — it must not
        // silently mark the user's mail as read (and disagree with the
        // explicit `mark_read` tool).
        let mut stream = self
            .client
            .uid_fetch(uid.to_string(), "BODY.PEEK[]")
            .await
            .map_err(|e| map_imap_error("UID FETCH body", e))?;
        let msg = stream
            .next()
            .await
            // An absent UID is a *request* failure: the session is fine, so
            // this error must not trigger a reconnect.
            .ok_or_else(|| anyhow::anyhow!("uid {uid} not found"))?
            .map_err(|e| map_imap_error("UID FETCH body", e))?;
        let body_bytes = msg.body().ok_or_else(|| anyhow::anyhow!("missing body"))?;
        let parsed = parse_mail(body_bytes)?;

        let mut headers = HashMap::new();
        for header in &parsed.headers {
            headers.insert(header.get_key().to_string(), header.get_value());
        }
        // A message with no `text/plain` part is reported as such rather
        // than as an empty body: a marketing mail that is HTML-only used to
        // come back as `{"body": ""}`, which reads as "this email is empty".
        //
        // A `text/plain` part that *exists* but whose transfer encoding is
        // corrupt is a third case, and must not be described as "no
        // text/plain part" — the caller would go looking for a different
        // body rather than a broken one.
        let body = match extract_text_body(&parsed) {
            TextBody::Found(text) => text,
            TextBody::Undecodable(reason) => {
                format!("[text/plain part could not be decoded: {reason}]")
            }
            TextBody::Absent => "[no text/plain part in this message]".to_string(),
        };
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
        let seqs: std::collections::HashSet<u32> = self
            .client
            .search(&criteria)
            .await
            .map_err(|e| map_imap_error("SEARCH", e))?;
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
            .await
            .map_err(|e| map_imap_error("SEARCH FETCH", e))?;
        let mut out = Vec::new();
        while let Some(msg) = stream.next().await {
            let msg = msg.map_err(|e| map_imap_error("SEARCH FETCH", e))?;
            out.push(summarize_fetch(&msg)?);
        }
        out.reverse();
        Ok(out)
    }

    /// Mark a message as seen (set `\Seen` flag) by UID.
    ///
    /// Returns `false` when no message with `uid` exists. IMAP's
    /// `UID STORE +FLAGS` on an absent UID is **not** an error — the
    /// server answers `OK` and simply never emits an untagged `FETCH`
    /// — so draining the STORE stream alone cannot tell success from a
    /// no-op. Probe with `UID FETCH <uid> (UID)` first: a present UID
    /// always produces one untagged `FETCH`, an absent one produces
    /// none.
    /// # Errors
    ///
    /// A `UID STORE` the server *rejects* (`NO` / `BAD`, e.g. because the
    /// mailbox is read-only or not selected) previously returned `Ok(true)`:
    /// `async-imap`'s store stream filters tagged completion responses out, so
    /// a rejected command yields an empty stream that a drain loop cannot
    /// distinguish from success, and the caller answered
    /// `{"status":"marked_read"}` for a flag that was never set. The flag is
    /// now read back after the store and a missing `\Seen` is an error.
    pub async fn mark_read(&mut self, uid: u32) -> Result<bool, anyhow::Error> {
        let mut probe = self
            .client
            .uid_fetch(uid.to_string(), "UID")
            .await
            .map_err(|e| map_imap_error("UID FETCH (probe)", e))?;
        let mut exists = false;
        while let Some(item) = probe.next().await {
            item.map_err(|e| map_imap_error("UID FETCH (probe)", e))?;
            exists = true;
        }
        if !exists {
            return Ok(false);
        }
        drop(probe);

        let mut stream = self
            .client
            .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
            .await
            .map_err(|e| map_imap_error("UID STORE", e))?;
        // Drain the response stream so the server actually
        // processes the STORE. Without this, the warning
        // "unused implementer of Stream" fires and the
        // server-side state never gets updated.
        while let Some(resp) = stream.next().await {
            resp.map_err(|e| map_imap_error("UID STORE", e))?;
        }
        // The stream borrows `self.client`; drop it before the read-back.
        drop(stream);

        // Verify: the store's own stream says nothing about acceptance, so
        // read the flags back.
        let mut verify = self
            .client
            .uid_fetch(uid.to_string(), "UID FLAGS")
            .await
            .map_err(|e| map_imap_error("UID FETCH (verify)", e))?;
        let mut seen = false;
        let mut found = false;
        while let Some(item) = verify.next().await {
            let msg = item.map_err(|e| map_imap_error("UID FETCH (verify)", e))?;
            found = true;
            if msg.flags().any(|flag| matches!(flag, Flag::Seen)) {
                seen = true;
            }
        }
        if !found {
            anyhow::bail!("uid {uid} disappeared while it was being marked read");
        }
        if !seen {
            anyhow::bail!(
                "the server did not apply \\Seen to uid {uid} \
                 (is the mailbox read-only?)"
            );
        }
        Ok(true)
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
pub(crate) fn escape_imap_string(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n'))
        .collect()
}

/// Walk a parsed RFC 822 message and return the first
/// `text/plain` part, decoded. Returns `None` if no plain-text
/// part exists.
/// What a message's text body yielded.
///
/// Three outcomes, not two: a `text/plain` part can be missing, present and
/// decoded, or present and *undecodable* (a corrupt base64 or
/// quoted-printable transport encoding). Collapsing the last two into "no
/// body" told the model the mail was HTML-only when in fact its text was
/// damaged — a diagnosis that sends the caller looking for the wrong thing.
#[derive(Debug)]
enum TextBody {
    /// A `text/plain` part was found and decoded.
    Found(String),
    /// A `text/plain` part exists, but its body could not be decoded.
    Undecodable(String),
    /// The message has no `text/plain` part at all (e.g. HTML-only mail).
    Absent,
}

fn extract_text_body(parsed: &mailparse::ParsedMail<'_>) -> TextBody {
    fn walk(m: &mailparse::ParsedMail<'_>) -> Option<TextBody> {
        let ct = m.ctype.mimetype.to_lowercase();
        if ct == "text/plain" {
            return Some(decode_part_body(m));
        }
        if ct.starts_with("multipart/") {
            // Prefer a decodable part over an undecodable one: a damaged
            // `text/plain` alternative should not hide a sibling that reads
            // fine. First decodable part wins, preserving the original
            // "first text/plain" preference; an undecodable one is reported
            // only if nothing better exists.
            let mut undecodable: Option<TextBody> = None;
            for sub in &m.subparts {
                match walk(sub) {
                    Some(TextBody::Found(text)) => return Some(TextBody::Found(text)),
                    // Keep the first undecodable part; a later one must not
                    // replace the reason the first failed.
                    Some(other) if undecodable.is_none() => undecodable = Some(other),
                    _ => {}
                }
            }
            return undecodable;
        }
        None
    }
    walk(parsed).unwrap_or(TextBody::Absent)
}

/// Decode one `text/plain` part, honouring a *declared* charset when it is
/// trustworthy and falling back to UTF-8 otherwise.
///
/// Three separate traps live behind "just call `get_body()`", each of which
/// has already shipped as a regression here:
///
/// 1. **No `charset=` at all.** `mailparse` defaults `ctype.charset` to the
///    literal `"us-ascii"`, which the `charset` crate decodes as Latin-1, so
///    a plain `Content-Type: text/plain` carrying UTF-8 came back as
///    `æ¸©åº¦`.
/// 2. **A charset label the `charset` crate does not know** — `utf8mb4`,
///    `unknown-8bit`, `x-unknown` are all common from real MUAs. `get_body()`
///    falls through to an ASCII decode that replaces every non-ASCII byte with
///    U+FFFD, i.e. **the body text is destroyed**, not merely mis-rendered.
/// 3. **A mislabelled body** in a *known* wrong charset (`us-ascii` carrying
///    UTF-8, which is what MUA bugs and naive gateways produce).
///
/// So: decode by the declared label when there is one, but treat a result
/// containing U+FFFD as untrustworthy and fall back to
/// [`decode_undeclared`]. A lossy decode is never better than the original
/// bytes, which are still available here.
fn decode_part_body(m: &mailparse::ParsedMail<'_>) -> TextBody {
    let declared = m
        .ctype
        .params
        .get("charset")
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty());
    // `get_body_raw` undoes Content-Transfer-Encoding only (base64 / QP),
    // which is what we want before deciding on a charset ourselves. It is
    // always read: even when branch 3 below delegates to `get_body()`, having
    // the undecoded bytes is what lets every other branch — and the fallback
    // when `get_body()` is lossy — stay in control.
    let raw = match m.get_body_raw() {
        Ok(raw) => raw,
        Err(e) => return TextBody::Undecodable(e.to_string()),
    };

    let Some(label) = declared else {
        return TextBody::Found(decode_undeclared(&raw));
    };

    // 1. A UTF-8 label: the bytes should be UTF-8. If they are not, the label
    //    is wrong, so fall back rather than emit replacement characters.
    if is_utf8_label(&label) {
        return TextBody::Found(decode_undeclared(&raw));
    }

    // 2. An ASCII/Latin-1 label. These are *single-byte* sets, so a
    //    `us-ascii` part carrying UTF-8 decodes "successfully" as mojibake —
    //    the original defect one label variety over. Prefer UTF-8 when the
    //    bytes are valid UTF-8 (the overwhelmingly likely truth), and only
    //    use the single-byte decode when they are not.
    if is_single_byte_label(&label) {
        return TextBody::Found(decode_undeclared(&raw));
    }

    // 3. Anything else (GBK, Shift_JIS, an unknown label). `get_body()` uses
    //    mailparse's charset table, which is the only multi-byte support
    //    available here — but an *unknown* label falls through to a lossy
    //    ASCII decode that destroys every non-ASCII byte. Treat replacement
    //    characters as proof the decode failed, and fall back to the bytes.
    match m.get_body() {
        Ok(body) if !body.contains('\u{FFFD}') => TextBody::Found(body),
        _ => TextBody::Found(decode_undeclared(&raw)),
    }
}

/// Whether `label` names the UTF-8 family.
fn is_utf8_label(label: &str) -> bool {
    matches!(label, "utf-8" | "utf8" | "utf8mb3") || label.starts_with("utf-8")
}

/// Whether `label` names a single-byte set whose every byte maps to a char.
fn is_single_byte_label(label: &str) -> bool {
    // Every ISO-8859 part, every Windows code page in common use, and ASCII.
    // The list is enumerated rather than pattern-matched on "windows-" (which
    // has a multi-byte member, windows-1258 is single-byte but 1252..1258 are
    // all single-byte while 932/936/949/950 are not).
    label.starts_with("us-ascii")
        || label.starts_with("ascii")
        || label.starts_with("iso-8859")
        || label.starts_with("iso8859")
        || label.starts_with("latin")
        || label.starts_with("windows-125")
        || label.starts_with("cp125")
        || label.starts_with("cp819")
        || label.starts_with("koi8")
        || label.starts_with("ibm819")
}

/// Decode bytes whose charset is absent or untrustworthy.
///
/// UTF-8 when valid (the overwhelmingly common case, and the only encoding
/// that can be checked without guessing); otherwise Latin-1, which never
/// fails and at least preserves every byte as a character.
fn decode_undeclared(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only transport failures may trigger a reconnect.
    ///
    /// `with_imap` retried on any error, so a `BAD` from a malformed search —
    /// or an absent UID — rebuilt the whole session and returned the same
    /// error, churning connections (and leaking a socket each time, since
    /// `async-imap::Session` has no `Drop`).
    #[test]
    fn only_transport_failures_are_retryable() {
        use async_imap::error::Error as ImapError;

        // Server answers: not retryable.
        for e in [
            ImapError::Bad("syntax error".into()),
            ImapError::No("no such mailbox".into()),
            ImapError::Parse(async_imap::error::ParseError::Invalid(vec![b'x'])),
        ] {
            let mapped = map_imap_error("test", e);
            assert!(
                !error_is_connection_loss(&mapped),
                "a server response must not reconnect: {mapped}"
            );
        }

        // A request-level failure (absent UID) is not retryable either.
        let absent = anyhow::anyhow!("uid 7 not found");
        assert!(!error_is_connection_loss(&absent));

        // Transport failures are.
        let io = ImapError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "closed",
        ));
        assert!(error_is_connection_loss(&map_imap_error("test", io)));
        assert!(error_is_connection_loss(&map_imap_error(
            "test",
            ImapError::ConnectionLost
        )));

        // The marker survives being wrapped in a new context layer.
        let wrapped = map_imap_error("test", ImapError::ConnectionLost)
            .context("while listing");
        assert!(error_is_connection_loss(&wrapped), "context must preserve it");
    }

    /// A non-UTF-8 charset must be decoded, not mangled. `get_body_raw()`
    /// ignores `charset=` entirely, so `café` in iso-8859-1 (0xE9) came
    /// back as U+FFFD.
    #[test]
    fn extract_text_body_honours_the_declared_charset() {
        // A genuine iso-8859-1 body: the `é` is the single byte 0xE9, not
        // the two UTF-8 bytes 0xC3 0xA9. `get_body_raw()` would hand these
        // bytes to `from_utf8_lossy` and produce U+FFFD.
        let mut raw = b"Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\ncaf"
            .to_vec();
        raw.push(0xE9);
        let parsed = parse_mail(&raw).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => {
                assert_eq!(body, "café", "the declared charset must be applied")
            }
            _ => panic!("expected a decoded text body"),
        }
    }

    /// A `text/plain` with **no** `charset=` carrying UTF-8 must decode.
    ///
    /// `mailparse` defaults `ctype.charset` to the literal `"us-ascii"`, so
    /// `get_body()` decodes UTF-8 bytes as Latin-1 and produced mojibake for
    /// every plain-text message that did not declare a charset — which is
    /// most of them. The previous `from_utf8_lossy(get_body_raw())` handled
    /// this correctly, so applying `get_body()` unconditionally was a
    /// regression.
    #[test]
    fn extract_text_body_decodes_utf8_when_no_charset_is_declared() {
        // `温度` as UTF-8, no `charset=` parameter at all.
        let raw = b"Content-Type: text/plain\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n\xe6\xb8\xa9\xe5\xba\xa6";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => {
                assert_eq!(body, "温度", "undeclared UTF-8 must not be Latin-1 decoded")
            }
            other => panic!("expected a decoded body, got {other:?}"),
        }
    }

    /// The same for a multipart alternative whose `text/plain` part omits
    /// `charset=`.
    #[test]
    fn extract_text_body_decodes_utf8_in_a_multipart_alternative() {
        let raw = b"Content-Type: multipart/alternative; boundary=X\r\n\r\n\
--X\r\n\
Content-Type: text/plain\r\n\r\n\
\xe6\xb8\xa9\xe5\xba\xa6\r\n\
--X\r\n\
Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>hot</p>\r\n\
--X--\r\n";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert!(body.contains("温度"), "got {body:?}"),
            other => panic!("expected a decoded body, got {other:?}"),
        }
    }

    /// Bytes that are neither valid UTF-8 nor declared must still come back
    /// as text (Latin-1), never as an error or an empty string.
    #[test]
    fn undeclared_non_utf8_bytes_fall_back_to_latin1() {
        let mut raw = b"Content-Type: text/plain\r\n\r\ncaf".to_vec();
        raw.push(0xE9); // Latin-1 `é`, invalid as UTF-8 on its own.
        let parsed = parse_mail(&raw).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert_eq!(body, "café"),
            other => panic!("expected a decoded body, got {other:?}"),
        }
    }

    /// A damaged `text/plain` part must not hide a sibling that decodes.
    #[test]
    fn a_corrupt_part_does_not_shadow_a_healthy_sibling() {
        let raw = b"Content-Type: multipart/alternative; boundary=Y\r\n\r\n\
--Y\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
!!!!not-base64!!!!\r\n\
--Y\r\n\
Content-Type: text/plain\r\n\r\n\
REAL BODY\r\n\
--Y--\r\n";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert!(body.contains("REAL BODY"), "got {body:?}"),
            other => panic!("a healthy sibling must win, got {other:?}"),
        }
    }

    /// A charset label the decoder does not know must not destroy the body.
    ///
    /// `utf8mb4` and `unknown-8bit` are common from real MUAs. Routing them
    /// through `mailparse::get_body()` falls back to an ASCII decode that
    /// replaces every non-ASCII byte with U+FFFD — the message text is gone,
    /// which is worse than the mojibake it replaced.
    #[test]
    fn an_unknown_charset_label_falls_back_to_utf8() {
        for label in ["utf8mb4", "unknown-8bit", "x-unknown"] {
            let mut raw = format!(
                "Content-Type: text/plain; charset={label}\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n"
            )
            .into_bytes();
            raw.extend_from_slice("温度".as_bytes());
            let parsed = parse_mail(&raw).expect("parse");
            match extract_text_body(&parsed) {
                TextBody::Found(body) => {
                    assert_eq!(body, "温度", "charset={label} must not be lossy");
                    assert!(
                        !body.contains('\u{FFFD}'),
                        "charset={label} produced replacement characters: {body:?}"
                    );
                }
                other => panic!("charset={label}: expected a body, got {other:?}"),
            }
        }
    }

    /// A body *mislabelled* `us-ascii` but carrying UTF-8 must still decode.
    ///
    /// MUA bugs and naive gateways produce this, and a strict Latin-1 read
    /// would turn `温度` back into mojibake — the original defect, one label
    /// variety over.
    #[test]
    fn utf8_mislabelled_as_ascii_still_decodes() {
        let raw = b"Content-Type: text/plain; charset=us-ascii\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n\xe6\xb8\xa9\xe5\xba\xa6";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert_eq!(body, "温度"),
            other => panic!("expected a body, got {other:?}"),
        }
    }

    /// A *known* single-byte charset carrying UTF-8 bytes must not be
    /// mis-decoded.
    ///
    /// `windows-1251`, `koi8-r`, `iso-8859-2` … all decode any byte string
    /// without error, so a body that is really UTF-8 came back as mojibake
    /// (`Привет` → `РџСЂРёРІРµС‚`). The old `from_utf8_lossy(get_body_raw())`
    /// handled these; the round-21 dispatch lost them until the single-byte
    /// list covered the whole family.
    #[test]
    fn a_single_byte_charset_carrying_utf8_still_decodes() {
        for label in [
            "windows-1251",
            "koi8-r",
            "iso-8859-2",
            "iso-8859-5",
            "iso-8859-7",
            "windows-1256",
            "cp1251",
        ] {
            let mut raw = format!(
                "Content-Type: text/plain; charset={label}\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n"
            )
            .into_bytes();
            raw.extend_from_slice("Привет".as_bytes());
            let parsed = parse_mail(&raw).expect("parse");
            match extract_text_body(&parsed) {
                TextBody::Found(body) => assert_eq!(body, "Привет", "charset={label}"),
                other => panic!("charset={label}: expected a body, got {other:?}"),
            }
        }
    }

    /// The same for a correctly-labelled single-byte body: a Latin-1 `é` is
    /// not valid UTF-8, so the single-byte decode must still be used.
    #[test]
    fn a_single_byte_charset_with_real_single_byte_bytes_decodes() {
        let mut raw = b"Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\ncaf"
            .to_vec();
        raw.push(0xE9);
        let parsed = parse_mail(&raw).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert_eq!(body, "café"),
            other => panic!("expected a body, got {other:?}"),
        }
    }

    /// A multi-byte charset (GBK) still uses mailparse's table.
    #[test]
    fn a_multibyte_charset_uses_the_charset_table() {
        // GBK for 中文测试.
        let mut raw = b"Content-Type: text/plain; charset=gbk\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n"
            .to_vec();
        raw.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4]);
        let parsed = parse_mail(&raw).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert_eq!(body, "中文测试"),
            other => panic!("expected a body, got {other:?}"),
        }
    }

    /// A genuine non-UTF-8 label still decodes with that charset.
    #[test]
    fn a_real_latin1_body_still_decodes_as_latin1() {
        let mut raw = b"Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\ncaf"
            .to_vec();
        raw.push(0xE9);
        let parsed = parse_mail(&raw).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => assert_eq!(body, "café"),
            other => panic!("expected a body, got {other:?}"),
        }
    }

    /// A quoted-printable part must be decoded too (transport encoding is
    /// independent of charset).
    #[test]
    fn extract_text_body_decodes_quoted_printable() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\r\n\
The answer is 42.=\r\n";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Found(body) => {
                assert!(body.contains("The answer is 42."), "got {body:?}")
            }
            _ => panic!("expected a decoded text body"),
        }
    }

    /// HTML-only mail has no `text/plain` part; `get_email` must say so
    /// rather than returning an empty body that reads as "nothing here".
    #[test]
    fn an_html_only_message_reports_the_missing_text_part() {
        let raw = b"Content-Type: text/html; charset=utf-8\r\n\r\n<p>Hi</p>";
        let parsed = parse_mail(raw).expect("parse");
        assert!(matches!(extract_text_body(&parsed), TextBody::Absent));
    }

    /// A `text/plain` part whose transfer encoding is corrupt is *not* the
    /// same as a missing part. Reporting it as absent sent the caller
    /// looking for an HTML body instead of at damaged data.
    #[test]
    fn a_corrupt_transfer_encoding_is_reported_as_undecodable_not_absent() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
!!!!not-base64!!!!\r\n";
        let parsed = parse_mail(&raw[..]).expect("parse");
        match extract_text_body(&parsed) {
            TextBody::Undecodable(reason) => {
                assert!(!reason.is_empty(), "the reason must say something")
            }
            TextBody::Found(_) => panic!("invalid base64 must not decode"),
            TextBody::Absent => {
                panic!("a present-but-broken part must not be reported as absent")
            }
        }
    }

    #[test]
    fn char_boundary_at_clamps_and_stays_on_a_boundary() {
        let s = "天天地"; // 3 bytes per char
        assert_eq!(char_boundary_at(s, 4), 3); // byte 4 is inside `地`
        assert_eq!(char_boundary_at(s, 1), 0);
        assert_eq!(char_boundary_at(s, 99), s.len());
        assert!(s.is_char_boundary(char_boundary_at(s, 4)));
    }

    /// A body of non-ASCII text longer than the 8 KiB cap used to panic in
    /// `to_json` via `&self.body[..MAX_BODY_CHARS]`.
    #[test]
    fn long_non_ascii_body_serialises_without_panicking() {
        let body = "温".repeat(MAX_BODY_CHARS); // 3 bytes each → 3x over budget
        let msg = FullMessage {
            headers: HashMap::new(),
            body,
        };
        let v = msg.to_json();
        let out = v["body"].as_str().unwrap();
        assert!(out.ends_with("... [truncated]"));
        assert!(out.len() < MAX_BODY_CHARS + 32);
    }
}
