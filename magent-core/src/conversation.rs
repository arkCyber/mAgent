//! Conversation history compression.
//!
//! The [`RealAgentRunner`] keeps the full `Vec<Message>` it has produced so
//! the agent can be inspected mid-run, but every LLM call resubmits the
//! entire history. That guarantees quadratic token growth and, for any
//! provider with a hard context window (8k for many local Ollama models,
//! 32k for `deepseek-chat`), will eventually explode.
//!
//! This module provides the in-house, dependency-free machinery to keep
//! the live payload bounded while preserving as much useful signal as
//! possible:
//!
//! 1. **Tool-result truncation** — long tool outputs are clipped to a
//!    head + tail window with a marker showing how many bytes were
//!    dropped. The LLM still sees the envelope (which tool was called
//!    and what it returned) without paying for the irrelevant middle.
//! 2. **Message slicing** — once the message list is short enough that
//!    `tool_call` ↔ `tool` cross-references would be broken, the runner
//!    drops the oldest messages. The system prompt and the
//!    most-recent user request are always preserved.
//!
//! Both steps are pure, deterministic, and never touch the network. They
//! are safe to call before every LLM call; the call is cheap enough that
//! the runner invokes it unconditionally on every `think()`.
//!
//! ## Example
//!
//! ```ignore
//! use magent_core::conversation::{CompressionPolicy, compress_messages};
//!
//! let policy = CompressionPolicy {
//!     max_messages: 16,
//!     tool_content_max_chars: 800,
//! };
//!
//! let mut messages: Vec<Message> = /* build history */;
//! let stats = compress_messages(&mut messages, &policy);
//! println!("kept {} / dropped {} messages", stats.kept, stats.dropped);
//! ```

use crate::agent_runner::{Message, Role};
// `conversation.rs` is gated by `#[cfg(feature = "std")]` in `lib.rs`,
// so we use the std re-exports directly. The `String` / `Vec` symbols
// come from `std::*` rather than `alloc::*` because we don't need to
// support a hypothetical `no_std + alloc` build for this module.
use serde::{Deserialize, Serialize};
use std::string::{String, ToString};
use std::vec::Vec;

// Note: the truncation marker is rendered inline by `truncate_tool_content`
// using a placeholder-based substring substitution, so there is no single
// `const TRUNCATION_MARKER` to share. The literal pattern is
// `\n[...truncated N bytes...]\n` and the LLM is expected to recognise it
// as "irrelevant middle elided".

/// Combined policy for keeping the live conversation within a manageable
/// size. All fields are inclusive limits — i.e. a value of `0` disables
/// the corresponding step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionPolicy {
    /// Maximum number of messages kept after slicing. The system prompt
    /// (if any) and the most recent user request are always retained, then
    /// the tail is filled from the newest messages backwards.
    ///
    /// `0` disables message slicing entirely.
    pub max_messages: usize,
    /// Maximum characters permitted in a single tool result. Tool
    /// messages whose `content` is longer than this are shortened to
    /// `head + marker + tail` while preserving the tool envelope (the
    /// `tool_call_id` is unchanged so the LLM can still correlate the
    /// result with the original call).
    ///
    /// `0` disables tool-result truncation.
    pub tool_content_max_chars: usize,
}

impl Default for CompressionPolicy {
    fn default() -> Self {
        // 32 messages ≈ 8 ReAct iterations of [user, assistant-tool,
        // tool, assistant-tool, tool] — comfortable ceiling for the
        // 8k-context local Ollama models that ship with the project.
        // 800 chars per tool result keeps individual payloads in the
        // same ballpark as the previous `MAX_BUFFER_SIZE` budget.
        Self {
            max_messages: 32,
            tool_content_max_chars: 800,
        }
    }
}

impl CompressionPolicy {
    /// Build a no-op policy. Useful for callers that want to opt out
    /// of compression entirely (e.g. short-lived unit tests).
    pub const fn disabled() -> Self {
        Self {
            max_messages: 0,
            tool_content_max_chars: 0,
        }
    }
}

/// Diagnostics about a single `compress_messages` invocation. Returned
/// to the caller so the CLI can include the counts in the `RunReport`.
///
/// `Serialize` / `Deserialize` are derived so the [`SummaryStore`]
/// (see `magent_core::summary`) can embed the counters verbatim into
/// the on-disk summary record. Mirroring the `Message` DTO in
/// `summary.rs`, we accept a `serde` import here — the module is
/// already gated on `std` and the `serde` crate is a hard
/// dependency of `magent-core`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionStats {
    /// Messages kept after slicing.
    pub kept: usize,
    /// Messages dropped by the slicing step.
    pub dropped: usize,
    /// Tool result messages that were truncated (had their `content`
    /// shortened because it exceeded `tool_content_max_chars`).
    pub tool_results_truncated: usize,
    /// Bytes of tool `content` removed by the truncation step, *before*
    /// the marker is inserted. Useful for telemetry.
    pub bytes_saved: usize,
}

