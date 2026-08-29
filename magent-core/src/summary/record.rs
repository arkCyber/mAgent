//! Pure-data layer for the summary store.
//!
//! This module is deliberately **IO-free** so it can compile under
//! `no_std + alloc` for embedded targets (nRF52840, ESP32-C3/C6).
//! The persistence trait and the host / KV implementations live in
//! sibling modules; the data types below are shared.
//!
//! See the parent `summary::mod` for the JSON schema, design
//! rationale, and the storage trait.

#![cfg(feature = "std")]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::agent_runner::{Message, Role};
use crate::conversation::CompressionStats;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Current schema version of [`SummaryRecord`]. Bump whenever the
/// JSON shape changes in a breaking way; readers MUST refuse to load
/// files whose `schema_version` is higher than this constant.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Maximum number of historical snapshots retained in
/// [`SummaryRecord::history`]. Once exceeded the oldest entry is
/// dropped on the next `save()`.
pub const HISTORY_MAX: usize = 5;

/// Maximum length (bytes) for a topic name. Mirrors
/// `PROMPT_AUTHOR_MAX` in the prompt store so the two stay in
/// lockstep.
pub const SUMMARY_TOPIC_MAX: usize = 128;

/// Maximum length (bytes) for `description`. Mirrors
/// `PROMPT_DESCRIPTION_MAX` so both metadata blocks have the same
/// ceiling.
pub const SUMMARY_DESCRIPTION_MAX: usize = 1_024;

/// Maximum length (bytes) for `author`. Mirrors `PROMPT_AUTHOR_MAX`.
pub const SUMMARY_AUTHOR_MAX: usize = 256;

/// Maximum number of tags. Mirrors `PROMPT_TAGS_MAX`.
pub const SUMMARY_TAGS_MAX: usize = 32;

/// Maximum length of a single tag (bytes). Mirrors
/// `PROMPT_TAG_MAX`.
pub const SUMMARY_TAG_MAX: usize = 64;

/// Maximum length (bytes) of `llm_summary`. Cap is intentionally
/// large because summaries are the whole point of this store, but
/// we still want a guardrail so a runaway LLM can't bloat the JSON
/// to gigabytes.
pub const SUMMARY_LLM_MAX: usize = 32 * 1024;

/// Hard cap on the **serialised** size of a single summary record,
/// enforced by the host store. The 64 KiB ceiling is roughly
/// 2× `SUMMARY_LLM_MAX` + a generous window, and fits comfortably
/// under the nRF52840 page size. The KV store has its own (smaller)
/// cap; see `KvSummaryStore::MAX_RECORD_BYTES` when that lands.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the summary storage layer. Each variant carries
/// enough context for a one-line diagnostic on the CLI. The shape is
/// deliberately similar to [`crate::prompt::PromptError`] so the two
/// stores feel like siblings.
#[derive(Debug)]
pub enum SummaryError {
    /// A user-supplied topic was empty, too long, or contained path
    /// separators.
    InvalidTopic(String),
    /// A summary's metadata field failed validation (length cap,
    /// control character, etc.). The string is human-readable.
    InvalidMetadata(String),
    /// `llm_summary` exceeded [`SUMMARY_LLM_MAX`].
    SummaryTooLarge {
        /// Actual size of the offending string in bytes.
        size: usize,
        /// Configured cap.
        max: usize,
    },
    /// Serialised record exceeded the per-backend size cap. The
    /// host store uses [`MAX_RECORD_BYTES`]; a KV backend will have
    /// its own ceiling. Either way the caller is being told
    /// "shrink the payload, not the storage".
    RecordTooLarge {
        /// Actual byte length of the serialised record.
        size: usize,
        /// Backend-specific cap.
        max: usize,
    },
    /// The summaries directory couldn't be created or inspected.
    DirIo {
        /// Directory we were trying to use.
        path: String,
        /// Underlying IO error string. We use `String` (not
        /// `io::Error`) because this enum must compile under
        /// `no_std` for the embedded port; the formatting stays the
        /// same.
        source: String,
    },
    /// A file existed but couldn't be parsed as a [`SummaryRecord`].
    Parse {
        /// Path of the offending file.
        path: String,
        /// Underlying serde error string.
        source: String,
    },
    /// A file parsed but its `schema_version` is newer than what we
    /// support.
    UnsupportedSchema {
        /// Path of the offending file.
        path: String,
        /// `schema_version` found in the file.
        found: u32,
        /// Highest `schema_version` this binary understands.
        supported: u32,
    },
    /// The named summary doesn't exist on disk.
    NotFound(String),
    /// Writing the JSON file failed.
    Write {
        /// Path we were trying to write.
        path: String,
        /// Underlying IO error string.
        source: String,
    },
    /// Reading an existing file failed (e.g. permission denied).
    Read {
        /// Path we were trying to read.
        path: String,
        /// Underlying IO error string.
        source: String,
    },
    /// Cross-version equality comparison was attempted. The caller
    /// tried to compare two records with different `schema_version`
    /// values, which is meaningless; use `compatible_with` instead.
    SchemaMismatch {
        /// `schema_version` on the left-hand side.
        lhs: u32,
        /// `schema_version` on the right-hand side.
        rhs: u32,
    },
    /// A summary with this topic already exists and the caller did not
    /// ask to overwrite it. Surfaced by the `run --save-summary` path so
    /// it refuses to silently clobber a previous run's summary (matching
    /// the `summary save` subcommand's overwrite protection).
    AlreadyExists(String),
}

