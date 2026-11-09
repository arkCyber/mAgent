//! Tool registry + concrete tool implementations.
//!
//! Each tool is a `(name, description, input_schema, handler)`
//! tuple. The registry walks the catalogue for `tools/list` and
//! dispatches `tools/call` by name. Handlers are kept small —
//! they translate JSON arguments into IMAP/SMTP operations and
//! format results as JSON strings.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::imap_client::ImapSession;
use crate::AppState;

/// MCP tool descriptor (returned in `tools/list`).
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    /// Tool name as the LLM will call it.
    pub name: &'static str,
    /// One-line description for the LLM.
    pub description: &'static str,
    /// JSON Schema for the tool's input arguments.
    pub input_schema: Value,
}

impl ToolDescriptor {
    /// Serialise for inclusion in the MCP `tools/list` response.
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        })
    }
}

/// Fallback `limit` when the caller omits it. Matches the `default` in the
/// advertised input schemas.
const DEFAULT_LIMIT: u32 = 20;
/// Advertised `limit` bounds (`minimum` / `maximum` in the input schemas).
const MIN_LIMIT: u32 = 1;
const MAX_LIMIT: u32 = 200;

/// Read and range-check the `limit` argument.
///
/// MCP clients do not validate `inputSchema` for us, and the value ends up in
/// the *text* of an IMAP command: `limit == 0` produced an empty sequence set
/// (`FETCH  UID …`, a protocol error surfaced as an opaque server reply), and a
/// value ≥ 2³² wrapped through `as u32` — asking for 4294967296 messages
/// returned **zero** rather than an error. We reject out-of-range values rather
/// than clamp so the caller sees the mistake.
fn limit_arg(args: &Value) -> Result<u32, anyhow::Error> {
    let Some(raw) = args.get("limit") else {
        return Ok(DEFAULT_LIMIT);
    };
    if raw.is_null() {
        return Ok(DEFAULT_LIMIT);
    }
    let n = raw
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("`limit` must be a non-negative integer, got {raw}"))?;
    if !(MIN_LIMIT as u64..=MAX_LIMIT as u64).contains(&n) {
        anyhow::bail!("`limit` must be between {MIN_LIMIT} and {MAX_LIMIT}, got {n}");
    }
    Ok(n as u32)
}

/// Read and range-check a `uid` argument.
///
/// Same rationale as [`limit_arg`]: `uid` is interpolated into the IMAP
/// command, and `as u32` silently truncated a UID ≥ 2³² (e.g. 4294967296 → 0,
/// which is not a valid IMAP UID at all).
fn uid_arg(args: &Value) -> Result<u32, anyhow::Error> {
    let raw = args
        .get("uid")
        .ok_or_else(|| anyhow::anyhow!("missing `uid`"))?;
    let n = raw
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("`uid` must be a non-negative integer, got {raw}"))?;
    u32::try_from(n)
        .ok()
        .filter(|u| *u >= 1)
        .ok_or_else(|| anyhow::anyhow!("`uid` must be an integer in 1..=4294967295, got {n}"))
}

/// Tool registry — owns the catalogue and a handle to the shared
/// [`AppState`] (config + lazy IMAP/SMTP sessions).
pub struct ToolRegistry {
    state: Arc<AppState>,
    catalogue: Vec<ToolDescriptor>,
}

