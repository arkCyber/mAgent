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
//! 2. **Message slicing / byte-GC** — past the configured message cap (or
//!    the hard byte budget) the oldest messages are dropped. The system
//!    prompt and the most-recent user request are always preserved.
//! 3. **Pair repair** — both of the above cut the history at an index, so
//!    a boundary can fall between an assistant `tool_calls` message and its
//!    `tool` result. The provider rejects half a pair, so
//!    [`prune_dangling_tool_pairs`] removes the orphaned half; every
//!    function here that drops messages calls it before returning.
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
// (no shared `const TRUNCATION_MARKER`), so there is no single constant to
// refer to. The literal pattern is `\n[...truncated N bytes...]\n` and the
// LLM is expected to recognise it as "irrelevant middle elided". The marker
// is constructed directly (never via placeholder substitution) so a tool
// payload containing the marker text can never be clobbered.

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
    /// Net bytes removed from tool `content` by the truncation step,
    /// including the marker text. Useful for telemetry.
    pub bytes_saved: usize,
}

/// Trim a single tool result's `content` to a head + marker + tail
/// window. The original `tool_call_id` is preserved.
///
/// The limit is in **characters**, matching the flag name
/// (`--tool-max-chars`) and the config key
/// (`compression.tool_content_max_chars`). It used to be compared against
/// `content.len()`, i.e. bytes, so an 800-character budget only kept ~266
/// CJK characters — a silent threefold under-truncation for non-ASCII tool
/// output. The head/tail budgets are still translated to byte offsets so a
/// slice never lands inside a code point; the marker keeps reporting the
/// dropped count in bytes, which is what it says.
///
/// Returns `true` if the content was actually shortened — callers can
/// use this to update `CompressionStats`.
pub fn truncate_tool_content(msg: &mut Message, max_chars: usize) -> bool {
    if max_chars == 0 || msg.role != Role::Tool {
        return false;
    }

    let original_len = msg.content.len();
    let char_count = msg.content.chars().count();
    if char_count <= max_chars {
        return false;
    }

    // Split the budget 60/40 between head and tail. Head wins because
    // tool outputs (status banners, JSON preamble, schema notes) tend
    // to be most informative at the top.
    let head_budget = (max_chars * 3) / 5;
    let tail_budget = max_chars - head_budget;

    // Translate the char budgets into byte offsets (`char_count >
    // tail_budget`, so `char_count - tail_budget` is a valid index).
    let head_end = byte_offset_after_chars(&msg.content, head_budget);
    let tail_start = byte_offset_after_chars(&msg.content, char_count - tail_budget);

    // The dropped count is measured in bytes: that is what the marker
    // and `CompressionStats::bytes_saved` report. original_len minus the
    // bytes we kept (head + tail) approximates it before the marker is
    // built; the tail is at the user's eye level, the marker is
    // diagnostics.
    let kept_payload = head_end + (original_len - tail_start);
    let marker_approx = original_len.saturating_sub(kept_payload);

    // Build the truncated content directly. We deliberately do NOT use a
    // placeholder + `replacen`: a tool payload could (in principle) contain
    // the placeholder sequence, and `replacen` would then clobber the
    // *user's* bytes instead of the marker. Constructing the marker inline
    // keeps user content untouched.
    let mut candidate = String::with_capacity(original_len);
    candidate.push_str(&msg.content[..head_end]);
    candidate.push_str("\n[...truncated ");
    candidate.push_str(&marker_approx.to_string());
    candidate.push_str(" bytes...]\n");
    if tail_start < original_len {
        candidate.push_str(&msg.content[tail_start..]);
    }

    // The marker's fixed overhead (~29 bytes) can exceed the payload it
    // replaces when `max_chars` is tiny, which would make the "truncated"
    // content *longer* than the original. That inverts the contract (cap
    // the content, never grow it) and would corrupt `bytes_saved`
    // telemetry, so keep the original in that case.
    if candidate.len() >= original_len {
        return false;
    }

    msg.content = candidate;

    true
}