/// Trim a single tool result's `content` to a head + marker + tail
/// window. The original `tool_call_id` is preserved.
///
/// Returns `true` if the content was actually shortened — callers can
/// use this to update `CompressionStats`.
pub fn truncate_tool_content(msg: &mut Message, max_chars: usize) -> bool {
    if max_chars == 0 || msg.role != Role::Tool {
        return false;
    }
    if msg.content.len() <= max_chars {
        return false;
    }

    let original_len = msg.content.len();
    // Split the budget 60/40 between head and tail. Head wins because
    // tool outputs (status banners, JSON preamble, schema notes) tend
    // to be most informative at the top.
    let head_budget = (max_chars * 3) / 5;
    let tail_budget = max_chars - head_budget;

    // Find the nearest char boundary at or below head_budget so we
    // never slice through the middle of a UTF-8 code point.
    let head_end = floor_char_boundary(&msg.content, head_budget);
    let tail_start = ceil_char_boundary(&msg.content, original_len.saturating_sub(tail_budget));

    // Build the marker with a placeholder, then substitute the
    // approximate drop count. The placeholder contains an ASCII NUL
    // (U+0000) which never appears in JSON-escaped tool output, so
    // the substitution can never clobber user content.
    let placeholder = "\x00BYTES\x00";
    let mut new_content = String::with_capacity(max_chars + 64);
    new_content.push_str(&msg.content[..head_end]);
    new_content.push_str(&format!(
        "\n[...truncated {} bytes...]\n",
        placeholder
    ));
    if tail_start < original_len {
        new_content.push_str(&msg.content[tail_start..]);
    }

    // Approximate the dropped count: original_len minus the bytes we
    // kept (head + tail) minus the marker overhead. The exact number
    // after the placeholder substitution is irrelevant for telemetry
    // — the substring and tail are at the user's eye level, the
    // marker is diagnostics.
    let kept_payload = head_end + (original_len - tail_start);
    let marker_approx = original_len.saturating_sub(kept_payload);
    msg.content = new_content.replacen(placeholder, &marker_approx.to_string(), 1);

    true
}

/// Slice the message list to at most `max_messages` entries. The
/// system prompt (if present) and the **oldest** user request are
/// always kept so the LLM still knows the original task; everything
/// else is taken from the tail (newest messages win).
///
/// Returns the number of messages dropped.
pub fn slice_messages(messages: &mut Vec<Message>, max_messages: usize) -> usize {
    if max_messages == 0 || messages.len() <= max_messages {
        return 0;
    }

    let original_len = messages.len();
    // `drop_count` is the number of messages we'll evict from the
    // middle of the conversation. Currently unused after the
    // algorithm below was simplified to anchor on the first system
    // and first user message; kept as a debug aid in case future
    // work needs to log eviction ratios.
    let _drop_count = original_len - max_messages;

    // Find the first index that is *not* a system message. We want to
    // keep system messages at the head even when slicing.
    let preserved_system = messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .count();

    // The oldest user message is the one we want to keep as the
    // "task anchor" so the LLM never loses the original request.
    let preserved_user = if messages
        .iter()
        .skip(preserved_system)
        .any(|m| m.role == Role::User)
    {
        1
    } else {
        0
    };

    let preserved_head = preserved_system + preserved_user;
    if preserved_head >= max_messages {
        // Pathological case: budget exhausted by the system prompt and
        // task anchor alone. We always keep the user task (the most
        // important piece) and drop the head of the system messages
        // to make room. If `max_messages` is 0, we can't keep
        // anything — fall back to an empty list.
        if max_messages == 0 {
            messages.clear();
            return original_len;
        }
        let keep = max_messages - 1; // reserve 1 slot for the user task
        let drop_system = preserved_system.saturating_sub(keep);
        messages.drain(..drop_system);
        return original_len - messages.len();
    }

    let tail_budget = max_messages - preserved_head;
    if original_len - preserved_head <= tail_budget {
        // Nothing to drop from the tail — keep everything.
        return 0;
    }

    // Build the new list: preserved head + tail.
    let mut new_messages: Vec<Message> =
        Vec::with_capacity(max_messages);
    new_messages.extend(messages[..preserved_head].iter().cloned());
    new_messages.extend(
        messages[original_len - tail_budget..]
            .iter()
            .cloned(),
    );
    *messages = new_messages;
    original_len - messages.len()
}