impl fmt::Display for SummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SummaryError::InvalidTopic(t) => write!(
                f,
                "invalid summary topic {:?}: must be non-empty, ≤ {} bytes, \
                 and contain no path separators",
                t, SUMMARY_TOPIC_MAX
            ),
            SummaryError::InvalidMetadata(msg) => {
                write!(f, "invalid summary metadata: {}", msg)
            }
            SummaryError::SummaryTooLarge { size, max } => {
                write!(f, "llm_summary is {} bytes; max is {}", size, max)
            }
            SummaryError::RecordTooLarge { size, max } => {
                write!(f, "serialised summary is {} bytes; max is {}", size, max)
            }
            SummaryError::DirIo { path, source } => {
                write!(f, "summaries directory {}: {}", path, source)
            }
            SummaryError::Parse { path, source } => {
                write!(f, "could not parse {} as a summary: {}", path, source)
            }
            SummaryError::UnsupportedSchema {
                path,
                found,
                supported,
            } => write!(
                f,
                "{} has schema_version {} but this magent binary only understands up to {}; \
                 upgrade the binary first",
                path, found, supported
            ),
            SummaryError::NotFound(name) => {
                write!(f, "no summary named {:?} on disk", name)
            }
            SummaryError::Write { path, source } => {
                write!(f, "could not write {}: {}", path, source)
            }
            SummaryError::Read { path, source } => {
                write!(f, "could not read {}: {}", path, source)
            }
            SummaryError::SchemaMismatch { lhs, rhs } => write!(
                f,
                "cannot compare summary records with mismatched schema_version ({} vs {})",
                lhs, rhs
            ),
            SummaryError::AlreadyExists(name) => write!(
                f,
                "summary {:?} already exists; pass --save-summary-overwrite to replace",
                name
            ),
        }
    }
}

impl std::error::Error for SummaryError {}

// ---------------------------------------------------------------------------
// Topic validation
// ---------------------------------------------------------------------------