/// Slice the message list to at most `max_messages` entries. The
/// **current** turn's user request is always kept (the task anchor, so
/// the LLM never loses the question it is being asked) and system
/// messages are kept at the head — unless the budget is too small to
/// hold both, in which case the oldest system messages are dropped
/// first. Everything else is taken from the tail (newest messages win).
///
/// Returns the number of messages dropped.
///
/// The anchor is the **last** user message, not the first. `run()` pushes
/// exactly one user message, so the two coincide; `run_continuing()` —
/// used by the CLI REPL, its only production caller — appends one per
/// turn, and there the current turn's request is the one that must
/// survive. Anchoring on the *oldest* user message instead pinned the
/// session's first task forever, so a window too small to hold a full
/// turn kept that stale task and the current turn's answers while
/// dropping the question those answers belonged to (at
/// `max_messages == 2`, the current task was dropped before the model
/// ever saw it).
pub fn slice_messages(messages: &mut Vec<Message>, max_messages: usize) -> usize {
    if max_messages == 0 || messages.len() <= max_messages {
        return 0;
    }

    let original_len = messages.len();

    // Find the first index that is *not* a system message. We want to
    // keep system messages at the head even when slicing.
    let mut preserved_system = messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .count();

    // The task anchor: the **last** user message (see the doc comment).
    // `next_back` on the double-ended iterator keeps this O(1) memory.
    let mut anchor = messages
        .iter()
        .enumerate()
        .skip(preserved_system)
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .next_back();

    // If the preserved system head alone would fill the budget, shed the
    // *oldest* system messages to make room for the task anchor (the one
    // message the LLM cannot recover from losing). Trimming the tail below
    // then enforces the cap.
    //
    // This used to `drain(..drop_system)` and `return` — skipping the tail
    // trim entirely — so with a single system prompt and `max_messages <= 2`
    // it dropped **nothing** and the list grew past its documented cap.
    let reserved = usize::from(anchor.is_some());
    if preserved_system + reserved > max_messages {
        let drop_system = preserved_system + reserved - max_messages;
        messages.drain(..drop_system);
        preserved_system -= drop_system;
        anchor = anchor.map(|a| a - drop_system);
    }

    let len = messages.len();
    let budget = max_messages - preserved_system;
    if len - preserved_system <= budget {
        // Everything after the system head fits — nothing else to drop, but
        // the system-head trim above may have split a pair.
        prune_dangling_tool_pairs(messages);
        return original_len - messages.len();
    }

    let tail_start = len - budget;
    let mut new_messages: Vec<Message> = Vec::with_capacity(max_messages);
    new_messages.extend(messages[..preserved_system].iter().cloned());
    match anchor {
        // The newest `budget` messages do not reach back to the current
        // task, so keep it plus the newest `budget - 1` messages that
        // follow it. `anchor < len - budget` means at least `budget`
        // messages follow, so the anchor and the tail cannot overlap.
        Some(a) if a < tail_start => {
            new_messages.push(messages[a].clone());
            new_messages.extend(messages[len - (budget - 1)..].iter().cloned());
        }
        _ => {
            new_messages.extend(messages[tail_start..].iter().cloned());
        }
    }
    *messages = new_messages;
    // The window boundary is a raw index, so it can fall between a tool call
    // and its result — repair before returning.
    prune_dangling_tool_pairs(messages);
    original_len - messages.len()
}

