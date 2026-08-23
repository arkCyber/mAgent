# Stored Run Summaries (`magent summary`)

`magent summary` is the CLI subcommand for **persisting the head/tail
window** of a run so the next run can pick up where the last one left
off. Each summary is a small JSON record you can `git diff`, audit, and
feed back into a future invocation with `magent run --load-summary`.

## Why

`magent run` already bounds the live conversation via
[`--max-messages`](docs/CONTEXT_MANAGEMENT.md) and
[`--tool-max-chars`](docs/CONTEXT_MANAGEMENT.md), but those bounds
are **lost the moment the process exits**. The next `magent run` starts
fresh and the model has no memory of what came before.

`magent summary` solves this by writing the compressed window to disk
under a user-chosen **topic**. The next run can then opt in with
`--load-summary <TOPIC>` and the previous window is injected as a
system note immediately after the live system prompt — so the model
sees "the prior conversation went like X, Y, Z" without paying for
the full history in tokens.

## Storage layout

By default summaries live under the user's XDG data directory:

| Platform | Resolved path                                              |
| -------- | ---------------------------------------------------------- |
| Linux    | `$XDG_DATA_HOME/magent/summaries/<topic>.json`             |
| macOS    | `$HOME/.local/share/magent/summaries/<topic>.json`         |
| Override | `$MAGENT_SUMMARIES_DIR`                                    |

