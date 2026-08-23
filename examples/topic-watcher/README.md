# topic-watcher

> Topic watcher: head/tail window compression for conversation history

A standalone simulation of the `CompressionPolicy` in
`magent-core::conversation`:

- Keep the first `head_count` messages.
- Keep the last `tail_count` messages.
- Drop everything in the middle.
- Truncate tool-result messages longer than `tool_max_chars` with
  a marker so the LLM still sees the byte count.

## What it demonstrates

| Stage                | Behaviour                                          |
|----------------------|----------------------------------------------------|
| Slicing              | `head = max_messages / 2`, `tail = max_messages - head` |
| Tool truncation      | In-place, with `\n[… truncated; see full record]`  |
| Stats                | `kept`, `dropped`, `tool_results_truncated`, `bytes_saved` |
| Determinism          | Same input + same policy → same output             |

## Running

```bash
cd examples/topic-watcher
cargo run --release
```

Expected output:

```text
=== Topic Watcher (head/tail compression) ===

Test: window under max_messages stays intact
  ✅ 5 msgs / cap 10 → 5 kept, 0 dropped
Test: head/tail window drops the middle
  ✅ 10 msgs / cap 4 → 4 kept (msg-0,1,8,9), 6 dropped
Test: tool messages longer than tool_max_chars get truncated
  ✅ 500-char tool result truncated to 133 chars (saved 367 bytes)
Test: stats reflect both slice and truncation
  ✅ kept=4 dropped=3 truncated=1 saved=117
Test: same input → same output across calls
  ✅ deterministic across replays

=== All watcher tests passed ===
```

## Why this matters

The `CompressionPolicy` is what keeps the conversation history
within the LLM's context window. The compressor runs at the start
of every LLM chat request, so its accuracy directly affects
token spend and response quality. The test suite pins down four
invariants:

1. **No over-drop when under the cap** — operators set a cap of
   32 messages, and we don't want to drop a 10-message window.
2. **Head/tail preserved** — the system prompt (first message)
   and the last user/assistant exchange must survive
   compression.
3. **Tool truncation is byte-visible** — the LLM doesn't lose a
   tool result silently; it sees `[… truncated; see full record]`
   and can decide whether to ask for the full record.
4. **Stats are real** — `bytes_saved` should reflect actual
   truncation, not a placeholder.

## Files

```
topic-watcher/
├── Cargo.toml
├── README.md
└── src/
    └── main.rs
```