/// Validate a user-supplied topic name. Mirrors
/// `crate::prompt::validate_name` so the two stores apply the same
/// filesystem-safety rules.
///
/// Rules:
/// - non-empty
/// - length ≤ [`SUMMARY_TOPIC_MAX`]
/// - no path separators (`/`, `\`)
/// - no `..` component
/// - no NUL bytes
pub fn validate_topic(topic: &str) -> Result<(), SummaryError> {
    if topic.is_empty() {
        return Err(SummaryError::InvalidTopic(topic.to_string()));
    }
    if topic.len() > SUMMARY_TOPIC_MAX {
        return Err(SummaryError::InvalidTopic(topic.to_string()));
    }
    if topic.contains('/') || topic.contains('\\') {
        return Err(SummaryError::InvalidTopic(topic.to_string()));
    }
    if topic == ".." {
        return Err(SummaryError::InvalidTopic(topic.to_string()));
    }
    if topic.contains('\0') {
        return Err(SummaryError::InvalidTopic(topic.to_string()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Free-form metadata attached to a summary. Identical shape to
/// `crate::prompt::PromptMetadata` — kept as a separate type so the
/// two stores can evolve independently, but with the same field
/// semantics and the same length caps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryMetadata {
    /// One-paragraph description of what this summary covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who created the summary (free-form string, often an email).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Tags for grouping / grepping.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Validate [`SummaryMetadata`]. Mirrors
/// `crate::prompt::validate_metadata` 1:1 so the two stores reject
/// the same bad inputs.
pub fn validate_metadata(meta: &SummaryMetadata) -> Result<(), SummaryError> {
    if let Some(d) = &meta.description {
        if d.len() > SUMMARY_DESCRIPTION_MAX {
            return Err(SummaryError::InvalidMetadata(format!(
                "description is {} bytes; max is {}",
                d.len(),
                SUMMARY_DESCRIPTION_MAX
            )));
        }
    }
    if let Some(a) = &meta.author {
        if a.len() > SUMMARY_AUTHOR_MAX {
            return Err(SummaryError::InvalidMetadata(format!(
                "author is {} bytes; max is {}",
                a.len(),
                SUMMARY_AUTHOR_MAX
            )));
        }
    }
    if meta.tags.len() > SUMMARY_TAGS_MAX {
        return Err(SummaryError::InvalidMetadata(format!(
            "metadata has {} tags; max is {}",
            meta.tags.len(),
            SUMMARY_TAGS_MAX
        )));
    }
    for t in &meta.tags {
        if t.len() > SUMMARY_TAG_MAX {
            return Err(SummaryError::InvalidMetadata(format!(
                "tag {:?} is {} bytes; max is {}",
                t,
                t.len(),
                SUMMARY_TAG_MAX
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Source / history DTOs
// ---------------------------------------------------------------------------

/// Provenance for a summary. Tells a future reader where this
/// snapshot came from so they can correlate it with a `RunReport`
/// JSON or a CI run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummarySource {
    /// Optional identifier for the run that produced the summary.
    /// Typically the `session_id` printed in `magent run --json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Provider name (`"ollama"`, `"deepseek"`, `"sim"`).
    #[serde(default)]
    pub provider: String,
    /// Model name. Empty when the provider reports no model.
    #[serde(default)]
    pub model: String,
    /// Number of messages in the conversation **before**
    /// compression ran. Useful for "we dropped N out of M" reports.
    #[serde(default)]
    pub original_message_count: usize,
    /// Snapshot of the compression policy that produced this window.
    /// Captured so a reader can tell whether the stored window was
    /// generated with aggressive or conservative settings.
    #[serde(default)]
    pub policy: CompressionPolicySnapshot,
}

/// JSON-friendly snapshot of the active `CompressionPolicy`. We
/// store the numbers verbatim (rather than the live struct) so a
/// stored summary survives even if the on-disk `CompressionPolicy`
/// struct grows new fields later.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompressionPolicySnapshot {
    /// `CompressionPolicy::max_messages` at save time.
    pub max_messages: usize,
    /// `CompressionPolicy::tool_content_max_chars` at save time.
    pub tool_content_max_chars: usize,
}

impl From<&crate::conversation::CompressionPolicy> for CompressionPolicySnapshot {
    fn from(p: &crate::conversation::CompressionPolicy) -> Self {
        Self {
            max_messages: p.max_messages,
            tool_content_max_chars: p.tool_content_max_chars,
        }
    }
}

/// One entry in [`SummaryRecord::history`]. Older snapshots are
/// kept so users can roll back without a `git`-style VCS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Unix seconds. Matches `updated_at` at the time the entry was
    /// promoted into history.
    pub updated_at: u64,
    /// `CompressionStats::kept` of the superseded snapshot.
    pub kept: usize,
    /// Session id of the run that produced the superseded snapshot,
    /// if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Message DTO
// ---------------------------------------------------------------------------

/// On-disk shape of an [`agent_runner::Message`].
///
/// We don't `#[derive]` `Serialize` / `Deserialize` on `Message`
/// itself because that would couple the runtime ReAct loop to a
/// serde-versioned wire format. Instead the store translates at the
/// boundary via [`MessageDto::from_message`] /
/// [`MessageDto::into_message`]. The two are guaranteed equivalent
/// at the JSON level; the DTO is stable across the `Message`
/// representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageDto {
    /// `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    pub role: String,
    /// Message body. For tool messages this is the (possibly
    /// truncated) tool result.
    pub content: String,
    /// `true` if the message carried a tool call. We don't store
    /// the full `ToolCall` (the arguments are usually huge and the
    /// LLM doesn't need them replayed); `tool_call_id` on the
    /// corresponding `tool` message is what really matters.
    #[serde(default, skip_serializing_if = "is_false")]
    pub had_tool_call: bool,
    /// `tool_call_id` for tool-result messages. Preserved verbatim
    /// so the LLM can correlate the result with the original call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

impl MessageDto {
    /// Construct a DTO from a runtime `Message`.
    pub fn from_message(m: &Message) -> Self {
        Self {
            role: role_to_string(m.role),
            content: m.content.clone(),
            had_tool_call: m.tool_call.is_some(),
            tool_call_id: m.tool_call_id.clone(),
        }
    }

    /// Re-hydrate a runtime `Message` from this DTO. We can't
    /// recover the full `ToolCall` (we deliberately don't store
    /// it), so the assistant-side `tool_call` is left as `None`
    /// and the `had_tool_call` flag is lost. The reconstructed
    /// `Message` is still usable for token estimation and
    /// injection, but cannot be re-dispatched by the runtime.
    pub fn into_message(self) -> Message {
        match self.role.as_str() {
            "system" => Message::system(&self.content),
            "user" => Message::user(&self.content),
            "assistant" => {
                if let Some(id) = self.tool_call_id {
                    // Defensive: tool_call_id on an assistant message
                    // is unusual (it's normally only on tool-result
                    // messages). Treat the assistant as plain text.
                    let _ = id;
                }
                Message::assistant_text(&self.content)
            }
            "tool" => {
                let id = self.tool_call_id.as_deref().unwrap_or("");
                Message::tool(id, &self.content)
            }
            other => {
                // Unknown role — fall back to user. Better than
                // panicking and matches the rest of the codebase's
                // "never panic on bad input" stance.
                Message::user(&format!("[unknown role {:?}] {}", other, self.content))
            }
        }
    }
}

fn role_to_string(r: Role) -> String {
    match r {
        Role::System => "system".to_string(),
        Role::User => "user".to_string(),
        Role::Assistant => "assistant".to_string(),
        Role::Tool => "tool".to_string(),
    }
}

// ---------------------------------------------------------------------------
// SummaryRecord
// ---------------------------------------------------------------------------

/// One persisted summary. See the module-level doc-comment in
/// `mod.rs` for the JSON shape and rationale.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryRecord {
    /// Bumped whenever the JSON shape changes in a breaking way.
    pub schema_version: u32,
    /// Lower-case, filesystem-safe identifier used as the file stem.
    pub topic: String,
    /// Where this summary came from.
    pub source: SummarySource,
    /// The compressed head/tail window the LLM actually saw on the
    /// last call before the run ended.
    pub head_tail_window: Vec<MessageDto>,
    /// Optional natural-language summary produced by the LLM (or a
    /// human). `None` when `--llm-summarize` was not requested or
    /// the summarisation call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_summary: Option<String>,
    /// Compression counters captured at save time.
    pub stats: CompressionStats,
    /// Free-form metadata.
    #[serde(default)]
    pub metadata: SummaryMetadata,
    /// FIFO of superseded snapshots. Capped at [`HISTORY_MAX`].
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    /// Unix seconds of the original write.
    #[serde(default = "now_unix_seconds")]
    pub created_at: u64,
    /// Unix seconds of the most recent write.
    #[serde(default = "now_unix_seconds")]
    pub updated_at: u64,
}

impl SummaryRecord {
    /// Version-aware equality. Two records are considered
    /// "compatible" only when their `schema_version` matches AND
    /// their fields compare equal. Use this whenever the caller
    /// might be holding a record loaded by an older binary.
    pub fn compatible_with(&self, other: &Self) -> Result<bool, SummaryError> {
        if self.schema_version != other.schema_version {
            return Err(SummaryError::SchemaMismatch {
                lhs: self.schema_version,
                rhs: other.schema_version,
            });
        }
        Ok(self == other)
    }

    /// Approximate serialised size in bytes (cheap — uses
    /// `serde_json::to_vec` and returns its length). Used by the
    /// host store to enforce [`MAX_RECORD_BYTES`] without paying
    /// for an extra string allocation.
    pub fn serialised_size(&self) -> usize {
        serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
    }

    /// Validate the record's data invariants. The storage layer
    /// calls this before persisting so a paste accident doesn't
    /// land on disk. Cheap; safe to call on every `save`.
    pub fn validate(&self) -> Result<(), SummaryError> {
        validate_topic(&self.topic)?;
        validate_metadata(&self.metadata)?;
        if let Some(s) = &self.llm_summary {
            if s.len() > SUMMARY_LLM_MAX {
                return Err(SummaryError::SummaryTooLarge {
                    size: s.len(),
                    max: SUMMARY_LLM_MAX,
                });
            }
        }
        if self.history.len() > HISTORY_MAX {
            return Err(SummaryError::InvalidMetadata(format!(
                "history has {} entries; max is {}",
                self.history.len(),
                HISTORY_MAX
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`SummaryRecord`]. Centralises defaulting so the CLI
/// and the runtime have a single canonical path for "I just finished
/// a run, please persist the compressed tail".
#[derive(Debug, Clone)]
pub struct SummaryBuilder {
    topic: String,
    source: SummarySource,
    window: Vec<MessageDto>,
    llm_summary: Option<String>,
    stats: CompressionStats,
    metadata: SummaryMetadata,
    history: Vec<HistoryEntry>,
    created_at: u64,
}

impl SummaryBuilder {
    /// Start a new builder for `topic`. Validates the topic name
    /// up-front so subsequent calls can't poison the record.
    pub fn new(topic: impl Into<String>) -> Result<Self, SummaryError> {
        let topic = topic.into();
        validate_topic(&topic)?;
        Ok(Self {
            topic,
            source: SummarySource::default(),
            window: Vec::new(),
            llm_summary: None,
            stats: CompressionStats::default(),
            metadata: SummaryMetadata::default(),
            history: Vec::new(),
            created_at: now_unix_seconds(),
        })
    }

    /// Set the [`SummarySource`]. Overwrites any prior value.
    pub fn with_source(mut self, source: SummarySource) -> Self {
        self.source = source;
        self
    }

    /// Set the head/tail window. Accepts runtime `Message`s and
    /// converts them to DTOs internally.
    pub fn with_window(mut self, window: &[Message]) -> Self {
        self.window = window.iter().map(MessageDto::from_message).collect();
        self
    }

    /// Set the head/tail window from pre-built DTOs. Useful when
    /// the caller already serialised to DTOs at the boundary —
    /// e.g. the runner's `--load-summary` path that reads back a
    /// stored record and wants to re-save it under a different
    /// topic without rebuilding `Message`s.
    pub fn with_window_slice(mut self, window: &[MessageDto]) -> Self {
        self.window = window.to_vec();
        self
    }

    /// Set the LLM-generated natural-language summary.
    pub fn with_llm_summary(mut self, summary: impl Into<String>) -> Self {
        self.llm_summary = Some(summary.into());
        self
    }

    /// Set the LLM summary, taking `Option<String>` so the caller
    /// doesn't have to drop a `None` itself.
    pub fn with_llm_summary_opt(mut self, summary: Option<String>) -> Self {
        self.llm_summary = summary;
        self
    }

    /// Set the compression counters.
    pub fn with_stats(mut self, stats: CompressionStats) -> Self {
        self.stats = stats;
        self
    }

    /// Set the free-form metadata.
    pub fn with_metadata(mut self, metadata: SummaryMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Inherit history from a previously persisted record. The
    /// caller (the `save` function) takes care of truncating to
    /// [`HISTORY_MAX`].
    pub fn with_history(mut self, history: Vec<HistoryEntry>) -> Self {
        self.history = history;
        self
    }

    /// Override `created_at`. Used by `save` to preserve the
    /// original timestamp across updates.
    pub fn with_created_at(mut self, ts: u64) -> Self {
        self.created_at = ts;
        self
    }

    /// Finalise into a [`SummaryRecord`]. Validates metadata and
    /// `llm_summary` length so a bad builder can't write a bad
    /// record.
    pub fn build(self) -> Result<SummaryRecord, SummaryError> {
        let now = now_unix_seconds();
        let rec = SummaryRecord {
            schema_version: CURRENT_SCHEMA_VERSION,
            topic: self.topic,
            source: self.source,
            head_tail_window: self.window,
            llm_summary: self.llm_summary,
            stats: self.stats,
            metadata: self.metadata,
            history: self.history,
            created_at: self.created_at,
            updated_at: now,
        };
        rec.validate()?;
        Ok(rec)
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

#[inline]
fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_window(n: usize) -> Vec<crate::agent_runner::Message> {
        let mut v = Vec::with_capacity(n);
        v.push(crate::agent_runner::Message::system("You are a coach."));
        v.push(crate::agent_runner::Message::user("orig task"));
        for i in 0..(n.saturating_sub(2)) {
            v.push(crate::agent_runner::Message::assistant_text(&format!(
                "turn-{}",
                i
            )));
        }
        v
    }

    // ---------- topic validation ----------

    #[test]
    fn validate_topic_accepts_simple_ascii() {
        assert!(validate_topic("weekly-health").is_ok());
        assert!(validate_topic("a").is_ok());
    }

    #[test]
    fn validate_topic_rejects_empty() {
        assert!(matches!(
            validate_topic(""),
            Err(SummaryError::InvalidTopic(_))
        ));
    }

    #[test]
    fn validate_topic_rejects_slash() {
        assert!(matches!(
            validate_topic("a/b"),
            Err(SummaryError::InvalidTopic(_))
        ));
        assert!(matches!(
            validate_topic("a\\b"),
            Err(SummaryError::InvalidTopic(_))
        ));
    }

    #[test]
    fn validate_topic_rejects_double_dot() {
        assert!(matches!(
            validate_topic(".."),
            Err(SummaryError::InvalidTopic(_))
        ));
    }

    #[test]
    fn validate_topic_rejects_too_long() {
        let t: String = "x".repeat(SUMMARY_TOPIC_MAX + 1);
        assert!(matches!(
            validate_topic(&t),
            Err(SummaryError::InvalidTopic(_))
        ));
    }

    #[test]
    fn validate_topic_rejects_nul_byte() {
        assert!(matches!(
            validate_topic("a\0b"),
            Err(SummaryError::InvalidTopic(_))
        ));
    }

    // ---------- metadata validation ----------

    #[test]
    fn validate_metadata_accepts_empty() {
        assert!(validate_metadata(&SummaryMetadata::default()).is_ok());
    }

    #[test]
    fn validate_metadata_rejects_long_description() {
        let m = SummaryMetadata {
            description: Some("x".repeat(SUMMARY_DESCRIPTION_MAX + 1)),
            ..Default::default()
        };
        assert!(matches!(
            validate_metadata(&m),
            Err(SummaryError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn validate_metadata_rejects_long_author() {
        let m = SummaryMetadata {
            author: Some("x".repeat(SUMMARY_AUTHOR_MAX + 1)),
            ..Default::default()
        };
        assert!(matches!(
            validate_metadata(&m),
            Err(SummaryError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn validate_metadata_rejects_too_many_tags() {
        let m = SummaryMetadata {
            tags: (0..(SUMMARY_TAGS_MAX + 1))
                .map(|i| format!("t{}", i))
                .collect(),
            ..Default::default()
        };
        assert!(matches!(
            validate_metadata(&m),
            Err(SummaryError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn validate_metadata_rejects_long_tag() {
        let m = SummaryMetadata {
            tags: vec!["x".repeat(SUMMARY_TAG_MAX + 1)],
            ..Default::default()
        };
        assert!(matches!(
            validate_metadata(&m),
            Err(SummaryError::InvalidMetadata(_))
        ));
    }

    // ---------- builder ----------

    #[test]
    fn builder_rejects_bad_topic() {
        let r = SummaryBuilder::new("bad/name");
        assert!(matches!(r, Err(SummaryError::InvalidTopic(_))));
    }

    #[test]
    fn builder_round_trip_through_serialise() {
        let mut window = make_window(4);
        window.push(crate::agent_runner::Message::tool("c1", "ok"));
        let rec = SummaryBuilder::new("weekly-health")
            .unwrap()
            .with_window(&window)
            .with_llm_summary("User slept 6h avg last week")
            .with_stats(CompressionStats {
                kept: 5,
                dropped: 10,
                tool_results_truncated: 1,
                bytes_saved: 200,
            })
            .with_metadata(SummaryMetadata {
                description: Some("week 32".to_string()),
                tags: vec!["weekly".into()],
                author: None,
            })
            .build()
            .unwrap();
        assert_eq!(rec.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(rec.topic, "weekly-health");
        assert_eq!(rec.head_tail_window.len(), window.len());
        assert_eq!(
            rec.llm_summary.as_deref(),
            Some("User slept 6h avg last week")
        );

        let json = serde_json::to_string(&rec).unwrap();
        let parsed: SummaryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, rec);
    }

    #[test]
    fn builder_rejects_oversized_llm_summary() {
        let huge = "x".repeat(SUMMARY_LLM_MAX + 1);
        let r = SummaryBuilder::new("big")
            .unwrap()
            .with_llm_summary(huge)
            .build();
        assert!(matches!(r, Err(SummaryError::SummaryTooLarge { .. })));
    }

    // ---------- MessageDto ----------

    #[test]
    fn dto_round_trips_system_message() {
        let m = crate::agent_runner::Message::system("sys");
        let d = MessageDto::from_message(&m);
        assert_eq!(d.role, "system");
        assert_eq!(d.content, "sys");
        assert!(!d.had_tool_call);
        assert!(d.tool_call_id.is_none());

        let m2 = d.into_message();
        assert_eq!(m2.role, Role::System);
        assert_eq!(m2.content, "sys");
        assert!(m2.tool_call.is_none());
        assert!(m2.tool_call_id.is_none());
    }

    #[test]
    fn dto_round_trips_tool_message_with_id() {
        let m = crate::agent_runner::Message::tool("c1", "ok");
        let d = MessageDto::from_message(&m);
        assert_eq!(d.role, "tool");
        assert_eq!(d.tool_call_id.as_deref(), Some("c1"));
        let m2 = d.into_message();
        assert_eq!(m2.role, Role::Tool);
        assert_eq!(m2.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(m2.content, "ok");
    }

    #[test]
    fn dto_handles_unknown_role_in_message_field() {
        let d = MessageDto {
            role: "wizard".into(),
            content: "hello".into(),
            had_tool_call: false,
            tool_call_id: None,
        };
        let m = d.into_message();
        assert_eq!(m.role, Role::User);
        assert!(m.content.contains("wizard"));
    }

    #[test]
    fn dto_skips_false_had_tool_call_in_json() {
        let m = crate::agent_runner::Message::user("hi");
        let d = MessageDto::from_message(&m);
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("had_tool_call"), "json was {}", json);
    }

    #[test]
    fn dto_serialises_true_had_tool_call() {
        let mut args = std::collections::HashMap::new();
        args.insert(
            "x".to_string(),
            serde_json::Value::Number(serde_json::Number::from(1u64)),
        );
        let call = crate::agent_runner::ToolCall {
            name: "noop".to_string(),
            arguments: args,
        };
        let m = crate::agent_runner::Message::assistant_tool_call(call);
        let d = MessageDto::from_message(&m);
        assert!(d.had_tool_call);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"had_tool_call\":true"), "json was {}", json);
    }

    // ---------- validate() ----------

    #[test]
    fn validate_rejects_history_overflow() {
        let mut rec = SummaryBuilder::new("x").unwrap().build().unwrap();
        rec.history = (0..(HISTORY_MAX + 1))
            .map(|i| HistoryEntry {
                updated_at: i as u64,
                kept: 0,
                source_session_id: None,
            })
            .collect();
        assert!(matches!(
            rec.validate(),
            Err(SummaryError::InvalidMetadata(_))
        ));
    }

    // ---------- compatible_with ----------

    #[test]
    fn compatible_with_rejects_schema_mismatch() {
        let a = SummaryBuilder::new("a").unwrap().build().unwrap();
        let mut b = a.clone();
        b.schema_version = CURRENT_SCHEMA_VERSION + 1;
        let r = a.compatible_with(&b);
        assert!(matches!(r, Err(SummaryError::SchemaMismatch { .. })));
    }

    #[test]
    fn compatible_with_accepts_equal_records() {
        let a = SummaryBuilder::new("a").unwrap().build().unwrap();
        let b = a.clone();
        assert!(a.compatible_with(&b).unwrap());
    }

    // ---------- serialised_size ----------

    #[test]
    fn serialised_size_grows_with_window() {
        let small = SummaryBuilder::new("s")
            .unwrap()
            .with_window(&make_window(2))
            .build()
            .unwrap();
        let big = SummaryBuilder::new("s")
            .unwrap()
            .with_window(&make_window(20))
            .build()
            .unwrap();
        assert!(big.serialised_size() > small.serialised_size());
    }

    // ---------- error Display ----------

    #[test]
    fn error_display_includes_context() {
        let e = SummaryError::InvalidTopic("../bad".into());
        let s = e.to_string();
        assert!(s.contains("invalid summary topic"));
        assert!(s.contains("../bad"));

        let e = SummaryError::NotFound("ghost".into());
        assert!(e.to_string().contains("ghost"));

        let e = SummaryError::SummaryTooLarge { size: 99, max: 10 };
        assert!(e.to_string().contains("99"));
        assert!(e.to_string().contains("10"));

        let e = SummaryError::RecordTooLarge {
            size: 1000,
            max: 500,
        };
        assert!(e.to_string().contains("1000"));
        assert!(e.to_string().contains("500"));

        let e = SummaryError::SchemaMismatch { lhs: 1, rhs: 2 };
        assert!(e.to_string().contains('1'));
        assert!(e.to_string().contains('2'));

        let e = SummaryError::AlreadyExists("dup".into());
        let s = e.to_string();
        assert!(s.contains("already exists"));
        assert!(s.contains("dup"));
        assert!(s.contains("--save-summary-overwrite"));
    }
}