`MAGENT_SUMMARIES_DIR` is the explicit override — useful for CI
runners, containers, and tests where `$XDG_DATA_HOME` may be unset.
A sibling directory `$DIR/.locks/` holds per-topic lock files used
for cross-process write coordination (see
[Concurrency model](#concurrency-model)).

## Subcommands

```text
magent summary save    <TOPIC> [--from <FILE>] [--overwrite] [--dir <DIR>]
magent summary show    <TOPIC>
magent summary list
magent summary delete  <TOPIC>
magent summary export  <TOPIC>
magent summary load    <TOPIC>
magent summary rollback <TOPIC> <INDEX>
```

Every sub-action prints either a pretty message (human mode) or a
JSON envelope (`--json` mode), mirroring the `magent run` /
`magent set-prompt` output.

### `save`

```sh
magent summary save health_coach_latest --from /tmp/just-finished.json
```

* Without `--from`, the record is read from **stdin** — useful for
  piping the output of another tool directly into the store.
* `--overwrite` replaces an existing record with the same name.
  **Default behaviour is to refuse**, so CI runs that retry don't
  silently overwrite the previous run's summary.
* `--dir <PATH>` overrides the default summaries directory for this
  single invocation (mostly for tests and CI).

`save` validates the record on the way in (topic name, metadata
fields, total size) and **refuses to write a record larger than
64 KiB** — see [Validation rules](#validation-rules).

### `show` / `list`

* `show <TOPIC>` — human-readable summary in default mode; the raw
  JSON record when `--json` is passed (matches `magent set-prompt show`).
* `list` — table of every stored topic (`TOPIC`, `UPDATED`, `KEPT`, `TAGS`).

### `delete` / `export` / `load`

* `delete <TOPIC>` — remove the file (idempotent: no error if it
  doesn't exist).
* `export <TOPIC>` — dump the raw JSON record to stdout (pipeable
  back into `save --from`).
* `load <TOPIC>` — dump **just the `head_tail_window` array** as
  JSON. Useful for debugging or feeding the window into a downstream
  tool without the metadata noise.

### `rollback <TOPIC> <INDEX>`

Promote `history[INDEX]` back into the active record's
`head_tail_window`, `llm_summary`, and `stats`. The previously-active
snapshot is pushed onto history (FIFO, capped at
[`HISTORY_MAX`](#validation-rules) = 5).

```sh
# Roll back to the second-most-recent snapshot.
magent summary rollback health_coach_latest 1

# Out-of-range → IndexOutOfRange { index: 99, len: 5 }
magent summary rollback health_coach_latest 99
```

## On-disk JSON shape

Every `<topic>.json` file is identical to what `show` would print:

```json
{
  "schema_version": 1,
  "topic": "health_coach_latest",
  "source": {
    "session_id": "run-88011-1754800000",
    "provider": "ollama",
    "model": "llama3.2",
    "original_message_count": 18,
    "policy": { "max_messages": 16, "tool_content_max_chars": 800 }
  },
  "head_tail_window": [
    {
      "role": "system",
      "content": "You are mAgent, an embedded AI health coach..."
    },
    {
      "role": "user",
      "content": "Read my heart rate."
    },
    {
      "role": "assistant",
      "content": "Heart rate is 72 bpm, well within resting range."
    },
    {
      "role": "tool",
      "content": "[… 23 lines omitted; see full record for raw bytes]",
      "tool_call_id": "call_abc123"
    }
  ],
  "llm_summary": "User asked for heart rate; assistant reported 72 bpm.",
  "stats": {
    "kept": 4,
    "dropped": 14,
    "tool_results_truncated": 1,
    "bytes_saved": 842
  },
  "metadata": {
    "description": "Latest run from the on-watch health agent.",
    "author": "you@example.com",
    "tags": ["wearable", "nrf52"]
  },
  "history": [
    {
      "updated_at": 1754716800,
      "kept": 4,
      "source_session_id": "run-88011-1754716800"
    }
  ],
  "created_at": 1754716800,
  "updated_at": 1754800000
}
```

Field reference:

* **`schema_version`** — bump this (and
  `magent_core::summary::CURRENT_SCHEMA_VERSION`) if you ever change
  the JSON shape in a breaking way. Newer magent binaries refuse to
  load files with a higher `schema_version`.
* **`topic`** — must be filesystem-safe (no `/`, no `..`, no leading
  dot) and ≤ 128 bytes. Mirrors `magent set-prompt` rules.
* **`source.session_id`** — opaque per-run identifier printed by
  `magent run --json`. `null` when the run didn't generate one.
* **`source.policy`** — snapshot of the `CompressionPolicy` that
  produced the stored window, so a reader can tell whether the
  window was generated with aggressive or conservative settings.
* **`head_tail_window[i].role`** — `"system"`, `"user"`,
  `"assistant"`, or `"tool"`. The LLM-side `Role` enum is
  intentionally not exposed; strings round-trip cleanly with any
  JSON tooling.
* **`head_tail_window[i].tool_call_id`** — preserved verbatim so the
  LLM can correlate the result with the original call. `null` on
  non-tool messages.
* **`llm_summary`** — optional natural-language summary produced by
  the LLM (or a human). `null` when summarisation wasn't requested
  or failed. Capped at 32 KiB.
* **`stats`** — compression counters captured at save time:
  * `kept` — messages kept after slicing.
  * `dropped` — messages dropped by the slicing step.
  * `tool_results_truncated` — tool messages that had their content
    shortened because they exceeded `tool_content_max_chars`.
  * `bytes_saved` — bytes of tool content removed by the truncation
    step (before the marker is inserted). Useful for telemetry.
* **`metadata.tags`** — always serialised as an array (possibly
  empty) so `grep` / `jq` queries work.
* **`history`** — FIFO of superseded snapshots. Index 0 is the
  oldest, index `len-1` is the most recent. Capped at
  [`HISTORY_MAX`](#validation-rules) = 5.
* **`created_at` / `updated_at`** — Unix seconds; `created_at`
  survives updates.

## Atomic write semantics

Every `save` is **crash-safe**. The flow:

```
┌────────────────────────────────────────────────────────┐
│  1. record.validate()                                  │
│     ↓                                                  │
│  2. acquire process mutex → per-topic lock file        │
│     ↓                                                  │
│  3. read previous record (if any) → merge_with_prev    │
│     ↓                                                  │
│  4. record.validate() again (post-merge)               │
│     ↓                                                  │
│  5. serialise JSON, check ≤ MAX_RECORD_BYTES (64 KiB)  │
│     ↓                                                  │
│  6. write <dir>/<topic>.json.tmp                       │
│     ↓ fsync                                            │
│  7. rename .json.tmp → .json        (atomic on same FS)│
│     ↓                                                  │
│  8. fsync dir          (best-effort; ignored on tmpfs)  │
└────────────────────────────────────────────────────────┘
```

Crash guarantees:

* A crash **before** step 7 leaves the directory without a record
  for `<topic>` (the temp file is orphaned and ignored on next read).
* A crash **after** step 7 leaves the previous full record on disk —
  the rename is atomic at the filesystem level (POSIX `rename(2)` /
  NTFS / APFS). On `exfat` and `fat32` `rename(2)` is *not*
  atomic — we document this here rather than trying to detect it.

Lock-file handling:

* Per-topic lock files live in `<dir>/.locks/<topic>.lock` and are
  acquired with `O_CREAT | O_EXCL`.
* The lock file is removed via an RAII guard on success or panic.
* A stale lock file (left over from a crashed process) is
  **overwritten on the next writer's `O_EXCL`** — a deliberate
  safety trade-off; we'd rather proceed than wedge.

## Concurrency model

```text
process A                     process B
─────────────────────         ─────────────────────
save("alpha")                 save("alpha")
  │                             │
  ├─ process mutex (in-proc)    ├─ process mutex (blocked)
  ├─ topic lock ("alpha")       │
  ├─ validate                  │
  ├─ atomic write              │
  ├─ release topic lock        │
  ├─ release process mutex     ├─ process mutex (unblocked)
                                ├─ topic lock ("alpha")
                                ├─ validate
                                ├─ atomic write
                                ...
```

Two layers of coordination:

1. **Process mutex** — serialises every `save` *within* a single
   process. Two threads in the same `magent` binary saving
   different topics don't fight over `O_EXCL` lock files (cheap,
   in-process).
2. **Per-topic lock file** — serialises saves across *processes*.
   Two `magent summary save` invocations from different shells
   writing the same topic are safe.

`load`, `list`, and `delete` do **not** acquire the topic lock —
they read or remove the record directly. If a reader sees a
half-written file (i.e. the temp file before the rename), the
reader treats it as a missing record (the record path is
`<dir>/<topic>.json`, not `<dir>/<topic>.json.tmp`).

## Validation rules

All limits are enforced at save time by `SummaryRecord::validate()`:

| Field                  | Limit                  | Constant                       |
| ---------------------- | ---------------------- | ------------------------------ |
| `topic`                | ≤ 128 bytes            | `SUMMARY_TOPIC_MAX`            |
| `metadata.description` | ≤ 1024 bytes           | `SUMMARY_DESCRIPTION_MAX`      |
| `metadata.author`      | ≤ 256 bytes, no ctrl   | `SUMMARY_AUTHOR_MAX`           |
| `metadata.tags`        | ≤ 32 entries           | `SUMMARY_TAGS_MAX`             |
| `metadata.tags[i]`     | ≤ 64 bytes, no ws      | `SUMMARY_TAG_MAX`              |
| `llm_summary`          | ≤ 32 KiB               | `SUMMARY_LLM_MAX`              |
| `history`              | ≤ 5 entries            | `HISTORY_MAX`                  |
| **Whole record**       | **≤ 64 KiB on disk**   | `MAX_RECORD_BYTES`             |

`save` **fails fast** on any violation — a too-large record never
reaches disk.

## Integration with `magent run`

`run` integrates with the summary store via two flags:

* `--save-summary <TOPIC>` — at the end of the run, persist the
  post-run window under `<TOPIC>`. Errors surface as a `[Summary]`
  trace line (Human mode) or `info:` line on stderr (JSON mode) —
  a save failure never poisons the primary result.
* `--save-summary-overwrite` — allow replacing an existing record.
  Default behaviour is to refuse.
* `--load-summary <TOPIC>` — load the stored record for `<TOPIC>`
  and inject its `head_tail_window` as a system note immediately
  after the live system prompt. A missing topic is a warning in
  Human mode and a hard error in `--json` mode.

```sh
# First run: persist the compressed tail.
magent run --save-summary coach_session "Track my morning run"

# Second run: continue from where the last one left off.
magent run --load-summary coach_session "Now compare with yesterday"
```

The injected system note is rendered by `render_summary_context()`
in `cli/src/runner.rs`:

```text
## Context from a previous run (topic: coach_session)
The following is the head/tail window the agent saw at the end of its previous run.
Use it as background, but treat the user's *current* task as the source of truth.

### Previous LLM summary
User asked for morning run tracking; agent recorded 5.2 km in 28:14.

### Previous window (4 messages)
[0] system: You are mAgent, an embedded AI health coach…
[1] user: Track my morning run
[2] assistant: Logged 5.2 km in 28:14, average pace 5:25/km.
[3] tool: [truncated sensor dump] (tool_call_id=call_abc123)
```

## Audit & version-control

Putting the summaries directory under git is the recommended way to
recover an audit trail across processes / machines:

```sh
git init ~/.magent  # one-time
git -C ~/.magent add summaries/
git -C ~/.magent commit -m "add coach_session summary"
```

Subsequent saves produce a normal `git diff`:

```diff
 {
   "schema_version": 1,
   "topic": "coach_session",
   "stats": {
-    "kept": 4,
+    "kept": 6,
     "dropped": 14,
     ...
   },
-  "history": [],
+  "history": [
+    { "updated_at": 1754716800, "kept": 4, "source_session_id": "run-88011-1754716800" }
+  ],
-  "updated_at": 1754716800
+  "updated_at": 1754800000
 }
```

## Programmatic API

Library users get the trait and the host implementation:

```rust
use magent_core::summary::{
    FileSummaryStore, SummaryRecord, SummaryStore,
};

let store = FileSummaryStore::open_default();
let record: SummaryRecord = store.load("coach_session")?;

println!("{} messages in window", record.head_tail_window.len());
```

Subcommand glue:

```rust
use magent::summary::{SummaryCmd, SummaryAction, SummarySaveOptions};
use magent::output::{Output, OutputKind};
use std::path::PathBuf;

let action = SummaryAction::Save(SummarySaveOptions {
    topic: "alpha".into(),
    from: Some(PathBuf::from("/tmp/seed.json")),
    overwrite: false,
    dir: None,
});
let mut out = Output::new(OutputKind::Human, true);
SummaryCmd::new(&action).execute(&mut out)?;
```

Rollback:

```rust
use magent_core::summary::{rollback, FileSummaryStore, SummaryStore};

let store = FileSummaryStore::open_default();
let next = rollback(&store, "coach_session", 0)?;
store.save(next)?;
```

## Schemas

* **v1** (current) — the layout above. Anything tagged
  `schema_version` ≤ 1 deserialises as `SummaryRecord`.
* **v0** — implicit: hand-written files that omit `schema_version`
  default to `1` via `#[serde(default)]` on
  `SummaryRecord::schema_version`. Older magent binaries that
  don't understand the field still load the rest.

Bumping the version requires:

1. Bump `CURRENT_SCHEMA_VERSION` in
   `magent-core/src/summary/record.rs`.
2. Add a `match` arm in `parse()` to migrate older records.
3. Bump `SUMMARY_STORE.md` with the migration notes (this file).

## Embedded backend (future)

The store is a trait (`SummaryStore`) so the same data layer works
on the host (`FileSummaryStore`, this implementation) and on
embedded targets (`KvSummaryStore`, planned). The trait surface
is intentionally minimal:

```rust
pub trait SummaryStore {
    fn save(&self, record: SummaryRecord) -> Result<WriteReport, SummaryError>;
    fn load(&self, topic: &str) -> Result<SummaryRecord, SummaryError>;
    fn list(&self) -> Result<Vec<SummaryRecord>, SummaryError>;
    fn delete(&self, topic: &str) -> Result<(), SummaryError>;
}
```

The embedded backend will use `postcard` + `heapless::Vec<N>` to
stay within the nRF52840's 256 KB RAM. See P2 in the project
tracker.

## See also

* [`docs/CONTEXT_MANAGEMENT.md`](docs/CONTEXT_MANAGEMENT.md) — how
  `--max-messages` / `--tool-max-chars` produce the
  `head_tail_window` in the first place.
* [`docs/PROMPT_STORE.md`](docs/PROMPT_STORE.md) — the analogous
  store for system prompts (same file-per-record JSON convention).
* [`docs/CONFIG.md`](docs/CONFIG.md) — how `magent run` config
  values like `compression.max_messages` become
  `CompressionPolicySnapshot::max_messages` in `source.policy`.