//! Topic watcher: head/tail window compression for heart-rate
//! history.
//!
//! Mirrors the production `CompressionPolicy` in
//! `magent-core::conversation` — keep the first `head_count`
//! messages, the last `tail_count` messages, drop the middle.
//! Tool-result messages that exceed `tool_max_chars` are truncated
//! in place with a marker so the LLM still sees the bytes count.
//!
//! Run with: `cargo run -p magent-tools --bin topic-watcher`

use std::collections::VecDeque;

fn main() {
    println!("=== Topic Watcher (head/tail compression) ===\n");

    test_keep_all_when_under_limit();
    test_drop_middle_keep_head_and_tail();
    test_truncate_long_tool_results();
    test_stats_count_dropped_messages();
    test_window_is_deterministic();

    println!("\n=== All watcher tests passed ===");
}

/// A single conversation message — minimised copy of
/// `magent-core::conversation::Message`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Message {
    role: Role,
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    /// Wire name used in the conversation log; kept around even
    /// though the head/tail compressor only uses `Role::Tool` for
    /// its truncation branch — the dispatcher in
    /// `magent-core::conversation` relies on it.
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// Snapshot of compression counters — mirrors
/// `magent_core::conversation::CompressionStats`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Stats {
    kept: usize,
    dropped: usize,
    tool_results_truncated: usize,
    bytes_saved: usize,
}

/// Compression policy — mirrors
/// `magent_core::conversation::CompressionPolicy`.
#[derive(Debug, Clone)]
struct Policy {
    /// Maximum messages kept total.
    max_messages: usize,
    /// Truncate tool-result messages longer than this.
    tool_max_chars: usize,
}

impl Policy {
    fn disabled() -> Self {
        Self {
            max_messages: 0,
            tool_max_chars: 0,
        }
    }
    fn new(max_messages: usize, tool_max_chars: usize) -> Self {
        Self {
            max_messages,
            tool_max_chars,
        }
    }
}

/// Marker inserted at the truncation point so the reader knows
/// the body was clipped.
const TRUNCATION_MARKER: &str = "\n[… truncated; see full record]";

/// Apply the policy in place. Returns counters describing what
/// was dropped / truncated. The function name mirrors the
/// production API.
fn compress_messages(messages: &mut VecDeque<Message>, policy: &Policy) -> Stats {
    let original_len = messages.len();
    let mut stats = Stats::default();

    // Step 1: truncate over-long tool messages.
    if policy.tool_max_chars > 0 {
        for m in messages.iter_mut() {
            if m.role == Role::Tool && m.body.len() > policy.tool_max_chars {
                let original = m.body.len();
                let keep = policy.tool_max_chars;
                let mut new_body = m.body[..keep].to_string();
                new_body.push_str(TRUNCATION_MARKER);
                m.body = new_body;
                stats.tool_results_truncated += 1;
                stats.bytes_saved += original - m.body.len();
            }
        }
    }

    // Step 2: head/tail slicing.
    if policy.max_messages > 0 && messages.len() > policy.max_messages {
        // Keep the first `head` and the last `tail` messages.
        // We use `head = max_messages / 2` and
        // `tail = max_messages - head` to match the production
        // defaults.
        let head = policy.max_messages / 2;
        let tail = policy.max_messages - head;
        let total = messages.len();
        let split = total - tail;
        let head: Vec<Message> = messages.drain(..head).collect();
        let tail: Vec<Message> = messages.drain(split - head.len()..).collect();
        messages.clear();
        messages.extend(head);
        messages.extend(tail);
    }

    stats.kept = messages.len();
    stats.dropped = original_len.saturating_sub(stats.kept);
    stats
}

fn synth_messages(n: usize) -> VecDeque<Message> {
    let mut out = VecDeque::new();
    for i in 0..n {
        let role = match i % 4 {
            0 => Role::System,
            1 => Role::User,
            2 => Role::Assistant,
            _ => Role::Tool,
        };
        out.push_back(Message {
            role,
            body: format!("message-{}", i),
        });
    }
    out
}

fn test_keep_all_when_under_limit() {
    println!("Test: window under max_messages stays intact");

    let mut msgs = synth_messages(5);
    let stats = compress_messages(&mut msgs, &Policy::new(10, 0));
    assert_eq!(stats.kept, 5);
    assert_eq!(stats.dropped, 0);
    assert_eq!(msgs.len(), 5);

    println!("  ✅ 5 msgs / cap 10 → 5 kept, 0 dropped");
}

fn test_drop_middle_keep_head_and_tail() {
    println!("Test: head/tail window drops the middle");

    let mut msgs = synth_messages(10);
    let stats = compress_messages(&mut msgs, &Policy::new(4, 0));
    assert_eq!(stats.kept, 4);
    assert_eq!(stats.dropped, 6);

    // First two should be the head, last two should be the tail.
    assert_eq!(msgs[0].body, "message-0");
    assert_eq!(msgs[1].body, "message-1");
    assert_eq!(msgs[2].body, "message-8");
    assert_eq!(msgs[3].body, "message-9");

    println!("  ✅ 10 msgs / cap 4 → 4 kept (msg-0,1,8,9), 6 dropped");
}

fn test_truncate_long_tool_results() {
    println!("Test: tool messages longer than tool_max_chars get truncated");

    let mut msgs = VecDeque::new();
    msgs.push_back(Message {
        role: Role::Tool,
        body: "x".repeat(500),
    });
    msgs.push_back(Message {
        role: Role::User,
        body: "short".into(),
    });

    let stats = compress_messages(&mut msgs, &Policy::new(0, 100));
    assert_eq!(stats.tool_results_truncated, 1);
    assert!(stats.bytes_saved > 0);
    assert!(msgs[0].body.len() < 500);
    assert!(msgs[0].body.contains("truncated"));
    // User message untouched.
    assert_eq!(msgs[1].body, "short");

    println!(
        "  ✅ 500-char tool result truncated to {} chars (saved {} bytes)",
        msgs[0].body.len(),
        stats.bytes_saved
    );
}

fn test_stats_count_dropped_messages() {
    println!("Test: stats reflect both slice and truncation");

    let mut msgs = VecDeque::new();
    msgs.push_back(Message {
        role: Role::Tool,
        body: "y".repeat(200),
    });
    for i in 0..6 {
        msgs.push_back(Message {
            role: Role::User,
            body: format!("u{}", i),
        });
    }

    let stats = compress_messages(&mut msgs, &Policy::new(4, 50));
    assert_eq!(stats.tool_results_truncated, 1);
    assert_eq!(stats.dropped, 7 - stats.kept);
    assert!(stats.kept <= 4);

    println!(
        "  ✅ kept={} dropped={} truncated={} saved={}",
        stats.kept, stats.dropped, stats.tool_results_truncated, stats.bytes_saved
    );
}

fn test_window_is_deterministic() {
    println!("Test: same input → same output across calls");

    let input = synth_messages(20);
    let policy = Policy::new(6, 30);

    let mut a = input.clone();
    let mut b = input.clone();
    let sa = compress_messages(&mut a, &policy);
    let sb = compress_messages(&mut b, &policy);

    assert_eq!(sa, sb);
    assert_eq!(a, b);

    println!("  ✅ deterministic (same input → same window)");
}

// Make sure the disabled policy is a no-op.
#[allow(dead_code)]
fn _disabled_policy_is_noop() {
    let mut msgs = synth_messages(20);
    let len_before = msgs.len();
    let _stats = compress_messages(&mut msgs, &Policy::disabled());
    assert_eq!(msgs.len(), len_before);
}
