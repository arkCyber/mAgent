# Conversation / Context Management

The mAgent core keeps the full conversation history for the lifetime of
a single `run()`, but every LLM call resubmits the entire history on
the wire. Without a cap, sessions grow quadratically with the number of
iterations and quickly exceed the model's context window (8k for most
local Ollama models, 32k for `deepseek-chat`).

This document describes the in-house, no-dependency compression machinery
that lives in `magent-core::conversation` and the CLI flags that tune
it.

## The compression pipeline

`magent-core::conversation::compress_messages()` runs **before every
LLM call** in `RealAgentRunner::think()` and applies two steps in order:

1. **Tool-result truncation** — any `Message` whose `role == Tool` and
   whose `content.len() > tool_content_max_chars` is shortened to a
   `head + marker + tail` window. The marker is
   `` `[...truncated N bytes...]` `` where `N` is the exact byte count
   removed. The `tool_call_id` is preserved so the LLM can still
   correlate the result with the original call.

2. **Message slicing** — once the message list exceeds
   `max_messages`, the runner drops the oldest messages and keeps:
   - every `System` message at the head (so the system prompt never
     gets lost — this is the v3 design, see below);
   - the first `User` message (the original task anchor);
   - the **most recent** `max_messages - preserved_head` messages.

The runner logs the compression counters when verbose mode is on:

```text
[Compress] kept=24 dropped=8 tools_truncated=2 bytes_saved≈1700
```

## v3: system prompt is persisted (not transient)

Up to and including v2, the system prompt was injected as a transient
first message on every LLM call (`insert(0, …); chat_with_messages(…);
remove(0);`). That kept `runner.messages()` small but had two
unpleasant consequences:

1. `runner.messages()` did not reflect what the LLM actually saw, so
   the compression pipeline (`compress_messages`) was operating on a
   partial view.
2. `approx_total_tokens()` undercounted by the size of the system
   prompt.

In v3 the system prompt is **persisted** as the first message of
`self.messages` and `runner.ensure_system_prompt(prompt)` reconciles
it on every `think()` call. The reconciliation is idempotent:

| Existing head of `self.messages`         | Action                           |
| ---------------------------------------- | -------------------------------- |
| System message with matching `prompt`    | no-op (already correct)          |
| System message with **different** content | replace in place (live edit)   |
| Anything else (user / assistant / tool)  | insert at index 0                |

The cost is one extra ~1-2 KB per `think()` (the system prompt is
already sent to the LLM anyway); the benefit is that the entire
pipeline — `runner.messages()`, `approx_total_tokens()`,
`compress_messages()` — sees the same view of the conversation that
the LLM does.

## When to use it

| Scenario                                           | Recommended policy                          |
| -------------------------------------------------- | ------------------------------------------- |
| Quick smoke test on a local 8k model               | defaults (32 messages / 800 chars/tool)    |
| Long health-coaching session, 13b+ model           | `max_messages=64` `tool_max_chars=2000`    |
| DeepSeek 32k context, want richer history          | `max_messages=200` `tool_max_chars=4000`    |
| Embedded target with no LLM (simulator only)       | `max_messages=0` `tool_max_chars=0`        |
| Tool dumps gigabytes (e.g. raw sensor logs)        | `tool_max_chars=200` only                   |

## Library API

```rust
use magent_core::conversation::{compress_messages, CompressionPolicy};

let policy = CompressionPolicy {
    max_messages: 32,
    tool_content_max_chars: 800,
};

let mut messages: Vec<Message> = /* build history */;
let stats = compress_messages(&mut messages, &policy);
println!("kept {} dropped {}", stats.kept, stats.dropped);
```

`CompressionPolicy::default()` returns the safe defaults (32 messages,
800 chars per tool result). `CompressionPolicy::disabled()` is a no-op
policy (both fields `0`).

`RunnerConfig` carries a `compression: CompressionPolicy` field. The
runner applies it transparently — you don't have to call
`compress_messages()` yourself.

```rust
let mut config = RunnerConfig::default();
config.compression.max_messages = 64;
config.compression.tool_content_max_chars = 2000;
let runner = RealAgentRunner::with_config(executor, config);
```

### Diagnostics

`RealAgentRunner` exposes two helpers for telemetry:

```rust
runner.approx_total_tokens(); // rough 4-char-per-token estimate
runner.compress_now();        // run the pipeline manually + return CompressionStats
```

`RunReport` (CLI) reports both numbers in the JSON envelope:

```json
{
  "iterations": 5,
  "tool_calls": 3,
  "provider": "deepseek",
  "using_ollama": false,
  "state": "Finished",
  "final_messages": 18,
  "approx_tokens": 412
}
```

## CLI flags

`magent run` exposes the policy through two flags:

```sh
magent run --max-messages 16 --tool-max-chars 400 "Summarise my week"
magent run --max-messages 0 --tool-max-chars 0 "lint task"  # disable entirely
magent run --max-messages 64 --tool-max-chars 2000 --provider deepseek "long task"
```

Defaults mirror `CompressionPolicy::default()` so the CLI is safe to run
without touching the flags at all.

| Flag                  | Default | Effect                                             |
| --------------------- | ------- | -------------------------------------------------- |
| `--max-messages N`    | `32`    | Max messages kept. `0` = no slicing.               |
| `--tool-max-chars N`  | `800`   | Max chars per tool result. `0` = no truncation.    |

Both flags appear in the `magent run --help` output under the
**CONTEXT MANAGEMENT** section.

### Persisting the compressed window across runs

The `--max-messages` / `--tool-max-chars` bounds above apply to
the **live** conversation only — they're lost the moment the
process exits. To carry the compressed window across runs, use
`magent run --save-summary <TOPIC>` (writes the post-run window
to disk) and `--load-summary <TOPIC>` (injects the stored window
as a system note before the next run). See
[`docs/SUMMARY_STORE.md`](SUMMARY_STORE.md) for the storage
layout, atomic-write semantics, and CLI reference.

## How the truncation works

For a 2,000-character tool result with a 100-character budget:

| Budget slice        | Value                                  |
| ------------------- | -------------------------------------- |
| Head                | 60% of `max_chars` (≈ 60 chars)        |
| Tail                | 40% of `max_chars` (≈ 40 chars)        |
| Marker              | `[...truncated 1862 bytes...]`         |
| Total               | ≈ 100 chars + marker length            |

UTF-8 safety: the boundaries are rounded down/up to the nearest
character boundary so we never slice through a multi-byte code point.
Tested with `"héllo"` etc.

## Trade-offs

- **Head + tail** is preferred over `head only` because tool outputs
  (e.g. CSV dumps, JSON envelopes) often put the actionable info at the
  end (a status banner at the top, the result rows at the bottom).
- **System prompt is preserved** across slicing. Dropping the system
  prompt would degrade every subsequent LLM call.
- **Original task is preserved**. Anchoring the slice to the first
  user message keeps the agent's goal in scope even after a long run.
- **No summarization step**. We deliberately avoid the LLM-summarize-
  old-messages strategy because:
  1. It costs an extra round-trip per `think()`.
  2. It depends on the LLM being reachable at compression time
     (which is exactly when we're trying to fail gracefully).
  3. Summaries are lossy in a way the user can't audit. The
     head/tail window is auditable.

If you want true LLM-driven summarization, do it as a post-run step
that processes the captured `RunReport.answer` + conversation log.