impl ToolRegistry {
    /// Build the static tool catalogue. Sessions open lazily on
    /// first call, so constructing the registry never touches
    /// the network.
    pub fn new(state: Arc<AppState>) -> Self {
        let catalogue = vec![
            ToolDescriptor {
                name: "list_inbox",
                description: "List recent messages in the INBOX. Returns up to `limit` summaries (default 20).",
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
            ToolDescriptor {
                name: "get_email",
                description: "Fetch a single email by IMAP UID, returning parsed headers and plain-text body.",
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
            ToolDescriptor {
                name: "search_emails",
                description: "Search INBOX for messages whose subject OR from-header contains `query`.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Non-empty substring to match (case-insensitive). Use `list_inbox` to list without a filter.",
                            "minLength": 1,
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
            ToolDescriptor {
                name: "send_email",
                description: "Send a plain-text email. `to` accepts a single address or a comma-separated list.",
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
            ToolDescriptor {
                name: "mark_read",
                description: "Mark a message as seen by IMAP UID.",
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
        ];
        Self { state, catalogue }
    }

    /// Snapshot of the catalogue for `tools/list`.
    pub fn tool_descriptors(&self) -> Vec<Value> {
        self.catalogue.iter().map(ToolDescriptor::to_json).collect()
    }

    /// Whether `name` is one of the tools this server advertises.
    ///
    /// The JSON-RPC layer uses this to keep "no such tool" a protocol
    /// error while reporting a *handler* failure as an MCP tool error
    /// (`isError: true`), per the MCP specification.
    pub fn contains(&self, name: &str) -> bool {
        self.catalogue.iter().any(|d| d.name == name)
    }

    /// Dispatch a tool call by name. Returns the tool's output as
    /// a JSON string ready to embed in the JSON-RPC `content`
    /// array.
    pub async fn call(&self, name: &str, args: Value) -> Result<String, anyhow::Error> {
        match name {
            "list_inbox" => self.tool_list_inbox(args).await,
            "get_email" => self.tool_get_email(args).await,
            "search_emails" => self.tool_search_emails(args).await,
            "send_email" => self.tool_send_email(args).await,
            "mark_read" => self.tool_mark_read(args).await,
            other => Err(anyhow::anyhow!("unknown tool: {other}")),
        }
    }

    // -----------------------------------------------------------------
    // Tool implementations
    // -----------------------------------------------------------------

    async fn tool_list_inbox(&self, args: Value) -> Result<String, anyhow::Error> {
        let limit = limit_arg(&args)?;
        self.state.ensure_imap().await?;
        let mut guard = self.state.imap.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("imap not connected"))?;
        let summaries = session.list_inbox(limit).await?;
        let json = ImapSession::summaries_to_json(&summaries);
        Ok(serde_json::to_string_pretty(&json)?)
    }

    async fn tool_get_email(&self, args: Value) -> Result<String, anyhow::Error> {
        let uid = uid_arg(&args)?;
        self.state.ensure_imap().await?;
        let mut guard = self.state.imap.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("imap not connected"))?;
        let msg = session.get_email(uid).await?;
        let json = ImapSession::full_to_json(&msg);
        Ok(serde_json::to_string_pretty(&json)?)
    }

    async fn tool_search_emails(&self, args: Value) -> Result<String, anyhow::Error> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing `query`"))?;
        // IMAP's `SUBJECT ""` / `FROM ""` match every message, so a
        // query that reduces to nothing after escaping (empty,
        // whitespace-only, or only quote/backslash/CR/LF characters)
        // turns this into "list the whole INBOX" — a surprising result
        // for a search, and one the caller can get explicitly via
        // `list_inbox`. Reject it rather than issue a match-all
        // search. Check the escaped form because that is what reaches
        // the server.
        if crate::imap_client::escape_imap_string(query)
            .trim()
            .is_empty()
        {
            anyhow::bail!("`query` must not be empty");
        }
        let limit = limit_arg(&args)?;
        self.state.ensure_imap().await?;
        let mut guard = self.state.imap.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("imap not connected"))?;
        let summaries = session.search_emails(query, limit).await?;
        let json = ImapSession::summaries_to_json(&summaries);
        Ok(serde_json::to_string_pretty(&json)?)
    }

    async fn tool_send_email(&self, args: Value) -> Result<String, anyhow::Error> {
        let to = args
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing `to`"))?;
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing `subject`"))?;
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing `body`"))?;
        self.state.ensure_smtp().await?;
        let guard = self.state.smtp.lock().await;
        let session = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("smtp not connected"))?;
        let response = session.send(to, subject, body).await?;
        Ok(json!({ "status": "sent", "server_response": response }).to_string())
    }

    async fn tool_mark_read(&self, args: Value) -> Result<String, anyhow::Error> {
        let uid = uid_arg(&args)?;
        self.state.ensure_imap().await?;
        let mut guard = self.state.imap.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("imap not connected"))?;
        if !session.mark_read(uid).await? {
            anyhow::bail!("no message with uid {uid} in INBOX");
        }
        Ok(json!({ "status": "marked_read", "uid": uid }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_when_absent_or_null() {
        assert_eq!(limit_arg(&json!({})).unwrap(), DEFAULT_LIMIT);
        assert_eq!(limit_arg(&json!({ "limit": null })).unwrap(), DEFAULT_LIMIT);
    }

    /// `limit: 0` used to reach the IMAP layer as an empty sequence set
    /// (`FETCH  UID FLAGS ENVELOPE`), which the server rejects with an opaque
    /// BAD rather than the caller's mistake.
    #[test]
    fn limit_zero_is_rejected() {
        let err = limit_arg(&json!({ "limit": 0 })).unwrap_err();
        assert!(err.to_string().contains("between 1 and 200"), "got: {err}");
    }

    /// `limit` used to be narrowed with `as u32`: 2³² wrapped to 0, so asking
    /// for 4294967296 messages returned none.
    #[test]
    fn limit_above_u32_max_is_rejected_not_wrapped() {
        let err = limit_arg(&json!({ "limit": 4_294_967_296u64 })).unwrap_err();
        assert!(err.to_string().contains("between 1 and 200"), "got: {err}");
    }

    #[test]
    fn limit_accepts_the_advertised_range() {
        assert_eq!(limit_arg(&json!({ "limit": 1 })).unwrap(), 1);
        assert_eq!(limit_arg(&json!({ "limit": 200 })).unwrap(), 200);
    }

    #[test]
    fn limit_non_integer_is_rejected() {
        assert!(limit_arg(&json!({ "limit": "5" })).is_err());
        assert!(limit_arg(&json!({ "limit": -1 })).is_err());
    }

    #[test]
    fn uid_rejects_zero_out_of_range_and_missing() {
        assert!(uid_arg(&json!({})).is_err());
        assert!(uid_arg(&json!({ "uid": 0 })).is_err());
        assert!(uid_arg(&json!({ "uid": 4_294_967_296u64 })).is_err());
        assert!(uid_arg(&json!({ "uid": "7" })).is_err());
        assert_eq!(uid_arg(&json!({ "uid": 1 })).unwrap(), 1);
        assert_eq!(
            uid_arg(&json!({ "uid": 4_294_967_295u64 })).unwrap(),
            u32::MAX
        );
    }
}