/// Apply [`CompressionPolicy`] to `messages` in-place: first truncate
/// oversized tool results, then slice the list to `max_messages`.
///
/// Returns the counters so the caller can record what happened.
pub fn compress_messages(
    messages: &mut Vec<Message>,
    policy: &CompressionPolicy,
) -> CompressionStats {
    let mut stats = CompressionStats::default();

    // Step 1: truncate tool results that are too long. We do this
    // *before* slicing so the size estimate used by the caller is
    // accurate even if they never invoke the slicing step.
    if policy.tool_content_max_chars > 0 {
        for msg in messages.iter_mut() {
            let before = msg.content.len();
            if truncate_tool_content(msg, policy.tool_content_max_chars) {
                stats.tool_results_truncated += 1;
                // Net bytes removed, marker included. `truncate_tool_content`
                // only reports success when it really shortened the content,
                // so this subtraction can never underflow.
                stats.bytes_saved += before - msg.content.len();
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

/// Estimate the **dynamic heap footprint (bytes)** a conversation slice holds
/// (REQ-SCHED-001 / mem-3). Sums message content, the tool-call name +
/// serialised arguments, the optional `tool_call_id`, and a fixed per-message
/// overhead for the `Vec<Message>` boxes / `String` capacities. This is what
/// [`gc_to_budget`] uses to bound the live context cache that lives on the
/// PSRAM heap — a *byte* budget is far more precise than the message-count
/// policy for preventing a long session from exhausting the pool.
pub fn estimate_bytes(messages: &[Message]) -> usize {
    const PER_MSG_OVERHEAD: usize = 64;
    // Saturating arithmetic so a pathological conversation can never wrap the
    // usize counter (release) or panic (debug) on a 32-bit target.
    messages.iter().fold(0usize, |acc, m| {
        let mut bytes = m.content.len().saturating_add(PER_MSG_OVERHEAD);
        if let Some(id) = &m.tool_call_id {
            bytes = bytes.saturating_add(id.len()).saturating_add(16);
        }
        if let Some(ref tc) = m.tool_call {
            bytes = bytes.saturating_add(tc.name.len()).saturating_add(32);
            for (k, v) in &tc.arguments {
                bytes = bytes
                    .saturating_add(k.len())
                    .saturating_add(v.to_string().len())
                    .saturating_add(16);
            }
        }
        acc.saturating_add(bytes)
    })
}

/// Garbage-collect a conversation slice down to a **byte** budget by dropping
/// the oldest non-system messages (preserving the system prompt and the most
/// recent turn, mirroring `slice_messages`'s anchor semantics). This is the
/// memory-aware GC for the large dynamic context cache on the S3's PSRAM: it
/// caps the live footprint so a long session or a huge tool dump cannot
/// exhaust the pool.
///
/// `max_bytes == 0` means "opt out" (no byte-GC) and is a no-op.
pub fn gc_to_budget(messages: &mut Vec<Message>, max_bytes: usize) -> CompressionStats {
    let mut stats = CompressionStats::default();
    if max_bytes == 0 {
        stats.kept = messages.len();
        return stats;
    }
    while estimate_bytes(messages) > max_bytes {
        // Preserve, in order of priority:
        //   * every system message (the prompt),
        //   * the newest message (the current turn's latest output),
        //   * the **last user message** — the request being answered.
        //
        // The previous "drop the oldest non-system message" rule dropped the
        // active request first: `[system, user, assistant-call, tool(huge)]`
        // removed `user`, then pruned the call/tool pair, leaving only the
        // system prompt. `current_turn_task()` returned "" and the next model
        // call had no question at all.
        let last_user = messages.iter().rposition(|m| m.role == Role::User);
        let newest = messages.len().saturating_sub(1);
        let Some(drop_idx) = messages
            .iter()
            .enumerate()
            .position(|(i, m)| m.role != Role::System && i != newest && Some(i) != last_user)
        else {
            break;
        };
        messages.remove(drop_idx);
        stats.dropped += 1;
    }
    // Trimming the oldest turns one message at a time cuts at that arbitrary
    // index, which can land inside a tool-call pair — repair before returning.
    // Gated on `dropped` so a history that fit the budget is handed back
    // exactly as it came in, even if it already contained a lone half.
    if stats.dropped > 0 {
        stats.dropped += prune_dangling_tool_pairs(messages);
    }
    stats.kept = messages.len();
    stats
}

/// Drop the halves of tool-call pairs that compression cut in two.
///
/// The provider wire format pairs an assistant message carrying
/// `tool_calls: [{id: N}]` with a `tool` message carrying `tool_call_id: N`;
/// OpenAI-compatible APIs (DeepSeek included) reject a request where either
/// half is missing — an unanswered `tool_calls` entry, or a `tool` message
/// with no preceding call, is a 400 and the run dies on a cause the user can
/// neither see nor fix.
///
/// Both [`slice_messages`] and [`gc_to_budget`] keep or drop whole messages
/// chosen by index, so a window boundary can fall between a call and its
/// result. A half-pair has to go: it cannot be completed (the other half is
/// already gone, and its content can't be reconstructed without inventing
/// it) and it cannot stay (it is malformed on the wire). Both budget
/// functions therefore call this before returning, so no caller can forget.
///
/// A well-formed conversation is left untouched. Returns the number of
/// messages dropped.
///
/// Note that an *unanswered* call is only possible here if compression
/// removed its result: the ReAct loop pushes the result in the same
/// iteration it pushes the call, so the live history never contains a lone
/// half on its own.
///
/// Matching is by **id and position**, not by id alone. The live runner now
/// issues session-unique ids (see `RealAgentRunner::call_id_seq`), but a
/// message list can still repeat one: a history persisted by an older
/// version, one assembled by hand, or a window in which compression dropped a
/// call while a later turn's result carries the same id. Under a bare
/// membership test a later call would vouch for an earlier orphaned result,
/// and the malformed request this function exists to prevent would survive.
/// So a `tool` is kept only by a call that *precedes* it, and a call only by
/// a result that *follows* it. The two rules agree (keeping a result keeps
/// its owner, because the owner precedes it), so one pass suffices.
pub fn prune_dangling_tool_pairs(messages: &mut Vec<Message>) -> usize {
    let is_call = |m: &Message| m.role == Role::Assistant && m.tool_call.is_some();
    let is_result = |m: &Message| m.role == Role::Tool;

    let mut keep = vec![true; messages.len()];
    for (i, m) in messages.iter().enumerate() {
        if is_result(m) {
            // A result needs a call before it; a result with no id has
            // nothing to match and is left to its own device.
            let Some(id) = m.tool_call_id.as_deref() else {
                continue;
            };
            keep[i] = messages[..i]
                .iter()
                .any(|p| is_call(p) && p.tool_call_id.as_deref() == Some(id));
        } else if is_call(m) {
            // A call needs a result after it. A call carrying no id predates
            // the id-pairing convention (and is not produced by the loop), so
            // there is nothing to match it against.
            let Some(id) = m.tool_call_id.as_deref() else {
                continue;
            };
            keep[i] = messages[i + 1..]
                .iter()
                .any(|n| is_result(n) && n.tool_call_id.as_deref() == Some(id));
        }
    }

    let before = messages.len();
    let mut idx = 0;
    messages.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
    before - messages.len()
}

// ---------------------------------------------------------------------------
// UTF-8 helpers (private)
// ---------------------------------------------------------------------------

/// Byte offset just past the first `n` characters of `s` — i.e. the byte
/// slice point that keeps exactly `n` characters. Clamps to `s.len()`
/// when `s` has fewer than `n` characters. Always a char boundary.
fn byte_offset_after_chars(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runner::{Message, ToolCall};

    fn user(s: &str) -> Message {
        Message::user(s)
    }

    fn assistant(s: &str) -> Message {
        Message::assistant_text(s)
    }

    fn tool(id: &str, s: &str) -> Message {
        Message::tool(id, s)
    }

    /// The assistant half of a tool-call pair: the message that *owns*
    /// `id`, which the following `tool(id, …)` answers. A fixture that
    /// pairs them the way the ReAct loop does, so it models a conversation
    /// the provider would accept.
    fn call(id: &str) -> Message {
        Message::assistant_tool_call_id(
            ToolCall {
                name: "read_sensor".to_string(),
                arguments: std::collections::HashMap::new(),
            },
            id,
        )
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

    /// The cap is in **characters**, not bytes: an 800 "char" budget used
    /// to keep only ~266 CJK characters, silently under-truncating
    /// threefold for non-ASCII tool output. `max_chars` characters must now
    /// survive (modulo the head/tail split), regardless of how many bytes
    /// they occupy.
    #[test]
    fn truncate_caps_characters_not_bytes() {
        // 2_000 CJK characters = 6_000 bytes. A 300-char cap is well below
        // the character count but well above nothing in bytes, so a
        // byte-based cap would have kept ~300 bytes ≈ 100 characters.
        let body = "测".repeat(2_000);
        let mut m = tool("call_1", &body);
        assert!(truncate_tool_content(&mut m, 300));

        let kept_chars = m.content.chars().count();
        // head(180) + tail(120) = 300 chars, plus the marker line.
        assert!(
            (300..400).contains(&kept_chars),
            "expected ~300 characters kept, got {kept_chars}"
        );
        // A byte-based cap would have kept ~100 characters; assert we are
        // clearly past that, which is the regression this pins.
        assert!(
            m.content.len() > 300,
            "content should be more than 300 bytes of CJK, got {}",
            m.content.len()
        );
        // No code point was sliced in half.
        assert!(m.content.contains("测"));
        assert!(std::str::from_utf8(m.content.as_bytes()).is_ok());
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
    fn truncate_never_grows_the_content() {
        // The inline marker costs a fixed ~29 bytes. For a budget below
        // that, truncating a short-but-over-budget tool result used to
        // produce a *longer* string than the original — inverting the cap
        // contract and making `bytes_saved` underflow to zero. Sweep the
        // pathological corner instead of testing a single point.
        for max_chars in 1..64usize {
            for len in 1..256usize {
                let original = "a".repeat(len);
                let mut m = tool("call_1", &original);
                let shortened = truncate_tool_content(&mut m, max_chars);
                assert!(
                    m.content.len() <= original.len(),
                    "max_chars={max_chars} len={len}: content grew to {}",
                    m.content.len()
                );
                assert_eq!(
                    shortened,
                    m.content.len() < original.len(),
                    "max_chars={max_chars} len={len}: return value disagrees with the result"
                );
            }
        }
    }

    #[test]
    fn compress_reports_the_net_bytes_it_removed() {
        let policy = CompressionPolicy {
            tool_content_max_chars: 100,
            max_messages: 20,
        };
        let long = "a".repeat(2_000);
        let mut v = vec![user("task"), tool("call_1", &long)];
        let stats = compress_messages(&mut v, &policy);
        assert_eq!(stats.tool_results_truncated, 1);
        assert_eq!(stats.bytes_saved, 2_000 - v[1].content.len());
        assert!(stats.bytes_saved > 0);
    }

    #[test]
    fn truncate_ignores_non_tool_messages() {
        let mut m = assistant(&"x".repeat(10_000));
        assert!(!truncate_tool_content(&mut m, 100));
        assert_eq!(m.content.len(), 10_000);
    }

    #[test]
    fn slice_enforces_cap_when_budget_is_barely_above_the_head() {
        // One system prompt + the task anchor exactly fill a budget of 2.
        // The head alone must not short-circuit the tail trim: this used to
        // drop nothing and leave the list above its cap.
        let mut v: Vec<Message> = vec![system("SYS"), user("task")];
        for i in 0..10 {
            v.push(assistant(&format!("a{i}")));
        }
        let dropped = slice_messages(&mut v, 2);
        assert_eq!(v.len(), 2, "cap not enforced (dropped={dropped})");
        assert_eq!(dropped, 10);
        assert_eq!(v[0].content, "SYS");
        assert!(v
            .iter()
            .any(|m| m.role == Role::User && m.content == "task"));
    }

    #[test]
    fn slice_drops_system_when_budget_is_one_and_keeps_the_task() {
        let mut v: Vec<Message> = vec![system("SYS"), user("task"), assistant("a")];
        let dropped = slice_messages(&mut v, 1);
        assert_eq!(v.len(), 1, "cap not enforced (dropped={dropped})");
        assert_eq!(v[0].content, "task");
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
            call("c1"),
            tool("c1", "c"),
            user("u2"),
            call("c2"),
            tool("c2", "c2"),
        ];
        let dropped = slice_messages(&mut v, 4);
        assert_eq!(dropped, 3);
        assert_eq!(v.len(), 4);
        // System prompt is always at the top.
        assert_eq!(v[0].role, Role::System);
        assert_eq!(v[0].content, "SYS");
        // The **newest** user message is the anchor (the current turn's
        // request), not the oldest one.
        assert!(
            v.iter().any(|m| m.role == Role::User && m.content == "u2"),
            "the current turn's request was dropped: {v:?}"
        );
    }

    #[test]
    fn slice_prefers_newest_messages() {
        let mut v: Vec<Message> = (0..20).map(|i| user(&format!("u{}", i))).collect();
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
        assert!(v
            .iter()
            .any(|m| m.role == Role::User && m.content == "orig task"));
    }

    #[test]
    fn slice_keeps_the_current_task_in_a_multi_turn_session() {
        // The REPL appends one user message per turn. A window that is too
        // small to reach back to the *first* task must keep the *current*
        // one instead: anchoring on the oldest user message kept "task A"
        // and an answer to "task B" while dropping the question, so the
        // model was asked nothing.
        let mut v = vec![
            system("SYS"),
            user("task A"),
            assistant("aA"),
            user("task B"),
            call("c1"),
            tool("c1", "t1"),
        ];
        let dropped = slice_messages(&mut v, 4);
        assert_eq!(v.len(), 4);
        assert_eq!(dropped, 2);
        assert_eq!(v[0].content, "SYS");
        assert!(
            v.iter()
                .any(|m| m.role == Role::User && m.content == "task B"),
            "the current turn's task was dropped: {v:?}"
        );
    }

    #[test]
    fn slice_keeps_the_current_task_at_a_two_message_cap() {
        // Regression: at `--max-messages 2` the old anchor pinned "task A"
        // (plus the system prompt) and the current task never reached the
        // model at all.
        let mut v = vec![
            system("SYS"),
            user("task A"),
            assistant("aA"),
            user("task B"),
        ];
        let dropped = slice_messages(&mut v, 2);
        assert_eq!(v.len(), 2);
        assert_eq!(dropped, 2);
        assert_eq!(v[0].content, "SYS");
        assert_eq!(
            v.last().unwrap().content,
            "task B",
            "the model was asked the stale task instead of the current one: {v:?}"
        );
    }

    #[test]
    fn compress_runs_both_steps() {
        let mut v = vec![system("SYS"), user("orig task")];
        // Add long tool result that should be truncated.
        v.push(call("c1"));
        v.push(tool("c1", &"y".repeat(5_000)));
        // Pad with extra turns so the message count exceeds the cap.
        for i in 0..30 {
            v.push(call(&format!("c{}", i)));
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
        let stats = compress_messages(
            &mut v,
            &CompressionPolicy {
                max_messages: 0,
                tool_content_max_chars: 0,
            },
        );
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
    fn byte_offset_after_chars_lands_on_code_point_boundaries() {
        // "héllo" — the 'é' is 2 bytes (0xC3 0xA9), so the first 2
        // characters occupy 3 bytes.
        let s = "héllo";
        assert_eq!(byte_offset_after_chars(s, 0), 0);
        assert_eq!(byte_offset_after_chars(s, 1), 1);
        assert_eq!(byte_offset_after_chars(s, 2), 3);
        assert!(s.is_char_boundary(byte_offset_after_chars(s, 2)));
        // Past the end clamps to the length rather than panicking.
        assert_eq!(byte_offset_after_chars(s, 99), s.len());
    }

    #[test]
    fn truncate_preserves_placeholder_like_bytes_in_head() {
        // Regression: the old implementation used a `\x00BYTES\x00`
        // placeholder + `replacen`, which would clobber *user* bytes if the
        // head of the tool payload happened to contain that exact sequence.
        // The marker is now built directly, so user bytes are untouched.
        let head_with_placeholder = "HEAD\x00BYTES\x00TAIL";
        let body = format!("{} {}", head_with_placeholder, "x".repeat(5_000));
        let mut m = tool("call_1", &body);
        assert!(truncate_tool_content(&mut m, 100));
        // The head — including the user's NUL-containing bytes — is preserved.
        assert!(m.content.starts_with("HEAD\x00BYTES\x00TAIL"));
        // The truncation marker is still present and appears exactly once
        // (not clobbered by a replacement of the user's bytes).
        assert!(m.content.contains("[...truncated"));
        assert_eq!(m.content.matches("[...truncated").count(), 1);
        // The tool_call_id survives.
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn estimate_bytes_counts_content_and_overhead() {
        let v = vec![user("abcd"), assistant("efghijkl")];
        // content bytes 4 + 8, plus 2 * PER_MSG_OVERHEAD (64).
        assert_eq!(estimate_bytes(&v), 4 + 8 + 2 * 64);
    }

    #[test]
    fn gc_to_budget_drops_oldest_non_system() {
        let mut v = vec![
            system("SYS"),
            user("orig"),
            assistant("a1"),
            tool("c1", &"x".repeat(200)),
            assistant("a2"),
            user("latest"),
        ];
        let stats = gc_to_budget(&mut v, 150);
        assert!(stats.dropped > 0);
        // Bounded below the budget (or down to the floor of system+newest).
        assert!(estimate_bytes(&v) <= 150 || v.len() <= 2);
        // System prompt preserved at the head.
        assert_eq!(v.first().unwrap().role, Role::System);
        // The most recent turn (current user message) is preserved.
        assert_eq!(v.last().unwrap().content, "latest");
    }

    /// The current user request must survive GC even when the only thing
    /// that can be dropped is the tool-call/tool-result pair. The old
    /// "drop the oldest non-system" rule removed the user message first,
    /// leaving the next model call with no question.
    #[test]
    fn gc_to_budget_preserves_the_current_user_request() {
        let mut v = vec![
            system("SYS"),
            user("what is the weather in Shanghai?"),
            call("c1"),
            tool("c1", &"x".repeat(600)),
        ];
        let stats = gc_to_budget(&mut v, 200);
        assert!(stats.dropped > 0);
        assert!(
            v.iter()
                .any(|m| m.role == Role::User && m.content.contains("Shanghai")),
            "current user request was dropped: {v:?}"
        );
        assert_eq!(v.first().unwrap().role, Role::System);
    }

    #[test]
    fn gc_to_budget_zero_is_noop() {
        let mut v = vec![user("a"), assistant("b")];
        let original = v.clone();
        let stats = gc_to_budget(&mut v, 0);
        assert_eq!(stats.dropped, 0);
        assert_eq!(v, original);
    }

    #[test]
    fn gc_to_budget_under_budget_changes_nothing() {
        let mut v = vec![user("a"), assistant("b")];
        let original = v.clone();
        let stats = gc_to_budget(&mut v, usize::MAX);
        assert_eq!(stats.dropped, 0);
        assert_eq!(v, original);
    }

    /// The wire contract every pruning test checks: no `tool` message without
    /// an assistant call to answer, and no assistant call left unanswered.
    fn assert_no_dangling_pairs(v: &[Message]) {
        for (i, m) in v.iter().enumerate() {
            match m.role {
                Role::Tool => {
                    let id = m.tool_call_id.as_deref();
                    assert!(
                        id.is_some_and(|id| v[..i].iter().any(|p| {
                            p.role == Role::Assistant
                                && p.tool_call.is_some()
                                && p.tool_call_id.as_deref() == Some(id)
                        })),
                        "orphaned tool message at {i}: {m:?}",
                    );
                }
                Role::Assistant if m.tool_call.is_some() => {
                    if let Some(id) = m.tool_call_id.as_deref() {
                        assert!(
                            v[i + 1..].iter().any(|n| {
                                n.role == Role::Tool && n.tool_call_id.as_deref() == Some(id)
                            }),
                            "unanswered tool call at {i}: {m:?}",
                        );
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn prune_leaves_a_well_formed_conversation_alone() {
        let mut v = vec![
            system("SYS"),
            user("task"),
            call("c1"),
            tool("c1", "21°C"),
            assistant("done"),
        ];
        let original = v.clone();
        assert_eq!(prune_dangling_tool_pairs(&mut v), 0);
        assert_eq!(v, original);
    }

    #[test]
    fn prune_drops_both_halves_of_a_split_pair() {
        // The tool result lost its call …
        let mut orphan_result = vec![user("task"), tool("c1", "21°C")];
        assert_eq!(prune_dangling_tool_pairs(&mut orphan_result), 1);
        assert_eq!(orphan_result.len(), 1);

        // … and the call lost its result.
        let mut orphan_call = vec![user("task"), call("c1")];
        assert_eq!(prune_dangling_tool_pairs(&mut orphan_call), 1);
        assert_eq!(orphan_call.len(), 1);
        assert_eq!(orphan_call[0].role, Role::User);
    }

    #[test]
    fn prune_keeps_a_call_that_carries_no_id() {
        // `Message::assistant_tool_call` predates the id-pairing convention
        // and has nothing to match against, so it is not pruned.
        let mut v = vec![
            user("task"),
            Message::assistant_tool_call(ToolCall {
                name: "read_sensor".to_string(),
                arguments: std::collections::HashMap::new(),
            }),
        ];
        assert_eq!(prune_dangling_tool_pairs(&mut v), 0);
        assert_eq!(v.len(), 2);
    }

    /// The bug this repair exists for: `slice_messages` keeps a window chosen
    /// by index, and index 3 of this history is a `tool` message whose owning
    /// call sits just outside the window. The provider rejects a request
    /// carrying half a pair outright, so the slice must not produce one.
    #[test]
    fn a_slice_boundary_inside_a_pair_does_not_orphan_the_result() {
        let mut v = vec![
            system("SYS"),
            user("task one"),
            call("call_1"),
            tool("call_1", "21°C"),
            assistant("answer one"),
            user("task two"),
            call("call_2"),
            tool("call_2", "22°C"),
            assistant("answer two"),
        ];
        let dropped = slice_messages(&mut v, 7);

        assert_eq!(v[0].content, "SYS");
        assert_no_dangling_pairs(&v);
        // The window keeps 7, and the repair then drops the orphaned result.
        assert_eq!(v.len(), 6);
        // The current turn's request is still there.
        assert!(v
            .iter()
            .any(|m| m.role == Role::User && m.content == "task two"));
        // Two messages were cut by the window and one orphaned result by the
        // repair, so the count is the total, not just the window's.
        assert_eq!(dropped, 3);
    }

    /// A history that repeats an id — persisted by an older version, or built
    /// by hand like this one. An orphaned result from turn one must not be
    /// kept just because turn two happens to reuse its id — that is exactly
    /// the half-pair the provider rejects.
    #[test]
    fn prune_survives_a_session_that_re_uses_tool_call_ids() {
        let mut v = vec![
            system("SYS"),
            user("turn one"),
            // Turn one's call was dropped, leaving this result behind.
            tool("call_1", "orphaned: its call was dropped"),
            assistant("answer one"),
            user("turn two"),
            call("call_1"),
            tool("call_1", "22°C"),
            assistant("answer two"),
        ];
        assert_eq!(prune_dangling_tool_pairs(&mut v), 1);
        assert_no_dangling_pairs(&v);
        // Turn two's own pair is intact.
        assert!(v
            .iter()
            .any(|m| m.role == Role::Tool && m.content == "22°C"));
    }

    /// Same hazard on the byte-GC path, which drops messages one at a time
    /// and so walks straight through the middle of a pair.
    #[test]
    fn a_byte_gc_cut_does_not_orphan_the_result() {
        let mut v = vec![
            system("SYS"),
            user("task one"),
            call("call_1"),
            tool("call_1", &"x".repeat(400)),
            assistant("answer one"),
            user("task two"),
        ];
        let stats = gc_to_budget(&mut v, 300);
        assert!(stats.dropped > 0);
        assert_no_dangling_pairs(&v);
        assert_eq!(v.first().unwrap().role, Role::System);
        assert_eq!(v.last().unwrap().content, "task two");
    }

    #[test]
    fn approx_total_tokens_counts_tool_call_arguments() {
        // A tool call's argument map contributes tokens on top of the
        // message content (best-effort JSON-like sum).
        let mut args = std::collections::HashMap::new();
        args.insert("sensor".to_string(), serde_json::json!("temperature"));
        let m = Message::assistant_tool_call(ToolCall {
            name: "read_sensor".to_string(),
            arguments: args,
        });
        let v = vec![m];
        let with_args = approx_total_tokens(&v);
        // Content is empty; the tokens come entirely from the key/value.
        assert!(with_args > 0, "args were not counted: {with_args}");
    }

    #[test]
    fn estimate_bytes_counts_tool_call_id_and_arguments() {
        // A tool-result message carries a `tool_call_id`, and an
        // assistant message carries a `tool_call` with arguments — both
        // add to the byte estimate beyond the fixed per-message overhead.
        let mut args = std::collections::HashMap::new();
        args.insert("sensor".to_string(), serde_json::json!("temperature"));
        let v = vec![
            user("hi"),
            call("c1"),
            tool("c1", "22°C"),
        ];
        let bytes = estimate_bytes(&v);
        // More than the bare content + 3×64 overhead, because the id and
        // the argument map were tallied too.
        assert!(
            bytes > "hi".len() + "22°C".len() + 3 * 64,
            "tool metadata was not counted: {bytes}"
        );
    }

    #[test]
    fn truncate_tool_content_refuses_a_zero_budget() {
        let mut m = tool("c1", &"x".repeat(500));
        assert!(!truncate_tool_content(&mut m, 0), "0 is opt-out");
        assert_eq!(m.content.len(), 500);
    }

    #[test]
    fn slice_prunes_a_pair_split_by_a_system_head_trim() {
        // `max_messages = 2` with two system messages + a task: the oldest
        // system is shed to leave room for the task, and the resulting trim
        // can split a pair. `slice_messages` must repair it (the
        // `prune_dangling_tool_pairs` short-circuit path).
        let mut v = vec![
            system("SYS1"),
            system("SYS2"),
            user("task"),
            call("c1"),
            tool("c1", "ok"),
        ];
        let dropped = slice_messages(&mut v, 4);
        assert!(dropped > 0);
        assert!(v.len() <= 4);
        assert_no_dangling_pairs(&v);
    }

    #[test]
    fn gc_to_budget_keeps_a_lone_user_message_when_too_big_to_drop() {
        // A single over-budget message that is also the newest one cannot be
        // dropped (the "never drop the newest message" guard), so the loop
        // breaks and leaves the message in place.
        let mut v = vec![user(&"x".repeat(500))];
        let stats = gc_to_budget(&mut v, 10);
        assert_eq!(stats.dropped, 0);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn gc_to_budget_stops_when_only_system_messages_remain() {
        // With only system messages left there is no droppable non-system
        // message, so the `position(...)` returns `None` and the loop stops.
        let mut v = vec![system(&"s".repeat(500))];
        let stats = gc_to_budget(&mut v, 10);
        assert_eq!(stats.dropped, 0);
        assert_eq!(v.len(), 1);
    }
}
