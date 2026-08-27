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
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as u32;
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
        let uid = args
            .get("uid")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("missing `uid`"))? as u32;
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
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as u32;
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
        let uid = args
            .get("uid")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("missing `uid`"))? as u32;
        self.state.ensure_imap().await?;
        let mut guard = self.state.imap.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("imap not connected"))?;
        session.mark_read(uid).await?;
        Ok(json!({ "status": "marked_read", "uid": uid }).to_string())
    }
}