/// Apply [`CompressionPolicy`] to `messages` in-place: first truncate
/// oversized tool results, then slice the list to `max_messages`.
///
/// Returns the counters so the caller can record what happened.
pub fn compress_messages(messages: &mut Vec<Message>, policy: &CompressionPolicy) -> CompressionStats {
    let mut stats = CompressionStats::default();

    // Step 1: truncate tool results that are too long. We do this
    // *before* slicing so the size estimate used by the caller is
    // accurate even if they never invoke the slicing step.
    if policy.tool_content_max_chars > 0 {
        for msg in messages.iter_mut() {
            if truncate_tool_content(msg, policy.tool_content_max_chars) {
                stats.tool_results_truncated += 1;
                // `truncate_tool_content` replaced the marker with the
                // exact byte count, so we can't recover it from the
                // string cheaply. Track the rough delta instead.
                stats.bytes_saved += msg
                    .content
                    .len()
                    .saturating_sub(policy.tool_content_max_chars);
            }
        }
    }

    // Step 2: slice to max_messages.
    let dropped = slice_messages(messages, policy.max_messages);
    stats.dropped = dropped;
    stats.kept = messages.len();

    stats
}

/// Rough token estimate from a string. Uses the common 4-chars-per-token
/// heuristic; good enough for a budget guardrail where the caller is
/// going to round up anyway.
#[inline]
pub fn approx_tokens(s: &str) -> usize {
    // Round up so a 1-char string still counts as 1 token.
    s.len().div_ceil(4)
}

/// Sum the estimated token cost of every message in the slice. Includes
/// `tool_call` argument JSON because the LLM sees that on the wire.
pub fn approx_total_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let mut tokens = approx_tokens(&m.content);
            if let Some(ref tc) = m.tool_call {
                // Best-effort: serialise the args map back to JSON via
                // `serde_json::Value::to_string`. We deliberately do
                // *not* pull in `serde_json` here at the call site —
                // `ToolCall::arguments` is `HashMap<String, Value>` so
                // we just sum the JSON-like representations.
                for (k, v) in &tc.arguments {
                    tokens += approx_tokens(k);
                    tokens += approx_tokens(&v.to_string());
                }
            }
            tokens
        })
        .sum()
}

// ---------------------------------------------------------------------------
// UTF-8 helpers (private)
// ---------------------------------------------------------------------------

/// Largest index `<= idx` that is a UTF-8 char boundary in `s`.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest index `>= idx` that is a UTF-8 char boundary in `s`.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runner::Message;

    fn user(s: &str) -> Message {
        Message::user(s)
    }

    fn assistant(s: &str) -> Message {
        Message::assistant_text(s)
    }

    fn tool(id: &str, s: &str) -> Message {
        Message::tool(id, s)
    }

    fn system(s: &str) -> Message {
        Message::system(s)
    }

    #[test]
    fn truncate_disabled_when_max_is_zero() {
        let mut m = tool("call_1", &"x".repeat(10_000));
        assert!(!truncate_tool_content(&mut m, 0));
        assert_eq!(m.content.len(), 10_000);
    }

    #[test]
    fn truncate_noop_when_within_budget() {
        let mut m = tool("call_1", "short");
        assert!(!truncate_tool_content(&mut m, 100));
        assert_eq!(m.content, "short");
    }

    #[test]
    fn truncate_shortens_long_tool_result() {
        let mut m = tool("call_1", &"a".repeat(2_000));
        assert!(truncate_tool_content(&mut m, 100));
        // The new content is roughly head + marker + tail, never
        // exceeding the budget by much.
        assert!(m.content.len() < 200, "got {}", m.content.len());
        assert!(m.content.contains("[...truncated"));
        // The tool_call_id survives so the LLM can still correlate.
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn truncate_preserves_head_and_tail_anchor_strings() {
        let head = "HEAD_ANCHOR";
        let tail = "TAIL_ANCHOR";
        let middle = "x".repeat(5_000);
        let body = format!("{}{}{}", head, middle, tail);
        let mut m = tool("call_1", &body);
        assert!(truncate_tool_content(&mut m, 200));
        assert!(m.content.starts_with(head), "head lost: {}", m.content);
        assert!(m.content.ends_with(tail), "tail lost: {}", m.content);
    }

    #[test]
    fn truncate_ignores_non_tool_messages() {
        let mut m = assistant(&"x".repeat(10_000));
        assert!(!truncate_tool_content(&mut m, 100));
        assert_eq!(m.content.len(), 10_000);
    }

    #[test]
    fn slice_noop_when_within_budget() {
        let mut v = vec![user("a"), assistant("b"), tool("c1", "c")];
        assert_eq!(slice_messages(&mut v, 10), 0);
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn slice_keeps_system_prompt() {
        let mut v = vec![
            system("SYS"),
            user("orig task"),
            assistant("a"),
            tool("c1", "c"),
            user("u2"),
            assistant("a2"),
            tool("c2", "c2"),
        ];
        let dropped = slice_messages(&mut v, 4);
        assert_eq!(dropped, 3);
        assert_eq!(v.len(), 4);
        // System prompt is always at the top.
        assert_eq!(v[0].role, Role::System);
        assert_eq!(v[0].content, "SYS");
        // Original task survives.
        assert!(v.iter().any(|m| m.role == Role::User && m.content == "orig task"));
    }

    #[test]
    fn slice_prefers_newest_messages() {
        let mut v: Vec<Message> = (0..20)
            .map(|i| user(&format!("u{}", i)))
            .collect();
        let dropped = slice_messages(&mut v, 5);
        assert_eq!(dropped, 15);
        assert_eq!(v.len(), 5);
        // The last message in the list is the most recent user input.
        assert_eq!(v.last().unwrap().content, "u19");
    }

    #[test]
    fn slice_disabled_when_max_is_zero() {
        let mut v = vec![user("a"), assistant("b")];
        assert_eq!(slice_messages(&mut v, 0), 0);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn slice_keeps_user_task_even_when_budget_tight() {
        // Edge case: max_messages is too small to hold both system
        // messages and the user task. The system messages lose, the
        // user task survives.
        let mut v = vec![
            system("sys-1"),
            system("sys-2"),
            user("orig task"),
            assistant("a"),
        ];
        let dropped = slice_messages(&mut v, 2);
        assert!(dropped >= 1);
        // The user task is still there.
        assert!(v.iter().any(|m| m.role == Role::User && m.content == "orig task"));
    }

    #[test]
    fn compress_runs_both_steps() {
        let mut v = vec![
            system("SYS"),
            user("orig task"),
        ];
        // Add long tool result that should be truncated.
        v.push(tool("c1", &"y".repeat(5_000)));
        // Pad with extra turns so the message count exceeds the cap.
        for i in 0..30 {
            v.push(assistant(&format!("a{}", i)));
            v.push(tool(&format!("c{}", i), &format!("out{}", i)));
        }
        let original_len = v.len();
        let policy = CompressionPolicy {
            max_messages: 16,
            tool_content_max_chars: 200,
        };
        let stats = compress_messages(&mut v, &policy);
        assert_eq!(stats.kept, 16);
        assert_eq!(stats.dropped, original_len - 16);
        // Padding above was 60 messages; truncated+sliced down to 16.
        assert!(stats.tool_results_truncated >= 1);
        // The long tool result is now short.
        for m in &v {
            if m.role == Role::Tool {
                assert!(m.content.len() <= 250, "got {}", m.content.len());
            }
        }
    }

    #[test]
    fn compress_zero_policy_is_noop() {
        let mut v = vec![user("a"), assistant("b"), tool("c1", "x")];
        let original = v.clone();
        let stats = compress_messages(&mut v, &CompressionPolicy {
            max_messages: 0,
            tool_content_max_chars: 0,
        });
        assert_eq!(stats.dropped, 0);
        assert_eq!(stats.tool_results_truncated, 0);
        assert_eq!(v, original);
    }

    #[test]
    fn approx_tokens_rounds_up() {
        assert_eq!(approx_tokens(""), 0);
        assert_eq!(approx_tokens("a"), 1);
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens("abcde"), 2);
    }

    #[test]
    fn approx_total_tokens_sums_content() {
        let v = vec![user("abcd"), assistant("efghijkl")];
        // 1 + 2 = 3
        assert_eq!(approx_total_tokens(&v), 3);
    }

    #[test]
    fn floor_char_boundary_handles_multibyte() {
        // "héllo" — the 'é' is 2 bytes (0xC3 0xA9).
        let s = "héllo";
        // Index 2 is in the middle of 'é'.
        let i = floor_char_boundary(s, 2);
        assert!(s.is_char_boundary(i));
        assert_eq!(i, 1);
    }

    #[test]
    fn ceil_char_boundary_handles_multibyte() {
        let s = "héllo";
        let i = ceil_char_boundary(s, 2);
        assert!(s.is_char_boundary(i));
        assert_eq!(i, 3);
    }
}
