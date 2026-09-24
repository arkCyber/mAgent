# CHANGELOG

All notable changes to the **mAgent** open-source codebase will be documented
here. Dates use `YYYY-MM-DD` and follow ISO 8601. Versions follow
[Semantic Versioning](https://semver.org/).

> **Scope reminder**: This file tracks the **open-source codebase**
> (`mAgent` / `MicroAgent`). The target commercial product line is marketed
> under the brand **arkChip-mAgent**; commercial SKU release notes are kept
> in a separate, private changelog and are shared with partners under NDA.

---

## [Unreleased]

### Fixed (2026-09-24) — the REPL prompt no longer leaks into a redirected stdout
- **`magent run --repl > session.log` logged one line *per keystroke*.**
  The line editor wrote its prompt and every redraw to stdout, so with a
  terminal stdin and a redirected stdout the log collected
  `❯ h`, `❯ he`, `❯ hel`, … — each prefixed with the redraw's `\r\x1b[K` —
  instead of the transcript. The prompt is terminal UI, not data.
  Both read paths now draw through the new `lineedit::ui_on_stdout`: UI
  goes to stdout only while stdout *is* the terminal. Otherwise it goes to
  stderr, which keeps two things true at once — the redirected stream
  holds the transcript and nothing else, and the prompt is still visible
  on the terminal when the *other* stream was the one redirected
  (`2> err.log`). When neither is a terminal (a scripted session) stderr
  takes it as the diagnostic stream.
  `clear`/`cls` follows the same rule and, unlike before, clears the
  screen on a terminal stderr too, while still writing nothing at all when
  neither stream is a terminal (no `ESC[2J` bytes in a file).
  Guarded by `repl_prompt_goes_to_stderr_not_stdout`; the four
  stdin/stdout combinations were each verified against a real pty.

### Fixed (2026-09-24) — a scripted `--repl` session now reports failure through its exit code
- **`printf 'do the thing\nq\n' | magent run --repl` exited 0 even when the
  task errored.** A failed task printed `✗ Error: …` to stderr and the loop
  carried on; at quit, `execute_repl` always returned `Ok(RunReport)`, which
  `main` maps to `ExitCode::SUCCESS`. So a *scripted* session — stdin is not a
  terminal, nobody reads the transcript — had no way to detect the failure,
  unlike the one-shot path where the same error propagates and exits 1. The
  session now counts failures and, **only when stdin is not a terminal**,
  returns the new `RunError::ReplTaskFailed { failed, total }` at the end
  (`error: \`magent run --repl\` was driven from non-interactive stdin and 1 of
  1 task(s) failed — exiting non-zero so a scripted session can detect it`).
  A human at a terminal still exits 0: quitting an interactive session is
  success, and the transcript already showed the failure.
  Guarded by `scripted_repl_exits_non_zero_after_a_failed_task` and
  `scripted_repl_exits_zero_when_every_task_succeeds`; the interactive
  exit-0 path was verified under a real pty.
  Exposing the exit code also surfaced a latent test bug:
  `repl_honours_the_max_messages_cap` drove its session **without `--mock`**,
  so it depended on whatever LLM happened to be reachable — against a local
  Ollama answering 401 all four tasks failed and the new non-zero exit
  tripped its `assert!(out.status.success())` — and it had been passing
  vacuously, since a failed task appends nothing to the conversation, so
  the `--max-messages` cap it exists to check was never under pressure. It
  now runs `--mock`, like every other REPL test, so the session really
  accumulates and the cap has teeth (17 messages uncapped vs 3 at
  `--max-messages 2`).
- **The REPL help now states that `history` records tasks only**, not
  commands (`history, hist Show task history (tasks, not commands)` plus a
  prose note) — the behaviour `retry` depends on, and the reason `history`'s
  numbering matches `Tasks run` in `stats`. Documentation only.

### Fixed (2026-09-24) — human-mode data now goes to stdout
- **`magent config show > backup.json` wrote a zero-byte file.** In human mode
  `config where` / `show` / `list` / `get` and `set-prompt show` / `list`
  rendered their payload through `Output::stderr_fmt_line`, i.e. to **stderr**,
  while the same commands' `--json` output went to **stdout** — so the stream
  depended on the mode rather than on the command, and every redirect silently
  produced an empty file. All six now write through the new
  `Output::stdout_fmt_line`, so `magent config show > backup.json`,
  `magent config list | grep ollama` and `magent set-prompt show foo | jq .provider`
  work in both modes.
  Diagnostics stay on stderr: confirmations (`config set`, `config init`), the
  `config validate` verdict, warnings, and the `scheduler status` labelled
  trace. `Output::stdout_fmt_line` mirrors `stderr_fmt_line` exactly (same
  `format_args!` contract, same no-op under `--json`), so a call site's stream
  choice is now the only thing that encodes data-vs-diagnostic — documented
  under "OUTPUT STREAMS" in `config --help` and `set-prompt --help`, and in
  `docs/CONFIG.md`.
  Guarded by `cli/tests/streams.rs`, which spawns the real binary and asserts
  on stdout and stderr separately (the unit tests can't observe the bug, since
  `Output` holds the process's real locks). Mutation-checked.
- `web3 list`/`identity`/`did`/`pubkey` are deliberately unchanged (their human
  form is a display table reached through `--json` for data), as is
  `scheduler status` (a labelled trace by construction).

### Fixed (2026-09-24) — three CLI-surface inconsistencies
- **`--tool-max-chars` now counts characters, not bytes.** The limit was
  applied as `msg.content.len() <= max_chars`, so an 800-"char" budget kept
  only ≈266 CJK characters — a silent threefold under-truncation for
  non-ASCII tool output (one-shot and REPL alike). `truncate_tool_content`
  now counts `chars()` and maps the 60/40 head/tail budgets onto code-point
  boundaries, so the flag, the config key
  (`compression.tool_content_max_chars`) and the core parameter all mean what
  they say. ASCII payloads are unaffected. The `[...truncated N bytes...]`
  marker and `CompressionStats::bytes_saved` still report **bytes**, which is
  what they claim. Docs realigned (`run --help`, `config --help`,
  `docs/CONFIG.md`, `docs/CONTEXT_MANAGEMENT.md`, whose
  "`content.len() > tool_content_max_chars`" line was the old semantics
  stated outright). Pinned by `truncate_caps_characters_not_bytes`.
  (Renaming the flag to `--tool-max-bytes` was rejected: the config key and
  the summary record's `policy` field are on-disk formats, so a rename would
  silently drop existing values.)
- **`scheduler --timezone <TZ>` no longer accepts a value it cannot honour.**
  `utc`/`UTC` mapped to `Utc` and *everything else* to `Local`, so
  `--timezone Bogus/Zone` ran happily and silently moved the cron wall clock;
  the help meanwhile advertised IANA names. It now accepts only
  `utc`/`local` (absent ⇒ local) and rejects anything else with
  `scheduler daemon: --timezone expects \`utc\` or \`local\`, got \`X\``.
  Honouring real IANA names would need a tz database, and the CLI takes no
  new dependencies. Two stale claims were removed: the
  `SchedulerTimezone::Local` doc's `$MAGENT_TIMEZONE` reading (never
  implemented) and the help's IANA clause. The daemon usage synopsis also
  gained the `--cron` / `--at` / `--timezone` it was missing. Pinned by
  `scheduler_rejects_a_timezone_it_cannot_honour`.
- **`magent run --prompt /nope.txt` printed the path twice.**
  `runner::load_prompt_file` stringified its error as `"{path}: {io}"` and
  `PromptError::PromptFileLoad` prefixed the same path again, giving
  `could not load system prompt: /nope.txt: /nope.txt: No such file or
  directory (os error 2)`. The loader now returns the bare `io::Error`, so
  the path appears once. Pinned by
  `prompt_file_load_error_names_the_path_exactly_once`.

### Fixed (2026-09-24) — usage errors no longer read "unknown flag: …"
- **`ParseError::UnknownFlag` was the CLI's catch-all.** Roughly eighty-five
  call sites used it for *any* usage error — a missing positional, two
  mutually exclusive flags, an unknown sub-action — while its `Display`
  prefixes `unknown flag: `. Two mutually exclusive scheduler flags therefore
  produced the nonsense sentence
  `error: unknown flag: scheduler daemon: --interval, --cron, and --at are
  mutually exclusive; pass exactly one`.
  New `ParseError::Usage(String)` renders its message **verbatim**, and the
  non-flag sites were migrated to it. The genuinely flag-shaped sites keep
  `UnknownFlag`, so `magent run --bogus`, `magent run -温度 hi`,
  `magent doctor x`, and `magent version x` still print `unknown flag: …`.
  Exit code is unchanged (2); the change is diagnostics-only.
- **A mistyped subcommand is no longer reported as a flag.** `magent
  frobnicate` used to say `unknown flag: frobnicate`; it now says
  `unknown subcommand: frobnicate (try \`help\` for the list of subcommands)`.
  A build without the `web3` feature special-cases the token it was compiled
  without: `magent web3 …` →
  `unknown subcommand: web3 (this binary was built without the \`web3\` feature)`
  (`unknown_subcommand_message`), instead of implying `web3` is a mistyped flag.
- **`help <topic>`** for a topic that is neither a subcommand nor a known help
  topic now answers `no help for \`<topic>\` — try \`help run\`, …`, not
  `unknown flag: help …`.
- Four new unit tests in `cli/src/cli.rs` pin the taxonomy: `Usage` renders
  verbatim with no prefix, an unknown subcommand is named as one, the
  `help <topic>` message, and the non-`web3`-build message. Verified live
  against both a default and a `--features web3` build.
- The `--json` contract is now stated on `GlobalFlags::json`: "`--json` ⇒
  parseable stdout" applies to *data* commands, not all of them — `help` and
  the `--help` texts stay human-readable prose by design.

### Added (2026-09-24) — the REPL has line editing and ↑/↓ history recall
- **`read_repl_line` was a bare `stdin.read_line`.** A typo could only be
  fixed with the terminal's own erase key, and ↑/↓ arrived as the literal
  bytes `ESC [ A` / `ESC [ B` — which the dispatcher then submitted to the
  LLM as a *task*. (`ReplSession::get_history_entry` and its `history_pos`
  field had been sitting `#[allow(dead_code)]` since the feature was
  sketched; both are now gone, since the browse position is per-read state
  and lives in the editor.)
  New `cli/src/lineedit.rs`, in two deliberately separate pieces:
  * `LineBuffer` — a pure `Vec<char>` + cursor with `insert`, `backspace`,
    `delete_forward`, `move_left/right/home/end`, `kill_to_end/start/word`.
    Cursor math is in **chars**, so multi-byte input edits correctly.
  * `read_line` — the terminal loop. On a TTY it clears `ICANON | ECHO`
    via `tcgetattr`/`tcsetattr` (`libc`, already a `cfg(unix)` dependency
    for the SIGPIPE fix), feeds each key to the buffer, and redraws with
    `\r ESC[K prompt text ESC[nD`. `ISIG` is deliberately **left set**, so
    Ctrl-C still signals the foreground process group and kills the REPL
    exactly as before — verified under a pty, not assumed. The terminal is
    restored by an RAII guard, so a panic cannot leave the shell in raw
    mode.
  Bindings: ←/→, ↑/↓, Home/End, Delete, Ctrl-A/B/E/F/K/U/W/L, and Ctrl-D
  (delete-forward mid-line, end-of-input on an empty line) — documented in
  the `help` box, whose rows stay rectangular.
  Two behaviours that had to be preserved, both pinned by tests:
  * **Piped input keeps working byte-for-byte.** Anywhere stdin is not a
    TTY the original `read_line` path runs unchanged, which is how every
    existing test drives the REPL.
  * **A pipe ending without a trailing newline still runs its last
    command** (`printf 'help' | magent run --repl`). EOF now hands back a
    non-empty buffer instead of discarding it; only an empty buffer means
    end-of-input. The `read_line` this replaced returned those partial
    bytes too.
  Raw mode is entered only when `io::stdin().is_terminal()`, and a failed
  `tcgetattr`/`tcsetattr` falls back to the plain read rather than costing
  the user the session. Verified end to end against a real pty: left-arrow
  insertion, Ctrl-A/Ctrl-E, and ↑ recall of the previous task all produced
  the expected redraws and ran the right tasks.
  Tested without a pty by splitting the loop into `tty::edit` over a
  generic `R: Read`/`W: Write`, so the whole keymap is driven from a
  `Cursor<Vec<u8>>`: 37 unit tests in `lineedit.rs` (16 over `LineBuffer`,
  21 over `edit`) plus one integration test for the no-trailing-newline
  pipe. Mutation-checked — dropping the draft-restore in history recall
  fails two tests.

### Fixed (2026-09-24) — a closed stdout turned `magent run --repl` into a panic
- **`magent run --repl | head` exited 101 with a Rust panic instead of
  stopping quietly.** Rust's runtime installs `SIG_IGN` for `SIGPIPE`
  before `main`, so when the reader walks away the next write fails with
  `EPIPE` rather than killing the process. `println!` treats a failed
  stdout write as fatal: `thread 'main' panicked at …/stdio.rs:1166: failed
  printing to stdout: Broken pipe (os error 32)`, exit 101. Every Unix
  filter does the opposite, and a pager or `head` quitting early is
  normal, not an error.
  `main()` now calls `restore_default_sigpipe()` as its first statement,
  setting the disposition back to `SIG_DFL` via `libc::signal`. The
  process is then killed by the signal (status 141) and exits without a
  panic. It is a CLI-wide, process-wide change on purpose: the REPL's
  trace path is not the only place with `println!`, so the one-shot path
  had the same latent panic. `libc` is a `cfg(unix)`-only dependency used
  for nothing else; a `#[cfg(not(unix))]` stub keeps other targets
  building, since `SIGPIPE` is a POSIX concept. Verified live —
  `yes help | head -500 | magent run --repl --mock` went from
  `pipestatus=0 101 0` + panic on stderr to `141 0 0` with clean stderr —
  and covered by `repl_is_not_killed_by_a_rust_panic_when_stdout_closes`
  (fails with exit 101 when the call is removed).

### Fixed (2026-09-24) — `--load-summary` never reached the model, on **either** path
- **The stored window was read, announced, and then discarded before the
  first LLM call.** Found while implementing the flag in the REPL: the
  injected note never survived to the request. Two independent reasons,
  one per path, both a direct consequence of *when* the CLI inserts it —
  at step 3.5, before the runner starts:
  * **One-shot**: the next call is `runner.run(&task)`, whose `run_inner`
    begins with `reset()` — and `reset()` clears the conversation. The
    note was gone before `ensure_system_prompt` ever ran. Verified with a
    local stand-in Ollama that logs the request body: the first request
    contained only the system prompt and the task.
  * **REPL**: `run_continuing()` keeps the conversation, but on a
    still-empty one `insert_after_system_prompt` puts the note at index 0
    (there is no system message yet for it to go after). `ensure_system_prompt`
    then sees a system message whose content is not the prompt, reads it as
    a *stale prompt*, and overwrites it in place.
  The fix moves the insertion into `run_inner`, where both hazards are
  behind it: `RealAgentRunner::seed_system_context(body)` queues the note,
  and `run_inner` applies it at index 1 right after the prompt is
  reconciled — after `reset()`, and at the one index `ensure_system_prompt`
  never touches. It is consumed once, so a multi-turn session keeps the
  note in its history instead of re-prepending it every turn.
  `insert_after_system_prompt` keeps its existing behaviour (it is still
  the right tool mid-session) but now documents the trap.
  End-to-end verified against the stand-in Ollama: one-shot and REPL turns
  1 and 2 all show `system prompt | previous-run note | task`, with exactly
  one copy of the note. Covered by
  `a_seeded_system_context_reaches_the_model_on_a_plain_run` (fails when the
  apply step is stubbed out), the same for `run_continuing` plus a
  no-duplication assertion, and
  `insert_after_system_prompt_on_an_empty_conversation_is_the_trap`
  (pins the old behaviour so the trap is not rediscovered).

### Added (2026-09-24) — the REPL honours `--load-summary`
- **The flag is no longer listed under `--repl ignores`.** `execute_repl`
  now loads the stored record and seeds the session, so
  `magent run --repl --load-summary coach` continues where the previous run
  left off instead of warning that it dropped the flag. A missing topic is
  a warning (the REPL cannot be in JSON mode, so there is no hard-error case
  to mirror from the one-shot path) and does not stop the session.
  `render_summary_context` is shared with the one-shot path, so both render
  the note identically. Covered by `repl_load_summary_is_honoured_and_reported`
  (load reported, nothing dropped, missing topic named) and
  `repl_keeps_the_loaded_summary_across_a_turn` (a differential message
  count against the same script without the flag — fails when the seeding is
  removed). `--save-summary` is still warned about: a REPL has no "end of
  run" to hang a save off.

### Fixed (2026-09-24) — `--probe-ollama` was a no-op, and `--mock` silently overrode it
- **The flag promised "probe on every `run()` call" and changed nothing.**
  `opts.probe_ollama` was set by the flag and by
  `config.runner.probe_ollama_on_run`, and `apply_config_overrides` dutifully
  mirrored the config onto it — but neither `build_runner` nor `execute_repl`
  ever read the field, because both hard-code
  `probe_ollama_on_run: !opts.mock`. In non-mock runs the runner already
  probed once, so the flag's only possible effect was one it never had; in
  mock runs it contradicted the mode it was combined with and was dropped
  without a word.
  The flag now means what it says. Both the one-shot and REPL paths stop
  calling `force_disable_backend()` on a failed startup probe when it is set
  — that call also clears `probe_ollama_on_run`, which was what froze a
  whole session into simulation — leaving the backend wired so `run()` reports
  `LLM backend unreachable` instead of answering from the canned responder.
  The REPL additionally resets the cached verdict between turns via the new
  `RealAgentRunner::reset_backend_probe()`, so a backend that comes up (or
  drops) mid-session is noticed rather than the first answer being latched.
  A probe that cannot be honoured at all (no API key, unbuildable client) now
  says so on stderr instead of failing silently, and `--probe-ollama --mock`
  is rejected up front as `ParseError::ProbeWithMock` — one insists the LLM
  be reachable, the other refuses to contact it. **Behaviour change**: a
  config that sets `runner.probe_ollama_on_run = true` now inherits the strict
  behaviour, since the CLI already treated the two as the same switch; the
  default is `false`, so only explicit opt-ins are affected.
  Covered by `reset_backend_probe_makes_the_next_run_probe_again` (a flaky
  backend, verified to fail when the reset is stubbed out),
  `reset_backend_probe_is_a_noop_without_a_wired_backend`,
  `reset_backend_probe_will_not_downgrade_a_probe_less_runner`,
  `probe_ollama_with_mock_is_rejected` (both flag orders, plus the REPL),
  `probe_ollama_alone_is_accepted`, and the REPL end-to-end
  `repl_probe_ollama_refuses_to_simulate_a_dropped_backend` — which pins the
  contrast against the unflagged run and asserts no simulated answer leaks
  into the transcript.

### Fixed (2026-09-24) — the REPL's boxes were ragged, and a one-word task could be shadowed by a command name
- **Box-drawing rows used byte length as their column count.** `print_welcome_banner`,
  `print_repl_stats`, `print_repl_help` and `print_repl_tools` padded with
  `REPL_BOX_WIDTH - text.len()`, so every row containing a non-ASCII glyph —
  the `🤖` in the banner, the `──` rules in the help and tools boxes — was
  short (bytes ≥ columns) and the right border wandered. Padding now goes
  through `display_width` / `char_display_width`, a small table covering the
  ranges the REPL actually prints; this keeps `unicode-width` out of the CLI's
  dependency set. Covered by `repl_boxes_are_rectangular`, which asserts every
  `║`/`╔`/`╚`/`╠` row of a `help`+`stats`+`tools` session is exactly 64 display
  columns — verified to fail both against a wrong pad and against a byte-length
  `display_width`.
- **`alias <name>` now resolves a name; `!<task>` forces a task.** `alias`
  ignored its argument and reprinted the fixed table, so a user asking
  `alias hist` learned nothing. It now expands the name through
  `alias_lookup` (`hist → history (alias)`, `help is a command (no alias
  needed)`, and a miss is named). Separately, the whole-line dispatch rule
  made a single-word task unreachable whenever it collided with a built-in
  name: `help` always ran the command. A leading `!` escapes that —
  `!help` runs `"help"` as a task — and a bare `!` explains itself. The help
  box documents both. Covered by `repl_alias_with_an_argument_resolves_that_name`
  and `repl_bang_prefix_forces_a_builtin_word_to_be_a_task`.

### Tests (2026-09-24) — the REPL's aliases and case-folding were undocumented by tests
- **Every alias the `help` box advertises now has to reach its arm.** The
  aliases (`?`, `cls`, `hist`, `rerun`, `exit`, `q`) live in the `match`
  patterns rather than a table, so dropping one leaves `help` advertising a
  name that answers `Unknown command` — and no test typed them. Added
  `repl_aliases_reach_the_same_arms_as_their_canonical_spellings` (seed a
  task, then drive `?`, `cls`, `hist`, `rerun`, `exit`), verified to fail
  when `| "?"` is removed from the help arm. Also added
  `repl_commands_are_case_insensitive` (`STATS` / `Hist` / `Quit` — the
  dispatcher matches on `cmd.to_lowercase()`, pinned for the first time;
  fails if the fold is removed) and `repl_exit_stops_reading_further_input`
  (a task written after `exit` must not run; fails if `exit` returns
  `Continue`). REPL integration tests: 28 → 31.

### Fixed (2026-09-24) — `--tool-max-chars` could *grow* a tool result instead of capping it
- **A small budget inflated the very payload it was meant to cap, and the
  savings counter under-reported.** `truncate_tool_content` rewrites an
  over-budget tool result as `head + "\n[...truncated N bytes...]\n" +
  tail`. The marker's fixed ~29-byte overhead is charged against the budget
  but not included in it, so when `max_chars` was smaller than the marker
  the "truncated" string came out **longer** than the original — inverting
  the documented contract (`Cap each tool result content`) and pushing the
  context *up* as the cap went down. Measured end-to-end with
  `magent run --repl --mock --tool-max-chars N`: `1` → 3302 B and `10` →
  3311 B of context, versus 3293 B with truncation disabled. The function
  now builds the candidate and keeps the original when the candidate is not
  strictly shorter, so it can only ever shrink. In the same pass,
  `compress_messages`'s `bytes_saved` was `new_len - max_chars` — the
  marker length, not bytes removed — and now sums the true net delta
  (`before - after`), matching the field's doc and the
  `examples/topic-watcher` reference implementation. Covered by
  `truncate_never_grows_the_content` (a 63 × 255 sweep, verified to fail
  against the old code) and `compress_reports_the_net_bytes_it_removed`.

### Fixed (2026-09-24) — `io.json_default = true` started a REPL that promised JSON and printed prose
- **The `--json` ⇒ JSON-on-stdout contract had a config-shaped hole.** The
  parser refuses `--repl` with the explicit `--json` flag
  (`ParseError::ReplJsonIncompatible`), but `io.json_default = true` puts the
  *same* `Output` into JSON mode with no flag to reject — the REPL started,
  wrote its banner and transcript to stdout, emitted no envelope, and exited
  0. A script doing `magent run --repl | jq` under that config got a banner.
  `execute_repl` now checks `out.kind()` first and returns the new
  `RunError::ReplJsonMode` (exit 1, stdout untouched) naming the key to
  change. Same family as the `--repl --verify-signed` fix: a guard existed,
  but a second entry point bypassed it. Covered by
  `repl_is_rejected_when_config_defaults_to_json`.

### Fixed (2026-09-24) — the REPL's task anchor pinned the *first* turn, not the current one
- **With a tight `--max-messages` the model was asked a stale question.**
  `slice_messages` preserves a "task anchor" so the LLM never loses the
  request it is answering, but it anchored on the **oldest** user message.
  `run()` pushes exactly one user message so the two coincide — but the REPL
  path (`run_continuing`) appends one per turn, so the anchor froze the
  session's first task. At `--max-messages 2` a two-turn session held
  `[system, task A]` on the second turn: the current task had been dropped
  *before the model ever saw it*, and the mock's keyword reply made this
  visible end-to-end. The anchor is now the **last** user message. The
  one-shot path is bit-identical (only one user message exists); the
  behaviour change is confined to multi-turn sessions. The module doc and
  the `CompressionPolicy` doc already promised "the most recent user
  request", so this aligns the code with its stated contract.
  Covered by `slice_keeps_the_current_task_in_a_multi_turn_session`,
  `slice_keeps_the_current_task_at_a_two_message_cap` (both verified to fail
  against the old anchor) and the end-to-end
  `repl_max_messages_keeps_the_current_task`.

### Fixed (2026-09-24) — `run --help` still said `--repl` only rejects `--json`
- **The help text went stale the moment the parser grew its third `--repl`
  constraint.** `run --help` advertised `--repl` as "(multi-turn; takes no
  <TASK>, rejects --json)" while the parser also rejects `--repl` +
  `--verify-signed` (added the same day) — so the flag looked combinable and
  the error came as a surprise. The line now names all three constraints,
  and `run_help_text_documents_repl_constraints` pins it, so a future
  constraint can't be added to the parser without the help following.

### Fixed (2026-09-24) — `--repl --verify-signed` dropped `--repl` without a word
- **Two terminal modes of `run` were silently reconciled in favour of one.**
  `execute()` returns from the `--verify-signed` branch *before* the REPL
  dispatch, so `magent run --repl --verify-signed <PATH>` verified the
  envelope, exited 0/2, and never started the REPL — the `--repl` flag
  vanished with no message. The parser already refuses `--repl` with a task
  (`ReplWithTask`) and with `--json` (`ReplJsonIncompatible`) for exactly
  this reason; the verify combination now gets the same treatment via a new
  `ParseError::ReplVerifySigned` (`web3_app`-gated, like the flag itself).
  Covered by `repl_with_verify_signed_is_rejected`.

### Fixed (2026-09-24) — the REPL `env` listing omitted both passphrase sources
- **`env` promises "every variable the CLI consults", but two of them were
  missing.** The `Credentials` block listed `MAGENT_AGENT_IDENTITY` — read
  only by `sign_after_run` — while omitting `MAGENT_WEB3_PASSPHRASE_ENV`
  and `MAGENT_WEB3_PASSPHRASE`, which `sign_after_run` reads at the same
  place (`runner.rs:1299`). A user who set the passphrase to sign a report
  saw it reflected nowhere in the listing. Both are now shown:
  `MAGENT_WEB3_PASSPHRASE_ENV` verbatim (its value is a variable *name*,
  like `MAGENT_AGENT_IDENTITY`) and `MAGENT_WEB3_PASSPHRASE` redacted as
  `set (***)` (it is the secret). Covered by `repl_env_lists_passphrase_vars`.

### Fixed (2026-09-24) — `run --repl` also dropped `--passphrase-env` without a word
- **The previous fix named two of the three missing signing flags.**
  `--passphrase-env` is read only inside `sign_after_run` (the vault
  passphrase lookup at `runner.rs:1292`), exactly like `--not-before` /
  `--not-after` — but it was still absent from the `--repl ignores …`
  warning, so `magent run --repl --sign --passphrase-env MY_PW` warned
  about `--sign` and dropped the passphrase var silently. Now named under
  the same `web3_app` gate.
  `repl_names_every_signing_flag_it_drops` covers all five signing flags
  (`--sign/--signer`, `--signed-output`, `--not-before`, `--not-after`,
  `--passphrase-env`).

### Fixed (2026-09-24) — `run --repl` silently dropped `--not-before` / `--not-after`
- **The `--repl ignores …` warning named every signing flag but two.**
  `execute_repl` lists the flags only the one-shot path can honour —
  `--load-summary`, `--save-summary`, `--sign`/`--signer`,
  `--signed-output` — but omitted `--not-before` and `--not-after`, even
  though they are consumed in exactly the same place: `sign_run_report`
  inside `sign_after_run`, which the REPL never reaches. So `magent run
  --repl --sign --not-before 100 --not-after 200` warned about `--sign`
  and dropped the validity window without a word. Both are now named,
  under the same `web3_app` gate as the flags themselves. Covered by
  `repl_names_every_signing_flag_it_drops`, which asserts the warning
  names all four signing flags.

### Fixed (2026-09-24) — a lone `/` in `run --repl` was silently ignored
- **The command sigil with no command was the one slash line that did
  nothing.** Every other slash line reports itself — `/nosuch` prints
  `Unknown command: /nosuch` and `/help extra` prints `<line> is not a
  command form` — but `/` (and `/` with trailing whitespace) stripped to an
  empty command name and hit the empty-input arm, which returned silently.
  It now reports `Unknown command: /. Type 'help' for available commands.`
  like any other unrecognised slash command. The empty-input arm is only
  reachable for a lone `/` (the loop skips blank lines, and
  `strip_prefix('/')` empties nothing else). Covered by
  `repl_slash_prefixed_commands_are_recognised`, which now feeds `/` into
  the session and matches the full sentence (the bare prefix
  `"Unknown command: /"` also matches the `/nosuch` line).

### Fixed (2026-09-24) — the REPL's `history` promised a command history it never kept
- **Only tasks are recorded, but everything around the store said
  "commands".** The REPL loop pushes to `session.history` solely on the
  `RunTask` path — built-in commands are never added (which is exactly what
  lets `retry` treat the last entry as the previous task). Yet the help box
  said "Type 'history' to list previous commands", the listing header said
  `Command history`, and the struct field was documented as a "command
  history for navigation". So typing `help`, `stats`, `tools` and then
  `history` printed `(empty)`, contradicting the help text one screen up.
  The header, the help line and the `alias` listing now all say **task**
  history (`Type 'history' to list previous tasks.`), which is what the
  store holds. Guarded by `repl_history_lists_tasks_not_commands`, which
  also asserts a built-in command never appears in the listing.
- Confirmed while investigating (no change): command dispatch is
  case-insensitive (`HELP`, `/STATS` resolve) and leading/trailing
  whitespace is trimmed (`  history  ` == `history`).

### Fixed (2026-09-24) — `run --repl`'s `context` and `stats` reported a byte count labelled "chars"
- **`tokens * 4` is not a character count.** `approx_tokens(s)` is
  `s.len().div_ceil(4)` (`magent-core/src/conversation.rs`), i.e. a *byte*
  count divided by four, so the REPL's `Size: ~{tokens * 4} chars` was
  really the message byte figure — close enough for ASCII, but roughly 3x
  too large for CJK UTF-8 text (a single Chinese character is 3 bytes, 1
  char). Both `context` and `stats` now print the runner's own
  `approx_total_bytes()` (which also counts the per-message overhead)
  under a `bytes` label. Guarded by
  `repl_context_size_is_labelled_bytes`.
- Confirmed while investigating (no change): `--provider` rejects anything
  but `ollama` / `deepseek`; the ollama mock-fallback label matches the
  banner; a per-task error (`✗ Error: …`) leaves the session alive with its
  counters intact and `retry` working; `--quiet` suppresses only the step
  trace.

### Fixed (2026-09-24) — the REPL's `prompt` command reported "(no messages)" in a session that had a prompt
- **`prompt` reads `messages[0]`, but the system prompt is not in the
  conversation until the first turn.** `ensure_system_prompt` inserts it
  inside `think()`, so a fresh `magent run --repl` had an empty message
  list and `prompt` — documented as "Show current system prompt" — answered
  `(no messages)` even though a prompt was plainly configured (and would
  appear on the next task). It now falls back to
  `runner.config().system_prompt` when the conversation is empty, so
  `prompt` shows the real prompt from the very first command. Verified with
  `--prompt-name`: the custom prompt shows before any task, survives
  `reset`, and matches what the next turn installs. Guarded by
  `repl_prompt_shows_the_prompt_before_the_first_task`.
- Confirmed while investigating (no change): `context` / `stats` / `session`
  already report `0` for a fresh session, so only `prompt` was wrong.

### Fixed (2026-09-24) — the context cap `--max-messages` was ignored for a small budget (`--repl` and one-shot)
- **`slice_messages` dropped nothing when a single system prompt plus the
  task anchor already filled the budget.** The "pathological" branch
  computed how many system messages to shed, drained them and **returned**
  — skipping the tail trim that enforces the cap. With one system message
  and `max_messages <= 2` that arithmetic yielded `drop_system == 0`, so a
  `magent run --repl --max-messages 2` session kept growing: four tasks
  left ~17 messages live against a documented cap of 2. The branch now
  only sheds system messages, then the (shared) tail trim runs, so the cap
  holds while the task anchor still survives. Guarded by two new
  `magent-core` unit tests
  (`slice_enforces_cap_when_budget_is_barely_above_the_head`,
  `slice_drops_system_when_budget_is_one_and_keeps_the_task`) and by
  `repl_honours_the_max_messages_cap`, which drives the real binary with
  `--max-messages 2` and asserts the live context stays at the cap.
  Verified end-to-end: the same session now reports 3 messages, not 13.
- Not a bug, confirmed while investigating: even at `--max-messages 1`, the
  system prompt is still in the request. `think()` re-inserts it via
  `ensure_system_prompt` *after* compression and immediately before calling
  the backend, so `prompt` never mislabels and the LLM never sees a
  system-prompt-free turn.

### Fixed (2026-09-24) — `run --repl` ended the session on a non-UTF-8 byte, and mislabelled a known command as "unknown"
- **One non-UTF-8 byte on stdin silently ended the whole session.**
  `read_repl_line` mapped *every* `read_line` error to `None`, the same
  sentinel it uses for EOF — so a pasted binary blob (or a mis-encoded
  pipe) discarded the rest of the input and printed only `Goodbye`, with
  no diagnostic and exit 0. `read_line` consumes a line's bytes before it
  tries to decode them, so this is a *per-line* error, not EOF: it is now
  reported on stderr (`✗ Input error (line skipped): …`) and the loop
  reads the next line. Guarded by
  `repl_skips_a_non_utf8_line_and_continues`, which sends a raw `\xff\xfe`
  line followed by `help` and asserts the `help` banner still prints.
- **A slash command with arguments reported a *known* command as
  unknown.** `/help extra` answered `Unknown command: /help`. A slash line
  with arguments is deliberately rejected (the `/` sigil means "command"),
  but the message now names the rule instead — `… is not a command form: a
  built-in command must be on a line by itself`. Guarded by
  `repl_slash_command_with_args_names_the_real_rule` (and the pre-existing
  `repl_slash_prefixed_commands_are_recognised`, updated to the new text).
- `cli/tests/repl.rs` grows to 20 tests; the `Sandbox` harness gains
  `session_env_bytes` so a test can put non-UTF-8 bytes on the child's
  stdin (a `&str` cannot express them). (21 after the `--max-messages` fix
  above.)

### Fixed (2026-09-24) — `run --repl`'s `help` box omitted `alias` / the `hist` alias and never named the `/` prefix form
- **`help` listed 13 of the 14 built-in commands.** The `alias` command was
  absent from the box entirely, and `history` did not mention its `hist`
  alias (the alias-only `hist` line existed in `history`'s own output but
  not here). The box also never documented the two parser rules a user most
  needs: that a leading `/` is accepted (`/help` == `help`) and that a
  command must be the whole line. All three are now stated. Guarded by the
  new `repl_help_lists_every_command`, which asserts each command/alias
  appears in its own command column — a plain `contains` was too weak, since
  matching prose (`` `alias` alone takes an argument``) satisfied it even
  with the table row deleted.
- **Known unresolved (routed to BACKLOG, needs an owner decision):** the REPL
  aborts with a Rust panic and exit code 101 when its stdout closes early
  (`magent run --repl | head`), whereas the one-shot path exits cleanly with
  1 — the REPL writes via raw `println!`, the one-shot path via `Output`
  (whose `io::Result`s are `?`-propagated). Fixing it means either routing
  every REPL print through `Output` or adding `libc` to reset `SIGPIPE` to
  `SIG_DFL` for the whole CLI; both are larger than a P0 fix, so it is
  documented rather than changed.
- `cli/tests/repl.rs` grows to 18 tests.

### Fixed (2026-09-24) — `run --repl` dropped `--load-summary` silently, and its `env` listing was incomplete
- **`--load-summary` was missing from the ignored-flag warning.** It is
  consumed at step 3.5 of `execute()`, *after* the REPL dispatch returns, so
  the one-shot path's `no summary named "…" on disk` warning never fired
  either — the flag vanished without a trace. It is now named alongside
  `--save-summary` / `--sign` / `--signed-output`. The warning text no longer
  claims a single-task rationale (wrong for the load case) and simply says
  the flags have no effect in an interactive session. Guarded by
  `repl_warns_about_flags_it_cannot_honour`.
- **The `env` command omitted three variables the CLI reads.**
  `MAGENT_PROMPTS_DIR` relocates the prompt store that `--prompt-name` reads
  — and the REPL honours `--prompt-name` — while `MAGENT_SUMMARIES_DIR` and
  `XDG_DATA_HOME` relocate the summaries/prompts base. They are now listed
  under a `Stores` group, so the listing again covers what the CLI actually
  consults. Guarded by `repl_builtin_commands_all_respond`.
- **Removed a dead `ReplOptions` struct** (`cli/src/cli.rs`) whose doc
  comment pointed at a `magent repl` subcommand that does not exist. Nothing
  referenced it (only `pub` visibility hid it from `dead_code`), and it
  duplicated `RunOptions` field-for-field — a trap for anyone adding a REPL
  flag to the wrong struct.

### Fixed (2026-09-24) — `run --repl` hijacked tasks whose first word was a command, and silently dropped `--save-summary` / `--sign`
- **A built-in command had to be the *whole* line, but the dispatcher
  matched the first word only.** So `reset the temperature sensor` reset the
  conversation (discarding the accumulated context), `clear the flash`
  cleared the screen, and `tools list` printed the catalog — which also made
  `help`'s "Any other input is treated as a task for the agent" untrue. A
  line with arguments now falls through to the agent, with `alias` the sole
  exception (it has a documented argument form). `magent run --repl`'s
  **behaviour change**: `quit now` / `help me` are now tasks, not commands —
  type the bare word. Guarded by
  `repl_multi_word_input_is_a_task_not_a_command`. (A *single-word* task that
  collides with a command name, e.g. a task literally named `help`, remains
  unreachable — still tracked in BACKLOG, needs an escape syntax.)
- **The REPL silently accepted output flags it can never honour.**
  `save_summary_after_run` and `sign_after_run` both hang off the one-shot
  `finalize_report`, which the REPL never reaches — so `magent run --repl
  --save-summary demo` wrote no summary and said nothing. It now warns on
  stderr, naming each dropped flag (`--save-summary`, `--sign`/`--signer`,
  `--signed-output`). Guarded by `repl_warns_about_flags_it_cannot_honour`.
- `cli/tests/repl.rs` grows to 17 tests.

### Fixed (2026-09-24) — `run --repl` lost conversation context between turns, and offered the model a blockchain tool with no backend
- **Every REPL turn was a context-free conversation.** `--repl` is
  documented as "maintains conversation context across multiple turns" (see
  `repl_mode` in `cli/src/cli.rs` and `run --help`), but the loop called
  `RealAgentRunner::run`, which clears the message history at the top of
  every call — so `read the temperature` followed by `read the humidity`
  started from scratch, and `context` reported the same size each time
  (`Messages: 5` after both). The core runner grew a `run_continuing()`
  (`run()` → `run_inner(task, keep_history)`) that keeps the history and
  re-inserts the system prompt idempotently, and the REPL now uses it;
  `Messages` goes `5 → 9`. Guarded by `repl_preserves_context_across_tasks`
  and, in `magent-core`, `run_continuing_keeps_history_while_run_resets_it`.
- **The REPL/one-shot prompt advertised blockchain tools that no executor
  could run.** With the `web3` feature compiled in but `--email-tools`
  absent, `CompositeExecutor::new(None)` returns `SimulatorOnly` — there is
  no blockchain backend at all — yet `optional_tool_descriptions()` injected
  `get_balance` / `send_transaction` into the system prompt unconditionally,
  so the model was invited to call tools whose only possible answer is
  `ConfigurationError`. The catalog now gates the blockchain half on
  `CompositeExecutor::has_blockchain()` (the executor's own answer, not "the
  feature is compiled in"), and the REPL's `tools` listing labels the
  blockchain group `(require --email-tools)` alongside the email group,
  matching where the executor is actually auto-wired. Guarded by
  `repl_hides_blockchain_tools_without_email_tools` and
  `build_runner_without_email_tools_has_no_optional_descriptions`.
- `cli/tests/repl.rs` grows to 15 tests.

### Fixed (2026-09-24) — `run --repl` advertised tools the model could never call, and `stats` totals only counted the last task
- **The REPL never handed the optional tools to the model.** Its
  `RunnerConfig` was built with an empty `tool_descriptions`, so the Email
  MCP and blockchain tools (which the `tools` command lists, and which the
  one-shot path injects via `build_runner`) were absent from the prompt the
  LLM received — the agent could not have called one, while `tools` said
  they were available. Both paths now build the catalog through one shared
  `optional_tool_descriptions()` helper. Guarded by
  `repl_hands_optional_tools_to_the_model`.
- **`stats` reported per-run counters under "Total".** `Total iterations` /
  `Tool calls` came straight from the runner, which zeroes them at the top
  of every `run()`, so a two-task session showed only the last task's
  numbers. They are now accumulated on the session (and the REPL's
  `RunReport` uses the same totals). Guarded by
  `repl_stats_accumulates_across_tasks`.
- **Module docs claimed an environment-variable layer that does not
  exist.** Both `cli/src/config.rs` and `cli/src/runner.rs` listed
  "Environment variables (`OLLAMA_HOST`, `DEEPSEEK_HOST`, …)" as a
  configuration layer for `magent run`. Nothing in the crate reads
  `OLLAMA_HOST` outside `magent doctor` — verified: `OLLAMA_HOST=… magent
  run --provider ollama` still contacts `localhost:11434`, while the
  config-file URL *is* honoured. The docs (and the stale storage table,
  which named a nonexistent `~/.magent/config.json` / `%APPDATA%` path) now
  match what `config_path()` and `run` actually do. `magent doctor`'s
  `--help` was already correct.
- `cli/tests/repl.rs` grows to 13 tests.

### Fixed (2026-09-24) — `run --repl`: env-supplied DeepSeek key was ignored, and `backend` lied about the live backend
- **`run --repl --provider deepseek` ignored `DEEPSEEK_API_KEY`.** The REPL
  wired its DeepSeek client from `--api-key` alone, so
  `DEEPSEEK_API_KEY=… magent run --repl --provider deepseek` announced
  *"deepseek (no API key)"* and silently simulated — while the byte-for-byte
  identical `magent run --provider deepseek "<task>"` connected for real
  (it resolves the key through `resolve_api_key_with_config`: `--api-key` →
  configured `api_key_env` → `OLLAMA_API_KEY`). `execute_repl` now takes the
  `ConfigRecord` the caller already loaded and uses the same resolution, so
  the two spellings of "run an agent" agree on credentials too. Guarded by
  `repl_honours_env_deepseek_api_key`.
- **`backend` / `stats` misreported a live DeepSeek session as
  `mock/deepseek`.** Both derived the label from `using_ollama()`, which is
  also `false` for a working DeepSeek backend — so a real connection was
  indistinguishable from the simulated fallback. The label the welcome
  banner computed is now carried on the session and reused, making banner,
  `backend` and `stats` one source of truth. Guarded by
  `repl_backend_command_matches_banner`.
- **`clear` wrote `ESC[2J` even when stdout was not a terminal.** Piped
  sessions (`magent run --repl | tee log`, and the test harness) collected
  raw escape bytes in their transcript; the sequence is now emitted only to
  a TTY. Guarded by `repl_clear_writes_no_escape_when_piped`.
- `execute_repl` no longer re-resolves the system prompt (the caller
  resolves it and errors out first), removing a duplicate file read and an
  unreachable fallback. `cli/tests/repl.rs` grows to 11 tests.

### Fixed (2026-09-24) — `run --repl`: `--quiet` was ignored, and `tools`/`env`/`alias` reported things that don't exist
- **`run --repl --quiet` still printed the per-step trace.** The one-shot
  path gates its `OutputTraceSink` on `opts.quiet`, but `execute_repl`
  installed `ReplTraceSink` unconditionally, so `→ read_sensor(...)` /
  `✓ …` still went to stderr. The REPL now skips the trace sink under
  `--quiet` (the `LogSink` stays, so `RUST_LOG` is unaffected). Guarded by
  `repl_quiet_suppresses_tool_trace`.
- **`tools` advertised a tool that doesn't exist.** The list was hand-written
  prose: it named `ble_scan()` (implemented nowhere in the workspace) and
  sensor names (`heart_rate`, `battery`) the host `SimulatorExecutor` can't
  produce, while omitting the blockchain tools entirely. It is now generated
  from `magent_core::ollama::TOOL_DEFINITIONS` — the same catalog the LLM is
  handed — plus `CompositeExecutor::email_tool_descriptions()` /
  `blockchain_tool_descriptions()`, so the REPL cannot drift from the real
  tool set again. Guarded by `repl_tools_lists_only_real_tools`.
- **`env` printed two variables nothing reads.** It showed `OLLAMA_URL` and
  `MAGENT_CONFIG`; the CLI consults neither (the config file is located via
  `MAGENT_CONFIG_FILE` / `MAGENT_CONFIG_DIR`, and the Ollama URL comes from
  `--ollama-url` or the config file). `env` now lists the variables actually
  read — `MAGENT_CONFIG_FILE`, `MAGENT_CONFIG_DIR`, `XDG_CONFIG_HOME`,
  `OLLAMA_API_KEY`, `DEEPSEEK_API_KEY`, `MAGENT_AGENT_IDENTITY` — with the
  secrets redacted to `set (***)`.
- **`alias` listed `stats` as an alias** (it is a command of its own) and
  omitted `retry, rerun` (which are real aliases). The list now matches the
  dispatcher: `help, ?` / `quit, exit, q` / `clear, cls` / `history, hist` /
  `retry, rerun`.
- `cli/tests/repl.rs` grows to 8 tests (the two above, plus the existing six).

### Fixed (2026-09-24) — `run --repl` now sees the config file, accepts `/`-commands, and has real test coverage
- **`run --repl` ignored the config file.** `RunCmd::execute` short-circuited
  to `execute_repl` *before* the `config::load()` + `apply_config_overrides`
  step, so `magent run "<task>"` honoured
  `$XDG_CONFIG_HOME/magent/magent.json` (and printed
  `[Config] applied provider=…,model=…`) while `magent run --repl` used the
  compiled-in defaults. A provider/model hint carried by a stored prompt
  (`--prompt-name X`) was dropped too — `execute_repl` took `resolved.text`
  and discarded `resolved.provider` / `resolved.model`. The REPL check now
  runs *after* the config and prompt-resolution steps, so both spellings of
  "run an agent" agree. (One visible consequence: `run --repl --prompt
  <bad-file>` now reports the load error instead of silently falling back to
  the built-in prompt — the behaviour `run "<task>"` already had.) Guarded by
  `repl_honours_config_file_provider`.
- **`/help`, `/quit`, … were reported as unknown commands.** The dispatcher
  matched only the bare token, so the slash form fell through to the
  "Unknown command" arm — even though `retry`'s history filter
  (`!h.starts_with('/')`) implied slash-commands existed. A leading `/` is
  now stripped before dispatch, and an unrecognised slash command echoes the
  full token it was given. Guarded by
  `repl_slash_prefixed_commands_are_recognised`.
- **`tools` listed the Email tools even when the crate was built without
  `email-tools`.** The email/blockchain sections now come from the
  executor's own cfg-aware catalogs (which return empty when their feature
  is off); the hand-written list was replaced wholesale in the entry above.
- **Dead predicate removed:** `retry` filtered history with
  `!h.starts_with('/')`, but history only ever holds tasks (commands are
  never pushed onto it), so the filter could never exclude anything. It is
  now plainly `session.history.back()`.
- **The REPL finally has tests.** New `cli/tests/repl.rs` drives the real
  binary with piped stdin in `--mock` mode (6 tests: every built-in command
  responds, slash form, `retry` with and without history, `reset` keeps the
  system prompt, clean EOF exit, config-file provider).

### Fixed (2026-09-24) — `run --repl`: `--json` was not JSON, a task was silently dropped, and the help described two features that don't exist
- **`magent --json run --repl` put 3.6 KB of human transcript on stdout**
  (the banner, the `❯` prompts), which is not a JSON envelope — the REPL
  writes straight through `println!` and ignores the `Output` layer
  entirely. Since an interactive transcript can't be a JSON envelope, the
  combination is now a **usage error** (`rc=2`) rather than a silent
  contract break: *"`magent run --repl` streams an interactive transcript
  and cannot emit a JSON envelope — drop --json"*.
- **`magent run --repl "read the temp"` silently discarded the task.**
  `execute()` enters the REPL as soon as `repl_mode` is set, so the
  positional argument never ran. Now a usage error (`ParseError::ReplWithTask`).
- **Two help-text claims that were false.** `magent run --help` advertised
  `magent run [OPTIONS]   Enter REPL if no task given` — but a bare
  `magent run` is (deliberately) a `MissingTask` error; and the REPL's
  `help` advertised `Use ↑/↓ arrows to navigate command history`, but the
  reader is a plain `read_line`, so an arrow key was submitted *as a task*
  to the LLM. Both claims are gone; `read_line_with_history` is renamed
  to `read_repl_line` with a doc comment that says what it actually does.
- Covered by `repl_with_task_is_rejected` / `repl_with_json_is_rejected`
  in `cli/src/cli.rs` and `repl_is_rejected_under_json_mode` /
  `repl_with_task_is_rejected` in `cli/tests/json_output.rs`.

### Added (2026-09-23) — `magent config export`, the redirect-safe counterpart to `config show`
- **`magent config export`** dumps the resolved config record as pretty JSON
  to **stdout in both modes**, so `magent config export > backup.json`
  produces a real file. `config show` renders its human output to stderr for
  display (and to stdout only under `--json`), so it was never
  redirect-safe; `export` mirrors the dedicated-verb surface that
  `set-prompt export` / `summary export` already provide. Added to the
  parser, `config --help`, and `docs/CONFIG.md`.

### Fixed (2026-09-23) — `--json` output was not JSON for three CLI commands
- **`magent --json version` printed `magent 0.1.0`** — the bare human string,
  not JSON — because `Command::Version` bypassed the `Output` layer with a
  direct `println!`. It now emits `{"name":"magent","version":"0.1.0"}` in
  JSON mode (human mode unchanged), through `Output`.
- **`magent --json web3 sign <NAME> --output <FILE>` wrote nothing to
  stdout.** The envelope goes to the file, so JSON mode emitted zero bytes;
  it now writes a confirmation envelope
  (`{"action":"sign","signer":…,"output":…,"bytes":…}`). Human mode is
  unchanged.
- **`magent --json set-prompt export <NAME>` printed the raw prompt text**
  (`You are terse.`), which is not valid JSON. Human mode keeps its
  byte-for-byte raw-text contract (so `… | magent run -p -` still works);
  JSON mode now emits the **full prompt record** (the same shape as
  `show --json`), consistent with the other export verbs
  (`summary export` / `web3 export` / `config export`), so
  `--json ⇒ parseable stdout` holds.

  All three are covered in `cli/tests/json_output.rs`.

### Fixed (2026-09-23) — the CLI: six commands were silent successes under `--json`, and `run --sign` advertised a flag it did not have
- **Six commands exited 0 under `--json` with zero bytes on *both* streams.**
  A CI pipeline doing `magent --json … | jq` saw an empty stream and either
  errored or silently processed nothing. Two distinct root causes:
  - `summary export` / `summary load` wrote through `Output::write_human`,
    which is a **no-op in JSON mode** — even though those actions exist
    precisely to feed another command and the doc comment claimed they wrote
    "regardless of `--json`". They now use `write_stdout_line`, which writes
    in both modes.
  - `summary delete`, `web3 verify`, `web3 delete`, and `scheduler run-once`
    (empty task list) had **no JSON branch at all** — they only wrote in
    human mode. Each now emits an envelope (`summary`:
    `{"topic":…,"deleted":true}`; `web3`: `{"action":"verify","ok":true,…}` /
    `{"action":"delete","name":…,"removed":true}`; `scheduler` now falls
    through to its existing `{"kind":"scheduler.run_once","ran":0,…}`
    envelope instead of early-returning before it).

  Covered by a new integration test file `cli/tests/json_output.rs`, which
  drives the real binary in a throwaway `$HOME` and asserts non-empty,
  parseable stdout for each of these paths (plus the ones that were already
  correct, so a refactor can't regress them).

- **`magent run --sign` documented a `--passphrase-env` flag it rejected.**
  The signing code's comment claimed "the runner accepts a `--passphrase-env`
  for parity", but `RunOptions` had no such field, so `run --passphrase-env X`
  failed with `error: unknown flag: --passphrase-env` and the only way to sign
  was to export `MAGENT_WEB3_PASSPHRASE`. The flag is now parsed and wired in;
  precedence is `--passphrase-env <VAR>` → `$MAGENT_WEB3_PASSPHRASE_ENV` →
  `MAGENT_WEB3_PASSPHRASE`. The failure message on a missing passphrase is now
  accurate (`export it, or pass --passphrase-env <VAR>`) and the flag appears
  in `magent run --help`.

### Fixed (2026-09-23) — the email MCP pair: `shutdown()` panicked by design, tool arguments reached IMAP unvalidated, and a bad config file blamed the wrong thing
- **`McpToolExecutor::shutdown()` panicked on every call.** It was the repo's
  only `tokio::runtime::Handle::current()`, which panics with *"there is no
  reactor running"* when there is no ambient runtime — exactly the case this
  crate documents for itself (`The CLI can be a plain fn main() (no
  #[tokio::main])`). Its sibling `execute_async_blocking` had always used the
  `tokio_runtime()` helper, which builds a process-wide runtime when none
  exists; `shutdown` now does the same. (Inside an async context the blocking
  call still cannot be made — noted on the method.) Regression test:
  `shutdown_outside_a_tokio_runtime_does_not_panic`. Its `Default` impl, too,
  carried a comment promising a fallback to `target/release/magent-email-mcp`
  that the code has never had; the comment now describes what it does (a bare
  `magent-email-mcp`, resolved via `$PATH`).
- **`magent-email-mcp`'s `limit` / `uid` arguments were cast with `as u32` and
  never range-checked.** The input schemas advertise `1..=200` and `minimum: 1`,
  but MCP clients do not enforce a schema for you, and the values are
  interpolated into the *text* of an IMAP command. `limit: 0` therefore built an
  empty sequence set — `FETCH  UID FLAGS ENVELOPE`, a protocol error the caller
  saw as an opaque server reply rather than as their own mistake — and a value
  ≥ 2³² wrapped: `limit: 4294967296` returned **zero** messages instead of
  erroring, and `uid: 4294967296` searched for UID 0, which is not a valid IMAP
  UID. Both are now parsed into `u32` fallibly and rejected out of range with a
  message naming the bound (`\`limit\` must be between 1 and 200, got 0`).
- **A malformed `~/.config/magent/email-mcp.toml` was reported as a missing
  field.** The fallback path parsed with
  `toml::from_str(raw).unwrap_or_default()`, so any syntax error silently became
  an all-`None` struct and surfaced as `email-mcp.toml missing imap.host` —
  sending the operator to look for a key that is in fact present in their file.
  The parse error is now propagated as-is, every diagnostic names the config
  path, and a file that is well-formed but genuinely incomplete still reports
  the specific missing key.

### Fixed (2026-09-23) — `magent web3`/`run --repl`: silent stdout loss, an infinite REPL loop, an ignored flag, and a mislabelled KDF error
- **`magent web3 sign … > env.json` and `magent web3 export … > alice.json`
  produced *empty* files.** Both commands documented their payload as the
  command's stdout (`sign` defaults to stdout; `export`'s help says "Dump the
  public-side JSON record to stdout"), but both wrote it through
  `Output::write_json_str`, which is a **no-op in human mode** — so without
  `--json` the envelope / record was silently dropped and only a one-line
  "signed with did: …" note reached stderr. `sign` with `--output <FILE>` was
  unaffected. A new `Output::write_stdout_line` writes the payload to stdout in
  *both* modes; `sign` (default output) and `export` use it, so redirection now
  behaves identically in human and JSON mode.
- **`magent run --repl` spun forever at EOF instead of exiting.** The REPL read
  input with `read_line`, whose `Ok(0)` at EOF (piped stdin, or Ctrl-D) was
  treated as an empty line → `continue` → the loop re-read EOF forever. A
  non-interactive `magent run --repl < script` never terminated and emitted a
  `❯ ` prompt line in a tight loop (measured ≈5.5 MB in 8 s before being
  killed). `read_line_with_history` now returns `Option<String>` and the loop
  breaks on `None`. `quit`/`exit`/`q` are unchanged.
- **`magent version --bogus` exited 0 and ignored the flag.** The `version`
  subcommand returned before inspecting the remainder of argv, so any typo
  (`version --bogus`, `version extra`) was silently swallowed — unlike every
  other subcommand, which reports `unknown flag: …` with exit 2. `version` now
  rejects trailing tokens. Global flags are still stripped before this check, so
  `version --json` keeps working.
- **Bad key-derivation input was reported as "Argon2id KDF failed".** The
  `--from-seed` / `--from-pubkey` paths reuse `decode_hex`, whose errors were
  tagged `Web3CliError::Kdf`; a typo in a seed therefore surfaced as
  `error: Argon2id KDF failed: invalid hex digit: 'z'`, sending the user after a
  key-derivation problem that didn't exist. A new `Web3CliError::Usage` variant
  renders these as-is: `invalid hex digit: 'z'`, `hex string must have an even
  number of digits`, `must pass --from-seed <HEX>`. The genuine Argon2id errors
  (salt/params/AEAD) still use `Kdf`.
- **`magent scheduler --help` listed `--preset` twice.** Cosmetic: the
  duplicated OPTIONS line was removed.

### Fixed (2026-09-23) — the `magent` CLI: two commands printed nothing for `--json`, a zero cap guaranteed a failed run, and the template help named the wrong syntax
- **`magent --json scheduler status` and `scheduler run-once` emitted zero
  bytes.** Both built a JSON envelope and handed it to `Output::final_answer`,
  which in JSON mode only *stashes* the string for a later `Output::write_json`
  flush — but neither path ever called it, so a CI pipeline that did
  `magent --json scheduler status | jq …` got an empty stream (exit 0). Both
  now call `Output::write_json_str` to write the pre-rendered envelope. This was
  the only pair of `final_answer` call sites that never flushed.
- **`-i 0` / `--max-iterations 0` (and `-t` / `--max-tool-calls 0`) were
  accepted and then made every run fail.** The ReAct runner tests
  `iteration >= max_iterations` *before* its first step, so a cap of `0`
  permits zero steps and the run ends with the opaque
  "run ended without a final answer". A comment in `runner.rs` claimed `0` was
  a legitimate "no cap" the runner honoured — it is not, and the core
  `MiniAgent` rejects `max_iterations == 0` outright. The flags now reject `0`
  with `expected a positive integer (>= 1)` (all four spellings: `-i`, `-i0`,
  `--max-iterations`, `--max-iterations=0`); `config validate` gained matching
  `>= 1` rules for `runner.max_iterations` / `runner.max_tool_calls`, and `run`
  warns when a hand-edited config carries `0` so the operator sees why the run
  is about to fail. `--max-messages 0` / `--tool-max-chars 0` are untouched —
  there `0` really does mean "off".
- **`magent set-prompt --help` advertised `{VAR}` placeholders, but the
  renderer only substitutes `{{VAR}}`.** The printed usage line read
  `Render a stored prompt with {VAR} placeholders`; a user who followed it got
  their template echoed back verbatim with no substitution and no error
  (unfilled placeholders are left as-is by design). The line now prints the
  real `{{VAR}}` syntax, matching `docs/PROMPT_STORE.md` and the module docs.

### Fixed (2026-09-23) — ESP32-S3 latency stats corrupted by integer narrowing
- **A timing sample above ~35.8 min wrapped negative and poisoned the
  running average and WCET.** `latency_metrics.rs` `record()` narrowed a
  `u64` µs value with `us as i32`. Any value above `i32::MAX` µs (≈ 35.8 min)
  became negative, so the `/api/status` `avg_ms` / `wcet_ms` for that channel
  reported a nonsense figure. The accumulator now saturates to `u32::MAX` on
  the `u64→u32` step and to `i32::MAX` on the `u32→i32` step before the
  average is updated — a corrupt sample can no longer drag the mean down.
  The module doc now also notes the two narrowings.

### Fixed (2026-09-23) — the shared truncation helper lost appends to a non-empty buffer
- **`text_util::push_str_truncated` budgeted against the total capacity `N`
  instead of the remaining space.** A caller appending to a non-empty
  `heapless::String<N>` asked `push_str` for more bytes than fit; that returned
  `Err`, which the `let _ =` silently discarded — so the whole append vanished.
  All current callers pass a fresh buffer, so this was latent rather than live,
  but it is now `N.saturating_sub(out.len())`. Covered by a new unit test
  (`appends_up_to_the_remaining_capacity`: `"he"` then `"xyz"` into a `<4>`
  buffer yields `"hexy"`).

### Fixed (2026-09-23) — the SNTP force-sync path logged a sync it had not done, and deferred the real one
- **`sntp_sync.rs` set `last_sync_ms = now` on a force-sync before any NTP
  exchange occurred,** then logged it as a completed sync. Because the periodic
  re-sync gate compares against `last_sync_ms`, the bogus stamp also *postponed*
  the genuine re-sync by up to `RESYNC_INTERVAL_S`. The force path now
  re-requests the poll without moving `last_sync_ms`, and logs the current RTC
  state plainly (`"force-sync: re-poll requested; recorded current RTC
  wall_unix=… (no NTP exchange yet)"`).

### Docs (2026-09-23) — three firmware modules described behaviour they do not have
- **`rt_watchdog.rs` stated an "18s" timeout as "15s"** in the module doc while
  `RT_WDT_TIMEOUT_MS` is `18_000`; the doc now names the constant. The `rt_wdt`
  `cfg` explanation was also under-specified: it now spells out that the S3
  overlays differ — `sdkconfig.defaults` (used by `build-s3.sh`) sets
  `CONFIG_ESP_TASK_WDT_EN=y` so the watchdog **is** armed on the linked quad-
  PSRAM board, while `sdkconfig.s3.defaults` (the N8R8 octal module) sets it
  `=n` and degrades the module to a no-op.
- **`device_key.rs` claimed test coverage that does not exist.** The comment
  block named `tests/device_key_integration_tests.rs` ("exercised by the
  hardware-in-the-loop CI job") and inline wire-format tests, but no `tests/`
  directory exists and the crate is an esp-idf bin no host `cargo test` can
  build — so neither has ever run. The comment now says so plainly and points
  at `docs/BACKLOG.md`.
- **`web_admin.rs` `reason_label` doc** implied the function itself surfaced
  the raw code; it returns `"unknown"` and callers render the number. Doc
  corrected.

### Fixed (2026-09-23) — ESP32-S3: three paths reported success for work that did not complete
- **A stalled USB-JTAG TX reported a truncated reply as fully sent.**
  `link_adapters.rs` `UsbSerialAdapter::send` gives up after ~100 ms of a
  non-draining host and `break`s out of the write loop — then returned
  `Ok(())`. The caller (`main.rs`, the reply drain) therefore logged
  `[ingress] reply sent to host: <reply>` for a reply whose tail was silently
  discarded, so a host would act on a half-received AT/agent response. The
  stall path now returns a new `AdapterError::Truncated` (mapped to
  `ErrorKind::WriteZero`), which the caller logs as a send failure.
- **HTTP body-read errors were swallowed as end-of-body.**
  `local_tools.rs` `http_get_bounded` and `at_dispatch.rs` `AT+HTTPGET` both
  matched `Err(_) => break` on the body read, identical to the clean `Ok(0)`
  EOF. `fetch_web` / `get_weather` / `get_news` then returned `success=true`
  with a byte count, and `AT+HTTPGET` emitted `+HTTPGET:<status> bytes=<n>`,
  for a body the server never finished delivering. Both now treat a mid-body
  read error as a failure: the tool returns `Err` (→ `success=false`) and the
  AT command returns `+CMDER:6`.
- **`AT+OTA` replied `+OTA:STARTED` before validating the URL.**
  `ota_dispatch` spawned the worker (which checks `https://`) and answered
  immediately, so a plaintext `http://` URL got a success reply and the refusal
  appeared only in the log. The dispatcher now calls `ota::validate_ota_url`
  synchronously and returns `+CMDER:7` for a rejected URL. The unreachable
  `Ok(())` log arm inside the worker (it `esp_restart`s on success) is now
  marked as such.

### Fixed (2026-09-23) — `AT+CWRECONNCFG` was persisted but never applied
- **The Wi-Fi supervisor ignored the operator's reconnect policy.** The
  dispatcher wrote `mag_at:reconn_int` / `mag_at:reconn_rep` to NVS, but the
  only reader was the command's own `?` query — the supervisor used hard-coded
  constants (`initial_backoff` 2 s, `max_backoff` 60 s, unlimited retries). So
  `AT+CWRECONNCFG=5,100` (documented as "retry every 5s up to 100 times")
  changed nothing. `run_wifi_supervisor` now reads both once at start and
  applies them: `interval` caps the adaptive reconnect backoff (the
  credential-error backoff keeps its deliberate long delay) and seeds the
  manager's `max_backoff`; `repeat` caps reconnect attempts per outage — after
  the budget is spent the supervisor stops and logs once, resuming on
  `AT+CWJAP=` or a reboot. `AT+CWRECONNCFG?` now reports values that actually
  take effect. Verified with
  `cargo +esp check --target xtensa-esp32s3-espidf --no-default-features
  --features board-s3,wifi,uart` (clean).

### Fixed (2026-09-23) — `AgentConfig` iteration/memory limits were write-only, so the ESP32-S3 ran on crate defaults
- **`MiniAgent::new` built its `BudgetEnforcer` from `BudgetEnforcer::with_defaults()`
  (50 iterations / 50 KiB) regardless of the `AgentConfig` it was handed.**
  `AgentConfig::with_max_iterations` / `with_max_memory` stored the values in
  the config but no production code read them, so the ESP32-S3 firmware's
  `with_max_iterations(20)` and `with_max_memory(MAX_DYNAMIC_CONTEXT_BYTES)`
  were silently discarded and the ReAct loop ran with the crate defaults. This
  is the more serious half of the S3 budget story: the memory axis is inert
  anyway (see below), but the *iteration* cap is enforced, and a documented
  ceiling of 20 was in practice 50. `MiniAgent::new` now builds the enforcer
  from `config.max_iterations` / `config.max_memory` (keeping the 10 s time
  limit, for which `AgentConfig` has no field). `test` `budget_limits_come_from_the_config`
  pins it: it failed as `left: 50, right: 7` before the fix.
- **The S3 "2 MiB dynamic-context budget" is a configured ceiling, not an
  enforced one.** `MAX_DYNAMIC_CONTEXT_BYTES` reaches the device only through
  `AgentConfig::with_max_memory` → `BudgetEnforcer::memory_limit`, which binds
  only if something calls `consume_memory`; no production path does. Its one
  real consumer, `conversation::gc_to_budget`, is called solely by the host
  `RealAgentRunner` (`agent_runner.rs`), not by the embedded `MiniAgent`, whose
  conversation is count-bounded (`MAX_CONVERSATION_MESSAGES` = 20 ×
  `MAX_BUFFER_SIZE` = 2 KiB ≈ 40 KiB). The `MAX_DYNAMIC_CONTEXT_BYTES` doc and
  the firmware's `with_max_memory` comment now say so instead of implying the
  device GCs against the figure.

### Docs (2026-09-23) — the S3 Lua docs blamed the build for an on-device crash, and described a driver stack the firmware does not use
- **`firmware/esp32-app/Cargo.toml` `lua` feature comment gave a stale reason
  for being OFF by default.** It said `mlua`'s vendored `lua-src` "does not yet
  build for `xtensa-esp32s3-espidf`", so enabling it "would break the S3
  firmware build". That was true for an earlier revision, but the `magent-lua`
  dep is now pinned to `default-features = false, features = ["piccolo"]` —
  a pure-Rust engine with no vendored C — and
  `--features board-s3,wifi,uart,lua` compiles cleanly for Xtensa. The real
  reason it stays off is that the Lua VM crashes Core0 on hardware
  (`build-s3.sh:17`). The comment now says the latter and names the build as a
  non-issue.
- **`docs/LUA_SCRIPTING_S3.md` carried a duplicated `Status (2026-08)` block**
  (two headers spliced mid-sentence) and described an `esp-hal` GPIO/I2C/PWM/SPI
  driver stack. The firmware implements `HardwareBackend` on raw **ESP-IDF C
  APIs** (`esp_idf_sys` LEDC / oneshot ADC / I2C / NVS, the same style as
  `local_tools.rs`), not `esp-hal`. The status blocks are merged into one, the
  remaining BLE references (removed from the firmware 2026-09-20) are dropped
  from the chip table and diagram, the driver stack is corrected, and the
  "not yet compiled for Xtensa" sketch header now reads "build-verified for
  Xtensa; on-device run pending".
- **`build-s3.sh` promised Lua self-test output its build cannot produce.** The
  script builds `--features board-s3,wifi,uart` (no `lua`, deliberately — the
  VM crashes Core0), yet its closing message told the operator to "Watch the
  console for the '[lua] <driver> ok/err' hardware self-test lines" — lines a
  build without `lua` never emits. The message now states the omission is
  expected and how to add `lua` back. The doc's flashing section, which told
  the reader to run `./build-s3.sh` to get a Lua-enabled image, now shows the
  explicit `--features …lua` command and flags the crash caveat. The fabricated
  `[lua] ble ok sent` sample line (impossible — `ble_send` always returns
  `Err`) and the "`flash`/`ble` … not yet wired" note (flash *is* wired) were
  removed.

### Fixed (2026-09-23) — the nRF52 watchdog was fed a constant, so its "remaining" figure never moved
- **`firmware/nrf52-app/src/main.rs` fed the watchdog clock with the timeout.**
  `watchdog::Watchdog::feed(current_time_ms)` stores its argument as
  `last_feed` and `remaining_ms(current_time_ms)` computes
  `timeout - (current_time_ms - last_feed)`. `main_task` passed
  `WATCHDOG_TIMEOUT_MS` (a constant `60_000`) to *both*, so `last_feed` became
  `60_000` and `remaining_ms(60_000)` computed `0` elapsed — the status line
  `[MAIN] Watchdog: {}ms remaining` printed the full `60000` on every cycle, a
  countdown that could not count. Both call sites now pass
  `Instant::now().as_millis()` (the same `embassy_time` clock the loop already
  sleeps on), so the figure decreases across the 5 s cycle. This is the second
  figure this scaffold reported as if it were measuring something; the battery
  level was the first (see the entry below). Verified with
  `cargo check -p magent-nrf52-app --target thumbv7em-none-eabihf` across
  default / `--no-default-features` / `thread`, all clean with zero crate
  warnings. The module remains a documented scaffold
  (`watchdog::check_timeout` / `HealthMonitor::check_all` still have no caller,
  tracked under "nRF52840 scaffold warning debt" in `docs/BACKLOG.md`), so this
  fixes the reporting, not the missing reset path.

### Docs (2026-09-23) — `SecurityManager` method docs understated the fail-closed paths
- **`decrypt` and `generate_auth_tag` described only the `std` simulator when
  they also fail closed on embedded.** Both said the no-`web3` path "uses the
  historical XOR placeholder" / "the simulator hash". The code does that only
  on a `std` build; on a `no_std` target both return `AgentError::CryptoError`
  (the module doc already said so — the method docs lagged). The method docs now
  mirror `encrypt`'s, naming the `std` simulator as test-only and the embedded
  path as fail-closed. No behaviour change.
- **`encrypt` claimed the nRF52840 "delegates encryption to the SoftDevice
  AES-CCM engine".** The same module's security notice says that engine "does
  not currently call into that engine". Nothing in the nRF52 firmware calls
  `encrypt`, and nothing wires the SoftDevice engine either, so the old wording
  asserted a delegation that does not exist. It now says the target simply never
  calls the method, with the SoftDevice path marked as unwired design intent.

### Docs (2026-09-23) — `magent_core::at` claimed a firmware AT watchdog and audit format that do not exist
- **Two of the module doc's "Aerograde guarantees" described firmware code
  that isn't there.** It claimed the dispatcher carries an
  `AtState { echo: bool, last_cmd_ticks }` the UART task ticks, with a >500 ms
  stale-line watchdog that forces `+CMDER:7 (timeout)`. No `AtState`,
  `last_cmd_ticks` or stale-line timeout exists anywhere in the tree
  (`grep -rn AtState --include='*.rs'` → nothing). It also claimed every
  command is logged as `(uptime_ms, command, result_code, elapsed_us)`; the real
  line is `[at] op=<name> kind=<kind> -> <outcome> (t=<now_ms>ms)` and the
  latency figures live in `latency_metrics`, not that record. The bullets were
  replaced with what the code actually guarantees: a single O(n) allocation-free
  parse that terminates by error (no loop to get stuck in), `+CMDER:<n>` +
  drop with no agent fallback, and the ESP32 hang detector being `rt_watchdog`
  (18 s hardware task WDT) — not anything in the parser. No behaviour change.
- **`docs/AT_COMMAND_REFERENCE.md` `ATE0`/`ATE1` row corrected.** It said
  "State kept by the dispatcher", which contradicts its own note and the code:
  `at_dispatch.rs` maps `AtOp::SetEcho { .. } => AtOutcome::NoReply` and stores
  no state. The row now says the command is accepted but inert, and names the
  actual arm.
- **Two `AtOp` doc comments attributed ESP32 behaviour to the nRF52.**
  `AtOp::NtpSync` and `AtOp::Ble` both said "nRF52 returns `+CMDER:9`
  unsupported". The nRF52 firmware has no AT command surface — `firmware/nrf52-app/src/`
  contains no dispatcher and never calls this parser — so it returns nothing.
  `+CMDER:9` is the *ESP32* dispatcher's code (and on the ESP32, `AT+BLE=…` is
  the removed-radio refusal, not an nRF52 fact). The comments now say ESP32-only
  and state what each side actually does.

### Docs (2026-09-23) — `safety::Watchdog` was described as a hardware watchdog timer it is not
- **The type had no clock, no hardware access, and no way to trip.** `feed()`
  was the only writer of the flag on a running device (the sole writer of the
  *tripped* state is the test hook `simulate_timeout()`), so `needs_feed()` is
  always `false` outside tests and the type enforces no deadline. Yet
  `docs/API.md` called it a *"Hardware watchdog timer interface"* and the
  `feed()` body carried an `// In real implementation …` note implying a real
  one was pending. No production caller reads the latch: `MiniAgent` feeds it
  once per ReAct iteration and never checks it. The struct/method docs now
  state plainly that it is a clockless heartbeat latch and that real timeout
  enforcement lives in the firmware (`rt_watchdog.rs` on ESP32,
  `watchdog.rs` on nRF52), which own a clock and the hardware counter;
  `docs/API.md` matches. No behaviour change.
- **Related false claim corrected.** `AgentTelemetry::watchdog_trips` and the
  comment above its increment in `agent.rs` described the counter as fed by the
  watchdog's explicit trip *and* the iteration budget's timeout. Neither is
  true: the counter increments only on `AgentError::OperationTimeout`, which
  nothing in the workspace raises, while the iteration budget raises
  `IterationBudgetExhausted`. The docs now say the counter reflects backend
  timeouts only. The underlying dead mechanisms (clockless `Watchdog`, inert
  `BudgetEnforcer` time axis) are recorded in `docs/BACKLOG.md` for the owner's
  call, since replacing or removing them needs either a clock in `no_std` core
  or a breaking change to a documented API.

### Fixed (2026-09-23) — the nRF52 scaffold logged agent execution and a battery reading that never happened
- **`firmware/nrf52-app/src/main.rs` `agent_task` logged work the firmware had
  not done.** The task looped over four canned strings (*"Read the temperature
  sensor"*, *"Check battery level"*, …) and, for each, emitted
  `[AGENT] Running task: <task>`, `[AGENT] Agent is running` and
  `[AGENT] Budget check passed` — while calling nothing at all. `MiniAgent` is
  constructed once in `main`, but driving it needs an `LlmBackend`, and this
  target has no network stack, no local model and no tool handler, so no task
  was ever run and no budget was ever checked. Those three lines went to the
  device's real log on every boot, where an operator — or anything ingesting
  that log — reads them as fact. `agent_task` now announces readiness exactly
  once (`MiniAgent ready; agent loop is a scaffold (no LLM backend wired), no
  task is run`) and is otherwise silent. This matches what the README
  (*"compiles, scaffold only"*) and `docs/BACKLOG.md` already said about this
  target; the log had simply disagreed with both.
- **The same file reported a fabricated battery level as a measurement.**
  `main_task` starts `battery_level` at 75 and decrements it every 60 loop
  cycles, then logs `[MAIN] Cycle #N | Battery: 74%`. There is no SAADC reading
  behind that number (tracked as open in `docs/BACKLOG.md`), so it is a
  liveness counter, not a measurement; the status line now marks it
  `(simulated)` and the counter carries a comment saying so. Verified with
  `cargo check -p magent-nrf52-app --target thumbv7em-none-eabihf` across the
  crate's three feature combinations (default / `--no-default-features` /
  `thread`), all clean with **zero** crate warnings — the state the
  2026-09-20 warning-debt pass established.

### Fixed (2026-09-23) — `WearLeveler::new(0, _)` panicked with an integer division by zero
- **`magent-core/src/wear_leveling.rs` clamped `sector_count` to at most 16 but
  allowed 0.** The rotation strategies then computed `% self.sector_count`, so
  the *first* `get_next_sector()` that reached the rotation threshold
  (`write_count >= max_writes_per_sector`) aborted the thread with
  `attempt to calculate the remainder with a divisor of zero` — a panic in a
  `no_std` library, not an error. `Dynamic` hits it at
  `(current + 1) % self.sector_count`; `Static` at
  `(write_count - 1) % self.sector_count`; `Hybrid` reaches `Static` on every
  100th write. The lower bound is now `clamp(1, 16)`: a one-sector leveler is
  the sensible degeneracy (every write lands in sector 0) and it keeps the
  constructor total instead of panicking later at an unrelated call site.
  `test_zero_sector_count_is_clamped_to_one` drives all three rotating
  strategies past the threshold; with the fix reverted the test panics at
  `wear_leveling.rs:119`.

### Added (2026-09-23) — the agent's `flash_read` / `flash_write` tools really persist on device
- **The ESP32 tool handler now backs `flash_read` / `flash_write` with the same
  NVS store the Lua runtime uses.** The ESP32 has no user-writable
  memory-mapped NOR, so an *address* maps onto an NVS blob keyed
  `flash_<addr:08x>` — the scheme the Lua `hardware.flash_*` API has used all
  along. That store now lives in a new `firmware/esp32-app/src/nvs_flash.rs`
  module (namespace `magent_lua`, unchanged, so blobs written by earlier
  firmware stay readable) instead of being private to `lua_task.rs`, and both
  callers share it. `flash_write address=N,data=…` survives a reboot;
  `flash_read address=N,length=M` reads it back. `flash_read` on an address
  that was never written is an error, not a silent empty read. Values written
  by the agent are visible to a Lua app and vice versa (same store, distinct
  keys per address; the persisted `main.lua` uses a different key and is not
  reachable from a numeric address). `erase` has no agent-facing tool and is
  `#[allow(dead_code)]` for the no-`lua` build.

### Fixed (2026-09-23) — a real target fabricated tool success instead of reporting "not supported"
- **`ToolRegistry::execute` fell through to its simulated executors for any tool
  a real-hardware handler did not cover.** `set_handler` marks a registry as
  backed by real hardware (the ESP32 firmware installs `Esp32ToolHandler`), yet
  an uncovered tool still ran the simulation: the agent's `flash_write` answered
  *"Wrote 5 bytes to address 2048"* without persisting a byte, `ble_send`
  answered *"Sent 5 bytes via BLE"* on a firmware that has **no BLE stack at
  all** (removed outright in 2026-09-20), and `read_heart_rate` / `read_glucose`
  / `read_ecg` invented vital signs. The built-in tool list is what
  `build_llm_system_prompt` shows the model, so it would call these and relay the
  invented success to the user as fact. A handler-installed registry now returns
  `success: false` with `not implemented on this target` for a *registered* tool
  its handler did not claim; an unregistered name keeps the `MissingField`
  error, and a registry with no handler (host/tests) simulates exactly as
  before. `real_target_refuses_to_simulate_unhandled_hardware_tools` was
  verified to fail on the previous code (`flash_write must not report success`).

### Fixed (2026-09-23) — HAL flash `erase_sector` panicked instead of returning `OutOfRange`
- **`magent-hal`'s `EspFlash::erase_sector` and `NrfFlash::erase_sector` built
  the range `sector..end` with `sector > end` for a sector past the end of the
  region.** An address beyond the region floors to a `sector` at or past
  `data.len()`, so `end` (clamped to `data.len()`) was *smaller* than the range
  start: the `&mut self.data[sector..end]` loop panicked ("slice index starts at
  N but ends at M") in debug and release alike, despite the `Result` return
  advertising a clean `OutOfRange`. A caller that guards its own bounds and
  relies on the error arm — the reason the type returns `Result` at all — got a
  crash instead. Both now clamp `end` first and return `OutOfRange` when the
  sector start is not below it; a partial overlap (a region smaller than one
  sector) stays legal and touches only in-region bytes. Verified that
  `nrf52/sim.rs::SimulatedFlash::erase` — the in-repo reference implementation —
  already performed exactly this check, so the two adapters had drifted from it;
  no change was needed there. The new `erase_sector(8192)` / `erase_sector(u32::MAX)`
  assertions in `flash_bounds_erase_and_and_only` and
  `flash_roundtrip_erase_and_bounds` were both verified to fail on the previous
  code (panic at `esp32.rs:137` / `adapter.rs:188`) and pass after.

### Fixed (2026-09-23) — KV store scans ignored the region size; the test double hid it
- **`storage.rs` bounded three scans at a hardcoded 64 KiB instead of the
  region the store owns.** `get` and `get_stats` looped `while offset < 65_536`,
  and `get_stats` reported `free_space: 65536 - used_space`. `FlashStorage`'s
  capacity is whatever the device reports, so on a smaller region the scan walks
  past the end: once the store *fills* its capacity, the next header read faults
  and a plain lookup for an absent key returns `StorageReadFailed { address }` —
  a hardware fault — where the answer is simply "key absent". The same bound
  made `get_stats` fail on a full store, and `65536 - used_space` reported a
  nonsensical free space for any region that was not exactly 64 KiB (and would
  underflow, panicking under debug overflow checks, once a store exceeded
  64 KiB of live entries). All three now use `self.storage.capacity()`, matching
  what `set` and `collect_live` already did.
- **`MockFlash::read` clamped `end` before its own bounds check, making the
  check dead.** `let end = (start + buf.len()).min(self.data.len()); if end >
  self.data.len() { return Err }` can never take the error path, so an
  out-of-range read fell through to `copy_from_slice` with a short source slice
  and panicked. The double is what every storage test asserts against, and it
  was turning the production bug above into a test-double panic instead of the
  `StorageReadFailed` the real HAL returns. `end` is no longer clamped
  (`checked_add` + an honest bounds check).
- `KvStore::new` now documents its precondition: `base_address` must be
  sector-aligned and the region must end on a sector boundary. Compaction
  reclaims space by erasing whole sectors from `base_address / sector`, so an
  unaligned base address would erase flash *before* the store. Every current
  caller passes 0; the constraint is recorded because violating it destroys a
  neighbour's data silently. `lookup_on_a_full_store_is_a_clean_miss_not_a_read_fault`
  covers the full-store case (16 × 256-byte entries fill the 4096-byte mock
  exactly, so the cursor lands on capacity) and was verified to fail on the
  previous code with `StorageReadFailed { address: 4096 }`.

### Fixed (2026-09-23) — unsigned subtraction on interval gates wrapped when a counter rewound
- **`sports_coach.rs::start_session` reset the session clock but not the
  interval timestamps.** `update` gates its encouragement and breathing
  corrections on `session_duration_s - last_encouragement_s > 180` (and `> 30`).
  `start_session` set `session_duration_s` back to 0 while leaving both
  `last_*_s` fields at their end-of-previous-session values, so the first
  update of a *second* session subtracted a larger number from zero: an
  "attempt to subtract with overflow" panic under debug overflow checks, and in
  release a wrapped ~4e9-second elapsed time that fired an encouragement and a
  breathing correction the instant the session began. `start_session` now zeroes
  both timestamps and the two gates use `saturating_sub` as defence in depth.
  `restarting_a_session_resets_the_interval_timers` pins the restart behaviour
  (verified to fail on the previous code).
- **`voice_notification.rs::get_next_voice`'s rate limiter wrapped on a
  backward clock.** The 2 s cooldown was computed as
  `current_time_ms - last_speech_ms`; a `u32` millisecond counter wraps every
  ~49.7 days, and any caller that switched time sources would likewise step
  backwards. The subtraction then produced a huge "elapsed" value that cleared
  the cooldown instead of holding it (and panicked under debug overflow checks).
  Now `saturating_sub`, so a rewound clock reads as "no time has passed";
  `get_next_voice_holds_the_rate_limit_when_the_clock_goes_backwards` covers it.

### Fixed (2026-09-23) — budget and buffer guards accepted oversized requests
- **`safety.rs::BudgetEnforcer::consume_memory` and `MemoryGuard::allocate`
  evaluated their limit check as an unchecked `used + bytes`.** For a request
  near `usize::MAX` — exactly the shape a length read off a wire takes — the sum
  wraps below `memory_limit` / `capacity`, so the guard *accepted* the very
  allocation it exists to reject while the counter it just incremented had also
  wrapped. In a debug build the same expression panics on the overflow instead,
  turning a rejected request into a crash. Both checks now use checked
  arithmetic and treat overflow as "exceeds" (`checked_add` for the budget,
  `saturating_add` for the guard so the reported `attempted` stays meaningful).
  Added `budget_enforcer_rejects_overflowing_memory_request` and
  `memory_guard_rejects_overflowing_allocation`, both verified to fail on the
  previous code.
- **`simulator.rs::SimFlashStorage::erase_sector` panicked despite returning
  `Result`.** The bound was computed as `4096.min(65536 - start)`, which
  underflowed for any out-of-range sector; the loop then indexed `data` out of
  bounds. A sector that was wholly outside the region returned `Ok(())`
  having erased nothing, and one far outside panicked. It now rejects
  out-of-range sectors with `StorageWriteFailed { BadAddress }` and erases a
  valid sector whole (`sim_flash_storage_rejects_out_of_bounds` covers sector
  16, `u32::MAX`, and a valid sector 15).
- **`FaultDetector::with_defaults`'s comment claimed "10 errors before fault".**
  `report_error` compares the pre-increment count, so the fault is raised by the
  report *after* the threshold — the 11th with the default threshold. Comment
  corrected to match the behaviour the existing test already pins.

### Fixed (2026-09-23) — `AT+PING <hostname>` could target 0.0.0.0 instead of the host
- **`ping.rs::resolve_ipv4` read an IPv6 `sockaddr` as an IPv4 one.** It passed a
  null `hints` to `lwip_getaddrinfo`, which means `AF_UNSPEC`, so lwIP resolves
  with `NETCONN_DNS_IPV4_IPV6` and returns one `addrinfo` per address in
  resolver order. For a name with no A record the first (and only) entry is
  `AF_INET6`, whose `ai_addr` is a `sockaddr_in6`. The code cast it to
  `sockaddr_in` and took `sin_addr` — which in `sockaddr_in6` is
  `sin6_flowinfo`, a field `create_addrinfo` memsets to zero and never fills.
  The address came back as 0.0.0.0, so `AT+PING some-v6-only-host` silently
  targeted the wildcard address instead of reporting that it could not resolve
  an IPv4 one. Both `CONFIG_LWIP_IPV6=y` default configs (`sdkconfig.defaults`,
  `sdkconfig.c61.defaults`) reach this path; the S3 build sets it `=n`. Verified
  against `lwip/src/api/netdb.c` (`ai_family = AF_UNSPEC` for null hints;
  `create_addrinfo` sets `ai_family = AF_INET6` and `sin6_addr`) and
  `lwip/sockets.h` (`sin_addr` and `sin6_flowinfo` are both at offset 4). The
  inline comment had asserted the opposite — "for AF_INET it is actually
  `sockaddr_in`" — which is true only of an `AF_INET` entry, so the assumption
  was never checked. `hints.ai_family` is now `AF_INET`, which makes the cast
  correct by construction.
- **A name resolving to 0.0.0.0 was treated as a valid destination.** lwIP
  returns one when a name resolves to no non-zero address (the `ipaddr_cnt == 0`
  branch of `lwip_getaddrinfo`), and a DNS server answering `0.0.0.0` — the
  usual blocklist response — does the same. An address of zero is now reported
  as "no IPv4 address", so the caller answers `+CMDER:4` instead of pinging the
  wildcard address.
- No host test covers this: the path runs through lwIP's C resolver on the
  target, and the firmware's agreed verification depth is `cargo check`. The
  evidence is the two lwIP sources above plus the bindgen `sockaddr_in` /
  `sockaddr_in6` / `addrinfo` layouts.

### Fixed (2026-09-23) — `AT+CWLAP` replayed hostile SSIDs into the AT response unescaped
- **A nearby access point could forge a line in the `AT+CWLAP` response.** The
  scan cache is built from `ap.ssid.as_str()`, and an SSID is 32 bytes that
  anyone in radio range chooses — CR and LF included. `cwlap_line` replays that
  cache into a `\r\n`-framed response, so an SSID holding a lone `\r` ended the
  line early and the remainder was read by the client as its own response —
  e.g. an SSID of `evil\r+CME ERROR:99` forged a `+CME ERROR:99` line. Only the
  LF half was accidentally harmless: `str::lines()` splits on LF, so a `\n`
  never reached the wire, but a bare `\r` passes through it untouched. The
  firmware already had the remedy — `escape_wire`, applied to the
  `AT+CWJAP` SSID, the `AT+CWHOSTNAME` value, and the DHCP-supplied IP reported
  by `AT+IFCONFIG` — it just never reached the one field the *network* controls.
  The SSID is now escaped where it enters the cache, so both the reply and the
  `visible APs` log line are safe.
- **`escape_wire` could emit a dangling backslash that ate the frame
  terminator.** Its truncation check was per-character rather than per-escape,
  so when exactly one byte of the 128-byte buffer remained it wrote the
  backslash and then failed to write the character it escapes. The result is a
  value ending in a lone `\`, which an AT client reads as escaping the CR of
  the terminating `\r\n` — a truncated value breaking framing even though every
  character in it *had* been escaped. Reproduced with a 1-char prefix followed
  by 64 quotes; unreachable for today's callers (a 32-byte SSID escapes to at
  most 64 bytes) but reachable by construction. Truncation is now all-or-nothing
  per escape. `escape_wire` also moved from the firmware into
  `magent_core::at` — next to `build_response`, the framing it exists to
  protect, and where it can be unit-tested on the host.

### Fixed (2026-09-22, continued) — the firmware's agent web tools had no SSRF guard, and the guard itself was bypassable
- **`fetch_web` (and every other on-device web tool) would fetch any URL,
  including the device's own HTTP server, a LAN service, or the cloud-metadata
  endpoint.** The host's `magent_core::web::validate_fetch_url` refused
  loopback / link-local / RFC1918 targets, but the firmware had only a scheme
  check and a length cap. The URL here is *model-generated*, and the page text
  it pulls back re-enters the conversation as attacker-controlled instructions —
  so a fetched document could steer the next turn into reading a LAN endpoint
  and echoing the body out to the cloud LLM. The classifier now lives once, in
  `magent_core::at_validate::public_fetch_target_refusal`, and both the host
  tools and the firmware's `local_tools::http_get_once` choke point call it, so
  the two can no longer drift into disagreeing. `AT+HTTPGET` keeps unrestricted
  reach on purpose: its URL is typed by the operator, not the model.
- **`http://::1/` walked straight past the guard.** `url_host_port` split the
  authority on the *last* colon, turning the bare `::1` literal into host `":"`
  and port `1`. The socket layer then re-glued those into `::1` and connected to
  loopback, while the classifier was handed `":"` — which parses as neither v4
  nor v6 and was therefore waved through. A lone colon is not something the
  classifier *can* recognise, so the fix belongs in the parser: an authority
  carrying several colons without brackets is now kept whole. The parser moved
  to `magent_core::at_validate::parse_http_authority` (host-testable, shared
  with the host tools) with the firmware's `url_host_port` delegating to it.
  Tests pin both the parse (`("::1", 80)`, formerly `(":", 1)`) and the
  end-to-end pairing that matters — parser output feeding the classifier —
  and both fail without the fix.

### Fixed (2026-09-22, continued) — OTA leaked its handle and allowed concurrent updates
- **`firmware/esp32-app/src/ota.rs::perform_ota` leaked an ESP-IDF OTA ops entry
  on three error paths.** `esp_ota_begin` heap-allocates an `ota_ops_entry_t`
  (`calloc` in `esp_ota_init_entry`, inserted into a process-wide list) and, with
  `OTA_SIZE_UNKNOWN`, erases the whole inactive slot *before* any network work.
  Three failure paths then returned without releasing it: `EspHttpConnection::new`,
  `client.request`, and `request.submit` — i.e. a bad URL, DNS failure, or an
  unreachable host. The module docstring claimed "every failure path either
  aborts the OTA handle … or returns without touching the boot partition", so the
  intent was right and only those paths were missed. Retrying a broken OTA URL
  (the natural operator response) leaked one entry per attempt. All pre-`esp_ota_end`
  paths now go through `abort_handle`. `esp_ota_end` must **not** be followed by an
  abort: per `esp_ota_ops.c` it `free()`s the entry on every return path, including
  verification failure, so aborting there would double-free.
- **Nothing prevented two concurrent OTAs.** `AT+OTA` spawns a worker thread per
  command, and `esp_ota_begin` only rejects a conflict with the *running*
  partition — a second handle on the same inactive slot is granted. Two commands
  would interleave two writers into one image (each having erased the slot), and
  whichever finished first would `esp_ota_set_boot_partition` + `esp_restart`
  into an image the other was still writing. `OTA_IN_PROGRESS` (an `AtomicBool`
  claimed via `compare_exchange`, released by an `OtaSession` guard's `Drop`)
  refuses the overlap with an explicit error. On success the guard is never
  dropped because `esp_restart` does not return — correct, the process is going
  away. Verified against the ESP-IDF source at
  `~/.espressif/esp-idf/components/app_update/esp_ota_ops.c`.
- **Observed but deliberately not changed:** `ota_dispatch` replies
  `+OTA:STARTED` before the worker runs, so a URL that `perform_ota` will
  immediately reject still reports success to the caller. The async ack is the
  documented `AT+OTA` contract (worker thread + immediate reply) and moving the
  validation into the dispatcher would leave direct `perform_ota` callers
  unguarded; noted here rather than changed unilaterally.

### Fixed (2026-09-22, continued) — corrupt `value_len` in the KV header panicked the scan
- **`magent-core/src/storage.rs::parse_kv_header` validated `key_len` but not
  `value_len`.** The latter is a full `u16` read straight out of flash, so a
  corrupt-but-plausible header (valid 1–32 byte key, garbage value length)
  produced an `entry_size` up to `3 + 32 + 65535 + 2 = 65572`. `collect_live`
  and `get_stats` slice their 512-byte scan buffers with that length —
  `&mut buf[..entry_size]` and `&buf[3 + key_len..3 + key_len + value_len]` —
  so on corrupt flash they panicked with "range end index out of range" instead
  of surfacing `StorageError::CorruptedData`. `get` had a local
  `value_len > 256` guard; the other two call sites had none. The cap now lives
  in `parse_kv_header` (returning `None`, the same policy as a corrupt
  `key_len`), which bounds `entry_size` to `293` for every caller. Since `set`
  already rejects values over 256 bytes, an over-long `value_len` is always
  corruption — the now-redundant `get` guard was removed. `MAX_VALUE_LEN`
  (256) replaces the scattered literals.
- **Three regression tests**, proven to fail without the fix (guards removed →
  `collect_live` panics at line 487 and `get_stats` at line 590, exactly the
  predicted slice panic): `get_stats_tolerates_corrupt_value_len_without_panicking`,
  `collect_live_tolerates_corrupt_value_len_without_panicking` (driven through
  `delete` → `compact`), and `parse_kv_header_rejects_value_len_above_max`
  (which also pins the boundary — exactly `MAX_VALUE_LEN` still parses).

### Fixed (2026-09-22, continued) — cron day-of-month and day-of-week were AND-combined
- **`cli/src/scheduler.rs::CronSpec::next_after` combined the two day fields
  with `&&`.** POSIX/vixie cron specifies the opposite: when *both* day fields
  are restricted (neither is `*`), a day matches if **either** matches — and
  only when one of them is `*` does the other alone govern. So
  `magent scheduler daemon --cron "0 0 1 * 1"` was documented in-tree as
  "00:00 on the 1st of every month, OR every Monday" but actually fired only on
  the 1st *when that date also happened to be a Monday* — silently skipping
  roughly 11 of every 12 monthly runs, with no error and no log. The matcher now
  implements the POSIX rule. The `doms.len() == 31` / `dows.len() == 7` checks
  are the "this field was `*`" test (`*` expands to the full range).
- **The existing test never checked the behaviour.** `cron_dom_or_dow_semantics`
  is named for the OR rule and its comment cites POSIX, but it only asserts the
  *parsed* `doms`/`dows` vectors — it never calls `next_after`, so it passed
  against the AND implementation. Two tests added that assert the scheduling
  result, anchored on independent dates (`date -u -r` verifies all three
  anchors are Mondays):
  `cron_dom_and_dow_are_or_combined_when_both_restricted` starts at
  2024-01-01T00:00:59Z and requires the next match to be 2024-01-08T00:00:00Z —
  the AND implementation returns 2024-04-01T00:00:00Z instead, verified by
  reverting the fix and watching the test fail with that exact value. The
  complement (dom `*` ⇒ dow alone governs) is pinned by
  `cron_star_dom_pins_day_of_week_to_the_dow_field`.
  Note the fixture's `:59` second is load-bearing, not arbitrary: `next_after`
  is second-precise, so starting one second past the minute returns the same
  matching minute's next second rather than advancing a day.

### Audited (2026-09-22, continued) — Lua `nvram`, AT validators, flash bounds
- **`host/lua-app/src/nvram.rs` (verified bounded, no finding).** The
  `get` / `read_all` scan cannot run away: every iteration goes through
  `entry_len`, which rejects `klen == 0`, `klen > MAX_KEY_LEN`,
  `vlen > MAX_VALUE_LEN` and `off + len > NVRAM_CAPACITY`, so the loop can only
  terminate at the `0xFF` end marker or with an error — never by overrunning the
  region. `rebuild` bounds-checks the serialised size *before* erasing, so a
  store that would not fit cannot destroy the existing one. Nothing is silently
  truncated: `validate_key` and the `MAX_VALUE_LEN` check both run before any
  write, and the two stored length bytes are `u8`s bounded to
  `MAX_KEY_LEN` / `MAX_VALUE_LEN`.
- **`magent-core/src/at_validate.rs` (no finding).** Every bounded write checks
  its length first and maps a failed `push_str` to `AtOutcome::error(8)`
  (`StringTooLong`) rather than truncating — which matters most for
  `AT+CWJAP=`, where a silently truncated SSID would associate the device with a
  *different* network. `AT+LLMCFG=`, `AT+HTTPGET=`, `AT+HOSTNAME=` and the
  flight-provisioning validator all follow the same check-then-push shape.
- **`magent_hal::nrf52::adapter::NrfFlash` (bounds sound).** `read` / `write`
  reject `address + len > data.len()` with `NrfError::OutOfRange`, and
  `erase_sector` clamps the sector end to `data.len()`. `day_of_week` in the
  scheduler was also checked against a known anchor (1970-01-01 = Thursday ⇒
  `dow == 4`) and is correct.

### Fixed (2026-09-22, continued) — the Lua sandbox accepted precompiled bytecode
- **`host/lua-app/src/vm.rs::LuaVm::run_script` let a script load as a
  *precompiled binary chunk* instead of being parsed.** mlua autodetects the
  chunk mode, so a `&str` whose contents begin with the Lua bytecode signature
  (`LUA_SIGNATURE` = `\x1bLua`) was handed to `lua_load` as bytecode. mlua's own
  `set_mode` docs spell out why that matters: *"Lua does not check the
  consistency of the code inside binary chunks. Running maliciously crafted
  bytecode can crash the interpreter."* That is precisely the threat the
  module's own doc comment claims to defend against ("an untrusted (or merely
  buggy) enterprise script must not be able to read/write the filesystem, spawn
  processes, load arbitrary modules, or hang the device") — the stdlib
  allow-list, the 512 KB heap cap and the instruction budget all operate on
  *parsed* code, and none of them inspects a bytecode blob. Fixed with
  `.set_mode(ChunkMode::Text)` at this one choke point, which every script
  reaches (`run_script` is what `AppRuntime::boot` and `reload` call).
  The old behaviour is *proven*, not inferred: with `set_mode` removed, the new
  test's 4-byte input `"\u{1b}Lua"` produces `bad binary format (truncated
  chunk)` — the loader took the binary branch — whereas with the fix it is
  rejected as `attempt to load a binary chunk (mode is 't')`. Covered by
  `precompiled_bytecode_chunk_is_rejected` in `host/lua-app/tests/lua_vm_tests.rs`.
- **Scope note:** this is a *host*-engine finding only. The firmware builds
  `magent-lua` with `default-features = false, features = ["piccolo"]`
  (`firmware/esp32-app/Cargo.toml:28`) and uses `PiccoloVm`, whose
  `run_script` compiles Lua *source text* to a prototype and has no bytecode
  loader at all — so the device was never exposed. `lua-run <file>` (host) and
  the host test/tool paths were. The stale doc comment on
  `firmware/esp32-app/src/lua_task.rs:701` said the on-device runtime "owns the
  sandboxed `mlua::Lua`", which is not true on that target; corrected while
  here.

### Audited (2026-09-22, continued) — web admin panel, ingress, DBO1/DBO2
- **`web_admin.rs` (read-only by construction, no finding).** Two `GET`
  handlers (`/` and `/api/status`) and no state-changing endpoint, so there is
  no CSRF or auth-bypass surface — only unauthenticated *disclosure* to the LAN.
  What it discloses is version, Wi-Fi state, IP, SSID, RSSI, heap, uptime and
  latency metrics: **no secrets** (no password, no device key, no API key). The
  string-valued fields that could be attacker-influenced — `ip` and `ssid` — are
  routed through `magent_core::escape::{html,json}`, `reason` through the fixed
  `reason_label` match, and everything else interpolated into the markup is
  numeric (`state: u8`, `rssi: i32`, `reason: u32`, heap/uptime) or a hardcoded
  channel name from `latency_metrics::report`. So the SSID is not an XSS or
  JSON-injection vector. Its lack of auth is therefore a telemetry-exposure
  question for the operator, not a credential leak.
- **`link_adapters.rs` / `communication/mqtt.rs` (no finding).** The "ingress"
  surface is physical: UART0, USB-serial, SPI, a GPIO button. `MqttAdapter` is
  host-only and in-memory — it does not speak the MQTT wire protocol and has no
  network listener, so there is no unauthenticated remote AT channel. There is
  no `TcpListener` / `TcpStream` / `UdpSocket` anywhere in `magent-core` or
  `cli`. `IngressMode::Signed` wraps external payloads in a device-signed
  envelope.
- **`web3` signature/payload binding (verified sound, already tested).**
  `verify_signed_message(signed, payload)` takes the payload as a *separate*
  argument rather than trusting `signed.payload` — the safe direction, since the
  verifier checks the bytes it will actually act on rather than the bytes the
  envelope claims. `verify_rejects_tampered_payload` and the `tampered_signer` /
  `tampered_sig` cases in `tests/web3_tests.rs` cover it. DBO2's open verifies
  the MAC **before** decrypting, with a constant-time compare.
- **New finding recorded in `docs/BACKLOG.md` (P2, not fixed):** the device
  identity seed — the root secret for the device DID/wallet — is sealed with
  DBO1, the pre-HKDF format that has **no integrity check**, so a bit-flip in
  the stored value silently yields a *different* identity with no error
  (`open_sealed_bytes` returns `Ok` for any well-formed input). The env-var
  provisioning path writes DBO1 too. Not remotely reachable (needs NVS write
  access), hence P2. The fix is a one-line swap to `wifi_pass_seal_v2::seal_str`
  — the read path already opens both formats — but it changes what is persisted
  for a security-critical secret on a path that cannot be validated on hardware
  here, so it is documented rather than applied. Full analysis, including why
  the DBO1 XOR construction does *not* leak confidentiality for the sizes in
  use (`pad period = lcm(32,12) = 96 > 64 = MAX_PLAINTEXT`), is in the backlog
  entry.

### Fixed (2026-09-22, continued) — a stable boot claimed a reset that may not have happened
- **`firmware/esp32-app/src/main.rs::mark_stable_boot` logged success
  unconditionally.** It is the crash-loop escape hatch: once the board has been
  up 60 s, it writes `0` over the NVS boot counter and logs "crash counter
  reset". The NVS write was discarded (`let _ = …`), so a failed write left the
  counter *high* — the next three quick reboots would still trip safe mode —
  while the log asserted the counter had been cleared. Its sibling
  `clear_at_safemode_flag` already checks the same call and warns. It now warns
  on failure and only claims the reset when the write succeeded. This is a
  diagnostics-accuracy fix, not a behaviour change: the counter is still
  best-effort, and a failed reset still means the *following* boot's
  `check_and_advance_crash_counter` reads the stale value it always would have.
- **Audited the other ignored-persistence sites and left them alone, now with a
  reason recorded in-tree.** `web3/wallet/esp32_nvs.rs::load_wallet` discards a
  failed `save_index` after updating `last_accessed` — genuinely advisory: the
  timestamp going stale must not fail a load whose keystore read succeeded, and
  the neighbouring write path *does* propagate the error. Comment added so the
  asymmetry with the `save_index(...)?` above it is deliberate rather than
  accidental. `web3/blockchain/esp32_http.rs:1227`'s `let _ =
  set_default_transport(t)` is inside a `#[test]`, not production code, and the
  test asserts the effect on the next line.

### Fixed (2026-09-22, continued) — `--at` timestamp parser panicked on non-ASCII
- **`cli/src/cli.rs::parse_rfc3339` aborted the process on a multi-byte
  character in the seconds field.** The parser slices fixed *byte* offsets
  (`[0..4]`, `[5..7]`, … `[17..19]`, then `[19..]`). Its separator check pins
  bytes 4/7/10/13/16 to ASCII, which makes `0..=17` boundary-safe as a side
  effect — but nothing constrains bytes 17–18. So
  `magent scheduler daemon --at 2026-08-11T09:00:日` put `17..19` inside the
  3-byte `日` and died with `end byte index 19 is not a char boundary; it is
  inside '日' (bytes 17..20 of string)`. Reachable from a plain CLI argument —
  a bad timestamp became a crash instead of an "invalid value" error. Added an
  `is_ascii()` check on the fixed-width prefix, which also guarantees offset 19
  is a boundary. Covered by
  `rfc3339_multibyte_in_time_fields_is_rejected_not_panicked`, plus the first
  tests this function has ever had (`rfc3339_offset_forms`, which pins the `Z`
  / space-separator / `±HH:MM` handling and the range rejections).

### Fixed (2026-09-22, continued) — a run that answered nothing reported success
- **`agent_runner::RealAgentRunner::get_result` fabricated `"Task completed"`.**
  The loop returns the last text-bearing assistant message. When *no* assistant
  message carried text it fell through to a canned `Ok("Task completed")`. That
  is reachable: every iteration can emit a tool call and nothing else, and the
  loop then stops on the iteration or tool-call budget. The caller saw a
  success string for a run that produced no answer — the same class of
  fabricated-success the `run()` LLM-error path was already changed to avoid.
  It now returns `Err` naming the budget that ran out, e.g.
  `run ended without a final answer: no assistant text was produced
  (iterations: 2/10, tool calls: 2/2)`. Covered by the regression test
  `run_tool_only_exhaustion_errors_instead_of_faking_success`.
- **`magent-core/tests/agent_tests.rs::test_multiple_sensor_reads` was asserting
  that fabrication.** Its `max_tool_calls: 3` is smaller than the simulator's
  own "monitor" script, which issues four tool calls (temperature, humidity,
  pressure, `ble_send`) before it will answer — so the script could never reach
  its terminal result and the test passed on the canned string. Budget raised to
  5 and the assertion tightened to the script's actual result
  (`"Environmental monitoring complete"`), so the test now checks what its name
  claims.

### Fixed (2026-09-22, continued) — `test-workspace.sh` could not go green
- **Step 1 (fmt) hard-failed on the repo's known, intentional drift.** The
  workspace carries hundreds of pre-existing rustfmt diffs — ~690 after
  stashing all working changes, i.e. committed, not local — and
  `.github/workflows/ci.yml` deliberately marks the same check
  `continue-on-error: true` ("the codebase is not yet rustfmt-normalized").
  Because the harness set `fail=1`, it reported ❌ on *every* run regardless of
  the code under test. Step 1 is now advisory (`⚠`, does not set `fail`), with a
  pointer to the CI comment. Normalizing the workspace is a separate,
  whole-tree commit and is not this script's job.
- **Step 6 (feature matrix) could not detect the known `nrf52` failure.**
  `set -o pipefail` is on, and `cargo check … 2>&1 | grep -q "invalid Mach-O
  section specifier"` returns **cargo's** exit status (101, since the check
  necessarily fails on macOS), not grep's. The `if` therefore always took the
  else branch and reported "failure reason does not match the known Mach-O
  limit" even though the message had matched. The output is now captured into a
  variable first, leaving only `printf` (always succeeds) and `grep` in the
  pipe.

### Fixed (2026-09-22, continued) — panic paths in three non-test helpers
- **`ollama.rs::parse_response` panicked on a non-ASCII reply with no trailing
  `","`.** When the content is the last field, the parser falls back to
  `json.len() - 2` as the slice end — a raw *byte* offset. A response ending in
  multi-byte text (any Chinese/Cyrillic/accented answer) puts it inside a code
  point and the slice aborts with "byte index N is not a char boundary". Clamped
  to a boundary; added a regression test. (The function is currently only
  reached from its own tests, so this is latent rather than live.)
- **`cli/runner.rs` panicked on an unreachable branch instead of falling back.**
  `DeepSeekClient::try_with_endpoint(...).expect("...")` relied on
  `resolve_api_key_with_config` having trimmed the key. Every path there does
  trim, so the branch is unreachable — but it was a panic whose safety depended
  on a distant helper's discipline. Replaced with the same graceful
  "simulated reasoning" fallback the missing-key branch above it uses.
- **`host/email-mcp/jsonrpc.rs` had an `expect` that was only safe because of an
  earlier guard.** `let id = req.id.clone(); if id.is_none() { return ... }` …
  `let id = id.expect("id was Some above")` — correct today, but any reordering
  of the guard turns a notification into a panic. Restructured to a
  `let Some(id) = ... else { ... }` binding.

### Verified (2026-09-22, continued) — feature-matrix sweep
- **ESP32-S3 firmware `cargo check` passes** with the `get_result` change above
  (`MCU=ESP32S3`, `sdkconfig.defaults`, `--no-default-features --features
  board-s3,wifi,uart`, 1m30s). `magent-core` compiles clean for `espidf`, so the
  new `Err` path does not disturb the firmware build. Not a substitute for
  on-hardware validation — the device was not attached.
- **`./test-workspace.sh` now exits 0 end-to-end** — the first time it can. All
  six steps run: fmt (advisory ⚠, by design), clippy on the default members,
  the default-member test suite, clippy + tests for the `web3,blockchain`-gated
  `magent` crate, and the 16-feature `--all-targets` matrix with `nrf52` skipped
  as the known macOS limitation. After the two harness fixes above, the `nrf52`
  probe reports `⊘` rather than `✗`, and the all-16-features row is green.
- Built `magent-core`, `magent`, `magent-lua`, and `mcp-tool-executor` with
  every declared feature in isolation (including `--all-targets`, which compiles
  the `#[cfg(test)]` modules — where the `web3` break above was hiding). All
  pass. The only failure is `--features nrf52`, which is the known macOS
  limitation (`nrf52840-pac`'s `#[link_section = ".vector_table.interrupts"]` is
  invalid Mach-O), not a code defect.

### Fixed (2026-09-22, continued) — `--features web3` without `std` did not compile
- **`magent-core` failed to build with `web3` but not `std`.** One test helper in
  `web3/blockchain/agent_tools.rs` collected into `std::vec::Vec`, but the crate
  is `#![no_std]` unconditionally and the `web3` feature does not imply `std`.
  This is not a theoretical combination: the **ESP32 firmware depends on
  `magent-core` with `default-features = false, features = [..., "web3", ...]`**
  (i.e. `web3` and no `std`), so any host-side `cargo test -p magent-core
  --features web3` — including the DBO2 seal suite — died at compile time.
  Changed to `alloc::vec::Vec`. `--features web3` now builds and runs 651 tests.
- **The DBO2 seal suite was therefore never exercised by the default test run.**
  `wifi_pass_seal_v2` is gated `#![cfg(feature = "web3")]`, the default test
  config does not enable `web3`, and the `web3` config did not compile — so its
  24 tests only ran once the above was fixed.

### Docs (2026-09-22, continued) — `docs/AT_COMMAND_REFERENCE.md` re-audited
- **Stale test counts and a renamed test.** The test plan claimed 22 DBO1 / 17
  DBO2 unit tests (actual: 24 / 24), "48 unit + 37 integration" for the parser
  (actual: 79 / 44), and "28 verbs" (the `AtOp` table has 37). It also cited
  `wifi_pass_seal_v2::tests::mac_tamper_returns_bad_mac`, which does not exist —
  the real name is `tampered_ciphertext_fails_mac`.
- **`AT+PING` was listed as "refused | needs ICMP"** in the §8 compatibility
  matrix, contradicting the command table 370 lines above it. It is implemented
  (`ping::ping_ipv4` via `esp_ping`, IPv4 + IPv6) and returns
  `+PING: reply=N rtt=Xus`.
- **Wrong DBO2 wire-format bounds.** §5.3 said `MAX_ENCODED_LEN = 253` with
  "ciphertext ≤ 192"; the real value is **189** (5 + 2*(12 + 64 + 16), from
  `MAX_PLAINTEXT = 64`). The algorithm block also stated
  `mac_key = HKDF-Expand(..., L = 16)`; `MAC_KEY_LEN` is **32** (the resulting
  HMAC tag is then truncated to 16).
- **§6.1 provisioning script never read the device.** It was
  `send() { printf "$1\r\n" > "$DEV"; sleep 0.1; }` — a bare redirect reopens the
  tty per write, so every reply was discarded and an error could not be caught.
  Rewritten with `stty` + a persistent `exec 3<>"$DEV"` and a `send` that
  asserts an expected reply, plus `AT+CWJAP?` / `AT+CWRECONNCFG?` read-back.
- **§6.3 claimed `AT+SAFEMODE=0` was needed to leave safe mode.** The
  `AT+SAFEMODE=1` flag is one-shot: `read_at_safemode_flag()` consumes it and
  `clear_at_safemode_flag()` writes `0` back after it is applied. Documented that
  auto-entry (≥ `CRASH_LOOP_THRESHOLD = 3` consecutive resets) is the *other*,
  sticky mechanism.
- Also corrected: the SSID length-cap row (33 **bytes**, not chars, per
  `CWJAP_SSID_MAX = 32`), the safe-mode row (`+CMDER:4`), and the footer review
  date.
- **Source doc-comment drift**: `wifi_pass_seal_v2.rs`'s module docs named the
  HKDF info strings as `"dbo2/cipher/v1"` / `"dbo2/mac/v1"`; the constants are
  `"magent/dbo2/cipher/v1"` / `"magent/dbo2/mac/v1"`.

### Fixed (2026-09-22, continued) — `tools/comprehensive-test.sh` could hang forever
- The two `curl` calls in the HTTP section had no `--max-time` /
  `--connect-timeout`. With no device on the network the script sat in the
  kernel's TCP timeout (minutes) looking like a deadlock. Bounded to 3 s
  connect / 5 s total; an offline board now fails promptly instead of hanging.

### Docs (2026-09-22) — `docs/DEPLOYMENT_GUIDE.md` was largely fabricated
- **Four `AT+` commands in the guide do not exist**: `AT+CIPSTA`, `AT+LOGLEVEL`,
  `AT+MEMINFO`, `AT+SLEEP`. They were lifted from Espressif's ESP-AT docs, which
  is a different firmware. Every `AT+` token in the file was diffed against the
  command table in `magent-core/src/at.rs`; the real equivalents are
  `AT+IFCONFIG` / `AT+CIPSTAMAC` (read-only, no static-IP setter),
  `AT+SYSLOG=<0..5>`, `AT+HEAP` / `AT+UPTIME`, and **nothing** (there is no
  runtime power-management AT interface).
- **Fabricated response formats.** `+GMR:` claimed a `git:`/date suffix the
  firmware does not emit (it is `+GMR:mAgent v<x> / AT v0.2 / <chip>`; there is
  no `vergen`, so no git hash or build date is embedded at all).
  `+WIFIPASSUPGRADE:DBO2` is really `CURRENT`/`LEGACY`/`NO_ENTRY`. OTA does not
  stream `+OTA:downloading/verifying/success` — it returns a single
  `+OTA:STARTED` and logs the rest. The `/api/status` example listed five field
  names that do not exist (`uptime_sec`, `free_heap`, `wifi_status`,
  `wifi_rssi`, plus an invented `cpu_temp`/`tasks_running`) and reported uptime
  in the wrong unit.
- **The NVS pre-provisioning recipe could never have worked.** It told
  operators to generate a `wifi_config.csv` with namespace `wifi` and keys
  `ssid`/`password`; the firmware reads namespace `magent`, keys `wifi_ssid` /
  `wifi_pass` (`NVS_KEY_*` in `main.rs`). A device provisioned that way would
  never associate. Also recorded that this path stores the password in
  plaintext (accepted as `LegacyPlaintext` on read) until
  `AT+WIFIPASSUPGRADE=1` re-seals it.
- **`AT+RESTORE` was described as an OTA rollback and as a way to exit safe
  mode.** It is neither: it erases the entire NVS partition — including the
  device identity — and reboots, irreversibly. Corrected, and documented that
  `AT+SAFEMODE=1` is a **one-shot flag consumed on the next boot** (it cannot
  exit a crash-loop-induced safe mode).
- **Three verification snippets only pretended to verify.** `verify-flash.sh`
  and the batch-flash loop wrote to the serial port, slept, and printed
  "Verification complete" / "All devices flashed successfully!" without ever
  reading a reply or checking an exit status — the same defect the CHANGELOG
  entry above records for `tools/comprehensive-test.sh`. Both now read back and
  assert on the real `+GMR:` / `+CWJAP?` / `+IDENT:` responses, and the flash
  loop collects each background job's status. Appendix A's checklist is now
  annotated with the command that actually proves each item.
- **ESP32 BLE claims were stale.** BLE was removed from the ESP32 firmware
  (`AT+BLE` returns error 9, the `ble_*.rs` sources are gone), but the platform
  matrix still advertised "WiFi 6 + BLE" / "WiFi + BLE".

### Fixed (2026-09-22, continued) — four more byte-slicing panics, and a flaky e2e test
- **`execute_voice_output` / `execute_send_notification` panicked on non-ASCII
  text.** Both did `let take = min(text.len(), room); result.push_str(&text[..take])`
  — `take` is a *byte* count, so an utterance longer than the 256-byte buffer
  whose cut landed inside a 3-byte character panicked the worker thread. The
  identical bug had already been fixed in `execute_read_sensor`; these two
  siblings were missed. Added one `clamp_utf8(&str, max_bytes)` helper and
  routed all three sites through it (which also removed the duplicated inline
  boundary scan).
- **The email MCP server panicked on any long non-English email.**
  `FullMessage::to_json` truncated with `&self.body[..MAX_BODY_CHARS]`, where
  `MAX_BODY_CHARS` is 8 KiB — an email body is the archetypal non-ASCII
  payload, so "message is long" turned into "connection died". Now clamps to a
  character boundary via `char_boundary_at`.
- **The Lua `string.match` shim panicked on an `init` index inside a code
  point.** `start = (init.max(1) as usize) - 1` is treated as a byte offset into
  a `&str`, so a script passing an index that lands mid-character panicked the
  VM. Now backs up to the previous character boundary (real Lua would index the
  raw byte and return garbage — strictly better than aborting).
- **`ollama_react_e2e` was sporadically failing** with "error sending request".
  The mock server sets the listener non-blocking so its accept loop can poll the
  stop flag — but on macOS/BSD an accepted socket **inherits** `O_NONBLOCK`, so
  `handle_client`'s first `read` could return `WouldBlock` before the client's
  request bytes arrived; the `Err(_) => return` arm treated that as fatal and
  closed the connection without replying. Reproduced and confirmed with a
  standalone `rustc` probe (non-blocking listener → `read` errors immediately;
  blocking listener → `read` waits), fixed by restoring blocking mode plus a
  5 s read timeout per accepted stream. Stress-verified 25/25 runs green.
- New regression tests: 3 in `magent-core/src/tools.rs` (including a long
  Chinese utterance), 2 in `host/email-mcp/src/imap_client.rs` (that crate had
  no tests at all), 1 in `host/lua-app/tests/piccolo_probe2.rs`.

### Fixed (2026-09-22) — UTF-8 truncation, a panic on config report, script honesty
- **Byte-sliced string truncation silently wiped non-ASCII payloads.** Four
  sites did `core::str::from_utf8(&bytes[..take]).unwrap_or("?")` to fit a
  `String` into a `heapless::String<N>`. Slicing at a raw byte offset splits a
  multi-byte code point, `from_utf8` then fails, and `unwrap_or("?")` throws
  away the **whole** payload — so a Chinese web preview or city name reaching
  the 255-byte `ToolResult::data` budget arrived at the agent as a single `"?"`.
  Replaced with `text_util::push_str_truncated`, which backs the cut off to the
  previous character boundary and keeps the prefix. (Sites:
  `local_tools.rs::into_result_data` — the one with real risk, since a
  `fetch_web` preview is up to 1024 bytes of arbitrary HTML/UTF-8 — plus the
  three label bridges in `link_adapters.rs`.)
- **`mask_key` panicked on a non-ASCII API key or token.** It sliced `&key[..4]`
  and `&key[b.len() - 4..]`, which panics with "byte index is not a char
  boundary" whenever either offset lands mid-character. Reachable from
  `AT+LLMCFG=?` / `AT+FLIGHTCFG=?` — i.e. a config *report* could reboot the
  device. Now counts and slices by character.
- **The trace renderer panicked on a non-ASCII LLM reply.**
  `agent_runner.rs`'s `TraceEvent::LlmResponse` arm did `&body[..200]`; a
  Chinese answer whose 200th byte fell inside a character panicked the process.
  `cli/src/runner.rs` already had the fix (`truncate_utf8`, with a regression
  test) — the same fixed bug was simply still live here. Added a local
  `floor_char_boundary` plus three tests.
- **Removed a dead `WifiManager::get_connection_quality`** (flagged by the C61
  build): `main.rs` deliberately takes *one* FFI RSSI read and shares it between
  the `AT+CWSTATE` snapshot and the quality grade, so the convenience method was
  unreachable.
- **`tools/comprehensive-test.sh` reported green regardless of the device.** Every
  serial "test" was `printf '…' > $PORT` followed by `test_result $?` — it wrote
  to the port and never read it, so the assertion could only fail if the *host*
  couldn't open the tty. On top of that, `set -e` plus `((pass_count++))` aborts
  the script at the first PASS (`((0++))` exits non-zero). It now opens the port
  read/write, sends a command, reads the reply and matches it against the real
  `+GMR:` / `+CWSTATE:` / `+CMDER:` formats; the JSON check no longer passes
  vacuously on empty input (`jq .` exits 0 on no input).
- **`tools/test-wifi-reconnect.sh` asserted success unconditionally.** It printed
  "✅ WiFi自动重连功能已实现" no matter what happened, showed sample log lines in
  green *as if observed* with the wrong subsystem tag (`[wifi-mgr] 信号质量:` —
  the firmware emits `[wifi-sup] ⚠️ 信号质量差:`), and quoted an invented
  "< 200 bytes" memory figure. Rewritten as an honest manual checklist: one real
  send/assert on `AT+CWSTATE`, then the observation steps with the strings the
  code actually logs (each verified against `main.rs` / `wifi_manager.rs`).
- **`test-workspace.sh` masked failures and skipped a whole feature surface.**
  It ran `cargo test … 2>/dev/null || echo "跳过"`, so a genuine failure printed
  "跳过" (skipped) and exit code stayed 0; it also invoked `-p host`, a package
  that does not exist. Rewritten to use the default members, track a real `fail`
  flag, and exit 1 with an honest banner. Added a step for
  `cargo clippy -p magent --features web3,blockchain --all-targets -- -D warnings`
  — the existing gate only ever lints **default** features, and `web3` /
  `blockchain` are `default = []`, so their code was never compiled by CI.
  Running the new gate surfaced three dead imports and one clippy lint
  (`field_reassign_with_default` in `parse_web3_sign_flags`), all fixed:
  test-only imports in `runner.rs` / `web3_blockchain.rs` moved inside their
  `#[cfg(test)]` scopes, and `Web3SignFlags::default()` now uses the
  struct-literal form.
- **`docs/QUICKSTART.md` web3 section omitted the vault passphrase.** The vault
  is encrypted, so `magent web3 new alice` fails outright without it — the
  guide now exports `MAGENT_WEB3_PASSPHRASE` (or `--passphrase-env <VAR>`).
  Verified: `web3 did` and `web3 list` are read-only and do **not** need it.

### Fixed (2026-09-21, continued)
- **CI never ran at all: `.github/workflows/ci.yml` was invalid YAML.** The
  `firmware-esp32c61` job's `actions/cache` step had two stray paths
  (`/root/.cargo/registry`, `  /root/.cargo/git`) pasted under the `path:` block
  but at the wrong indent — the block scalar terminated early and the following
  line parsed as a mapping key with no `:`, aborting the whole document
  (`could not find expected ':'`). GitHub Actions rejects an unparseable
  workflow outright, so **every job** (host, nRF52, C61) was dead, which is why
  the C61 build break went unnoticed for as long as it did. The two lines were
  redundant — the runner's `HOME` is `/root` inside the `idf-rust` container, so
  the `~/.cargo/registry` entry already covers them. Removed; the file now
  parses with all three jobs intact.
- **`build-c61.sh` could corrupt the tracked `sdkconfig.defaults`.** The
  script saves the S3 config to a *fixed* path
  (`/tmp/sdkconfig.defaults.s3.backup`) before swapping in the C61 file, and
  restores from it in an `EXIT` trap. Two concurrent builds — a CI matrix, or
  two terminals — clobber each other's backup, and the trap then copies the
  wrong contents back over the tracked file, silently replacing the S3 config
  with the C61 one. The backup is now a per-run `mktemp` file, removed by the
  trap.
- **`main.rs` had an un-compilable `#[cfg(not(feature = "wifi"))]` arm.**
  `mod wifi_manager;` was gated on `#[cfg(feature = "wifi")]`, but the Wi-Fi
  supervisor's no-wifi fallback arms name `ConnectionQuality` from that module.
  Dead in every real build (the `wifi` feature is in `default` and no tracked
  build disables it), but it would not compile if one ever did. The module is
  now compiled unconditionally — it depends only on `esp-idf-svc`, which is an
  unconditional dependency.
- **Removed a misleading test helper** in `magent-core/src/at_validate.rs`:
  `make_oversized(n, fill)` ignored both arguments and always returned a
  96-byte buffer, so its "smoke test" asserted nothing about its name.

### Changed (2026-09-21) — the ReAct loop no longer fabricates LLM replies
- **`RealAgentRunner` silently substituted a canned "simulated" answer for any
  failed LLM call.** Both failure points did it: a backend that failed the
  reachability probe at the start of `run()`, and a mid-run `chat_with_messages`
  error (link drop, timeout, HTTP 5xx). Each fell through to
  `generate_simulated_response()`, so a caller could not tell a fabricated
  scripted result from a real model reply — the same hazard class as the
  `encrypt()`/`decrypt()` pass-through fixed in the 2026-09-20 security pass.
  `run()` already returns `Result`, so both now propagate:
  - probe failure → `Err("LLM backend unreachable (…)")`;
  - mid-run failure → `Err("LLM backend error: …")`.
- **Simulation is now an explicit opt-in**, via the new
  `RealAgentRunner::without_backend()`: it drops the backend so every response
  comes from the built-in scripted responder. This matters because
  `RealAgentRunner::new()`/`with_config()` wire an `OllamaClient` at
  `http://localhost:11434` by default — the previous "simulation mode" was in
  practice just that backend failing to probe. Demos and tests that want the
  scripted responder now ask for it (`agent_tests.rs`,
  `nrf52_comprehensive_tests.rs`).
- **Not changed:** `MiniAgent` (the firmware's agent, `magent-core/src/agent.rs`)
  keeps its heuristic fallback when the LLM errors. That path is a documented,
  logged, back-off-bounded offline degradation that executes a *real* local tool
  call — it does not emit a fabricated answer dressed up as model output.

### Fixed (2026-09-21)
- **The ESP32-C61 firmware did not build.** `./build-c61.sh` failed with 8
  errors — `cannot find type ip6_addr`, `cannot find function ip6addr_aton`,
  `cannot find struct ip_addr` / `ip_addr__bindgen_ty_1`, all in `ping.rs`.
  `sdkconfig.c61.defaults` sets `CONFIG_LWIP_IPV6=n` while `sdkconfig.defaults`
  (S3) sets `=y`, and `ping.rs` used those bindgen symbols unconditionally; they
  are only generated when lwIP IPv6 is on. The C61 is the **default** board
  (`default = ["wifi", "uart", "board-c61"]`), so this was the primary target.
  Fixed in both directions, as the two configurations are both legitimate:
  - `sdkconfig.c61.defaults` now sets `CONFIG_LWIP_IPV6=y`, restoring IPv6
    `AT+PING` parity with the S3.
  - `ping.rs`'s IPv6 path is now gated on a new `ipv6_ping` cfg emitted by
    `build.rs` (which already read the effective sdkconfig for `rt_wdt`), so an
    IPv6-less build still compiles: `resolve_ip6` and the `ip_addr` union
    construction are `#[cfg(ipv6_ping)]`, and an IPv6 literal answers
    `+CMDER:9` instead of being mis-parsed as IPv4. Without the gate the
    non-IPv6 shape of `target_addr` (a bare `ip4_addr_t`, not the tagged
    `ip_addr` union) cannot be constructed at all.
- **CI could not have caught it.** The `firmware-esp32c61` job ran a bare
  `cargo build --release` with only `MCU=ESP32C61` — it never performed the
  `sdkconfig.c61.defaults` → `sdkconfig.defaults` swap that `build-c61.sh` does,
  so it compiled the C61 against the **S3** config (`IPV6=y`) and stayed green
  while the real local build was broken. The job now invokes `./build-c61.sh`,
  so CI and local builds cannot diverge again.

### Security (2026-09-20 P0 pass)
- **TLS certificate verification was off in every default build**:
  `sdkconfig.defaults`, `sdkconfig.c61.defaults` and `sdkconfig.s3.defaults`
  shipped `CONFIG_ESP_TLS_INSECURE=y` with no
  `CONFIG_ESP_TLS_SKIP_SERVER_CERT_VERIFY`, so the LLM API key, OTA images and
  `AT+HTTPGET` were all exposed to MITM. Both are now `=n`. Confirmed effective:
  the generated C61 `sdkconfig.debug` reports
  `# CONFIG_ESP_TLS_INSECURE is not set`.
- **`build-s3.sh` leaked Wi-Fi credentials in a tracked file**: it exported
  `MAGENT_WIFI_SSID` / `MAGENT_WIFI_PASS` literals (committed in `a823f04`).
  Removed — they must come from the environment. **The committed passphrase is
  still in git history and must be rotated; history rewriting is a separate,
  owner-approved step.**
- **`magent-core/src/security.rs` fail-closed**: on `no_std` builds without
  `web3`, `encrypt` / `decrypt` / `generate_auth_tag` returned the **input
  unchanged** (or a tag synthesized by hex-encoding the plaintext — i.e. not a
  MAC, so `verify_auth_tag` accepted forged data). They now return
  `AgentError::CryptoError`. The XOR simulator is kept only for the `std`
  test path.
### Removed (2026-09-20)
- **BLE deleted from the firmware.** The Bluedroid controller requires ~244 KB
  of *contiguous DMA-capable internal DRAM* (`MALLOC_CAP_8BIT|DMA|INTERNAL`;
  PSRAM cannot back it) and neither target can supply that alongside Wi-Fi plus
  the agent threads — the S3 fails `esp_bt_controller_init` with 257
  `ESP_ERR_NO_MEM` at ~244–253 KB free, and the C61 (~134 KiB total) asserts in
  `btm_ble_init` once Wi-Fi has taken its share. `CONFIG_BT_ALLOCATION_FROM_SPIRAM_FIRST`
  and disabling BLE 5.0 did not free enough, and the S3's Bluedroid controller
  exposes no `BTDM_CONTROLLER_*` Kconfig to shrink it.
  Removed: `src/ble_config.rs`, `src/ble_at.rs`, `src/ble_wallet.rs`,
  `src/ble_gatt.rs`, the `ble` cargo feature, the `#[cfg(feature = "ble")]`
  paths in `main.rs` / `at_dispatch.rs` / `lua_task.rs`, and `CONFIG_BT_*=y` in
  `sdkconfig.c61.defaults` / `sdkconfig.s3.defaults`.
  Behavioural surface: `AT+BLE=…` is still *parsed* and uniformly answers
  `+CMDER:9` (never a misleading `OK`); the Lua `hardware.ble_send` always
  returns `Err` rather than silently dropping the payload. Provisioning and
  command channels are UART0 (`AT+CWJAP=`, `AT+LLMCFG=`) and the web admin panel.
  See the *BLE removed outright* entry in `docs/BACKLOG.md`.
- **Correction to an earlier note in this file.** The change filed for the
  2026-09-20 pass claimed `--features ble` no longer compiled and attributed it
  to an ESP-IDF 5.5.5 regression. That was an **invocation error, not a
  regression**: `build-c61.sh` swaps `sdkconfig.c61.defaults` into
  `sdkconfig.defaults` before invoking cargo (the pio driver only reads the
  standard filename), and the failing probes ran a bare `cargo check --features
  ble` — which read the *S3* file, where `CONFIG_BT_ENABLED=n`, so bindgen
  emitted no BLE symbols. Any BLE work claimed in this pass was superseded by
  the removal above.

### Fixed (2026-09-20 P0 pass)
- **nRF52840: `static_mut_refs` soundness bug + 67 build warnings → 0.** `main.rs`
  accessed four `static mut` globals (`WATCHDOG`, `BLE_STATE`, `POWER_STATE`,
  `AGENT`) through `unsafe` blocks justified only by a comment that Embassy
  serialises the two tasks on one core. Forming `&`/`&mut` to a `static mut` is
  the aliasing hazard Rust forbids, and the invariant would have broken silently
  under a second core or a second `main_task` spawn. Replaced with
  `Mutex<CriticalSectionRawMutex, RefCell<T>>` (interrupt-masking `lock()`), so
  the compiler enforces what the comment asserted; no borrow is held across an
  `.await`. `MiniAgent` holds deliberately-`!Send` `dyn LlmBackend` /
  `dyn ToolHandler`, so its slot uses one `AgentSlot` newtype with a documented
  `unsafe impl Send + Sync` (single-core justification) — one auditable
  assertion replacing five unsafe access sites. The remaining 57 warnings were
  all scaffold dead-code in `ble.rs`/`watchdog.rs`/`sensors.rs`/`power.rs` (none
  in `main.rs`), silenced with a module-scoped `#![allow(dead_code)]` so the live
  path stays strict. See the *nRF52840 scaffold warning debt* entry in
  `docs/BACKLOG.md`.
- **`AT+CWMODE=2` / `=3` reported `OK` but silently disabled Wi-Fi on the next
  boot**: the dispatcher persisted the mode to `mag_at:wifi_mode`, and
  `setup_platform` then refused to bring the radio up for any mode other than 1
  — so the device came up with no Wi-Fi and no explanation. AP / dual-mode are
  unimplemented, so the SET now fails fast with `+CMDER:7`. `AT+CWMODE?` and
  `AT+CWMODE=1` are unchanged.
- **`AT+CWQAP` was a no-op and `AT+CWLAP` could stall**: `AT+CWQAP` now raises a
  flag the Wi-Fi supervisor consumes on its next tick (disconnect + suppress the
  auto-reconnect path until the next `AT+CWJAP=`, otherwise `if !connected`
  re-associated within one tick). `AT+CWLAP` raises a scan-request flag that
  bypasses the 60 s opportunistic-scan throttle, so it returns live results
  instead of a stale cache or a permanent `+CWLAP:scan-started`.
- **`WifiManager::get_rssi` passed the wrong pointer type**: `esp_wifi_sta_get_rssi`
  takes `*mut c_int` (4 bytes); the code declared `i8` and passed `*mut i8`, so a
  4-byte store went through a 1-byte local. Found by the first cross-compile —
  `wifi_manager.rs` had never been compiled. Dead `WifiManager::connect()` (never
  called; `main` configures the radio itself) and its now-unused
  `WifiManagerConfig::ssid` / `password` fields removed, so the manager no longer
  retains a copy of the WPA2 passphrase.
- **`build-s3-no-ble.sh` + `sdkconfig.s3-no-ble.defaults` deleted.** They were
  both redundant and actively hazardous, and are recoverable from
  `/tmp/build-s3-no-ble.sh` + `/tmp/sdkconfig.s3-no-ble.defaults` (and were
  never tracked by git).
  *Redundant*: the pair existed only to build an S3 image with BLE off, which
  `build-s3.sh` now does unconditionally — BLE was removed from the firmware
  entirely on 2026-09-20.
  *Hazardous*: `build-s3-no-ble.sh` **overwrites the tracked
  `sdkconfig.defaults`** with its own config for the duration of the build. That
  config, read as a whole, is a stale snapshot that contradicts several
  documented, deliberate decisions in the file it replaces:
  - `CONFIG_ESP_MAIN_TASK_STACK_SIZE=8192` (vs the 16384 that
    `sdkconfig.defaults` documents as necessary — 8 KiB "triggered a
    stack-protection fault right after boot phase 7/8");
  - `CONFIG_ESP_TASK_WDT_INIT=y` (vs the deliberate `n` that keeps ESP-IDF from
    auto-subscribing the IDLE-in-WFI main task and tripping the TWDT);
  - `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` (which `sdkconfig.defaults`
    documents as a **build failure** against the default partition table via
    `check_table_contents`).
  It also omitted `CONFIG_PARTITION_TABLE_CUSTOM` / `_CUSTOM_FILENAME` outright
  — so a build through that script would have produced a firmware with the
  *default* partition table, i.e. not the layout `flash-s3.sh` writes.
  Nothing was merged: every divergent value was a documented regression or a
  direct contradiction, so there was no safe subset to carry over.
- **`WifiManager::get_rssi` reported a failed read as the best possible
  signal**: it returned `0` on error, and `ConnectionQuality::from_rssi(0)`
  grades `0` as `Excellent` (`> -50`). So whenever `esp_wifi_sta_get_rssi`
  failed — e.g. the STA was not associated — the supervisor logged "signal
  excellent" and, critically, `should_warn()` never fired, silently suppressing
  the weak-signal warning. `get_rssi` now returns `Option<i32>` (`None` on
  error) and a new `ConnectionQuality::Unknown` carries that case; the
  supervisor reads the RSSI **once** per tick and derives the grade from that
  value instead of issuing a second FFI call. `AT+CWSTATE` keeps `0` as its
  "unknown" sentinel.
- **Deleted two dead files** that were never declared as modules and so were
  never compiled: `firmware/esp32-app/src/wifi_manager_stub.rs` and
  `wifi_manager_tests.rs` (532 lines combined).
- **README overclaims corrected**: the Security section claimed hardware-backed
  encryption and BLE security that the firmware does not implement
  (`SecurityManager` is not instantiated in firmware; `WearLeveler` is not
  wired up). The nRF52840 row is now marked "compiles, scaffold only" with the
  specific gaps listed (`agent_task` never drives `MiniAgent`, BLE calls only
  log, no SAADC, hardware WDT never fed).

**Verification for this pass** (compile/host only — no hardware): `cargo test`
green (1978 passed, 0 failed) and `cargo clippy --all-targets -- -D warnings`
clean, both over the workspace's `default-members` (the firmware crates are
excluded on purpose: `nrf52840-pac`'s `#[link_section = ".vector_table…"]` is
invalid in Mach-O, so `--workspace` cannot build on a macOS host); `cargo check`
clean for ESP32-C61 (default features) and ESP32-S3 (`--target
xtensa-esp32s3-espidf --no-default-features --features board-s3,wifi,uart`).
There is no `ble` feature left to check.

### Fixed
- **`AT+PING` always answered `+CMDER:6` (2026-09-15)**: `ping.rs` built its
  config from `esp_idf_sys::esp_ping_config_t::default()`, and the bindgen
  `Default` is a **zeroed** struct — the C `ESP_PING_DEFAULT_CONFIG()` macro also
  fills in `data_size`, `ttl`, `task_stack_size` and `task_prio`. With
  `task_stack_size == 0`, `esp_ping_new_session` calls
  `xTaskCreate(..., 0, ..., 0, ...)`, which always fails
  (`E ping_sock: esp_ping_new_session(228): create ping task failed`) and every
  ping collapsed into `+CMDER:6` — indistinguishable from "the host did not
  reply", and reproduced with 125 KB internal DRAM free, so it was **not** a
  memory issue. The IDF macro's values are now set explicitly, and the two
  failure paths log the underlying `rc` plus the reply/RTT counters. Verified on
  hardware: `AT+PING=<own ip>` → `+PING: reply=4`, `AT+PING=8.8.8.8` →
  `+PING: reply=4 rtt=203us`.
- **`AT+CWLAP` silently returned an empty list (2026-09-15)**: the reply line is
  bounded by `REPLY_LINE_MAX` (256 B) and a single `write!` of
  `"+CWLAP:{cached}"` writes **nothing** for an argument that does not fit, so a
  20-entry cached scan emitted a bare `+CWLAP:` — the list was invisible exactly
  when an operator needs it (diagnosing an AP that is not in range). The reply is
  now built entry-by-entry, emitting as many as fit and marking truncation with
  `...`:
  `+CWLAP:(arksong,-43);(TJ-WIFI,-46);…;...`.
- **S3 flash wrote a stale partition table** — see the entry below this block.

### Added
- **`get_flight_price` tool + `AT+FLIGHTCFG=` (2026-09-16)**: the agent can now
  answer "最便宜机票" questions. The device has **no keyless flight-price source**
  (surveyed *from the board*: Travelpayouts → `401` without a token,
  `flights.ctrip.com` → `432 whaleguard block`, `api.skypicker.com` → URL
  unparseable, `r.jina.ai` / `tianxun.com` → unreachable), so the feature is
  key-driven exactly like the LLM backend:
    - `AT+FLIGHTCFG=<provider>,<token>` / `AT+FLIGHTCFG?` (new `AtOp::FlightCfg`
      in `magent-core/src/at.rs`, validator `validate_flightcfg_set` in
      `at_validate.rs`: provider must be `travelpayouts` or `kiwi`
      case-insensitively, token ≤128 B, no whitespace/control bytes; the reply
      masks the token as `+FLIGHTCFG:travelpayouts,abcd…wxyz`).
    - `local_tools::get_flight_price` (`from=<IATA>,to=<IATA>[,date=YYYY-MM]
      [,limit=1-5]`): Travelpayouts `prices_for_dates` (token as a query param)
      or Kiwi Tequila `v2/search` (key in an `apikey` header — the shared HTTP
      helper gained optional extra headers for this). Fares are rendered
      compactly (`CNY1350 2026-10-01 MU 直飞; `) within the 256-byte tool-result
      budget, and only whole entries are emitted.
    - Verified on hardware: the model maps "查询上海到西藏的最便宜的机票价格" to
      `get_flight_price args=from=SHA,to=LXA` and, with no provider configured,
      the tool answers `no flight provider configured — run
      AT+FLIGHTCFG=<provider>,<token>` (no invented prices).

### Fixed
- **A tool with an over-long description silently vanished from the LLM prompt
  (2026-09-16)**: `Tool::description` is a `heapless::String<128>`, and the
  registration path used `String::<128>::try_from(desc).unwrap_or_default()` —
  so a 148-byte description became an **empty string**, and
  `ToolRegistry::describe()` skips empty descriptions. The model therefore never
  saw `get_flight_price` and truthfully answered "I don't have that tool" (this
  was mis-diagnosed twice: the model's own recitation of the list is truncated by
  `max_tokens=256`, so it is not evidence of prompt content). Registration now
  reports the overflow (`log::error!`) instead of defaulting to empty.
- **`ToolRegistry::describe()` dropped the tail of the tool list (2026-09-16)**:
  capacity raised 1024 → 2048 B, entries are committed only if they fit whole,
  and any omission is logged (`[tools] describe(): N tool(s) omitted …`) — the
  prompt is the model's only view of its capabilities, so it must never silently
  lose a tool. `build_llm_system_prompt` likewise warns if the tool list does not
  fit its `MAX_BUFFER_SIZE` buffer.
- **Boot-time tool inventory log (2026-09-16)**: the agent now logs the exact
  "Available tools:" list it hands to the LLM once per boot
  (`[agent] LLM tool inventory (14 tools): …`), so "why doesn't the model use
  tool X?" is answerable from the console.

  the firmware's `Esp32ToolHandler` had always implemented `fetch_web`, but the
  agent's *tool registry* — the list `MiniAgent::build_llm_system_prompt` hands
  to the model — never included it, so the cloud LLM had no idea the device
  could reach the network and answered weather/news from (stale) parametric
  memory. On-device network tools + their registration:
    - `firmware/esp32-app/src/main.rs`: registers `fetch_web`, `get_weather`
      and `get_news` (`ToolType::WebFetch`) with the argument grammar in the
      description, so the LLM can actually pick them.
    - `firmware/esp32-app/src/local_tools.rs`: shared bounded
      `http_get_bounded()` helper (DNS/TCP preflight + CA bundle + 6 s timeout +
      bounded read) with **one bounded retry** (a phone-hotspot uplink drops the
      occasional TLS handshake: `mbedtls_ssl_handshake returned -0x0050`, ~1 in 4
      attempts observed), plus the two new tools:
      `get_weather city=<name>` (wttr.in one-liner, no API key) and
      `get_news limit=<n>` (60s digest JSON, `data.news` parsed with serde_json,
      output truncated to the 256-byte tool-result budget). Body preview raised
      256 B → 1024 B. Cities are percent-encoded before they go on the wire.
    - `magent-core/src/tools.rs`: new `ToolType::WebFetch` (no simulation —
      returns `ConfigError::NotConfigured` if a target advertises a web tool
      without a real handler).
    - Verified on hardware with a DeepSeek key + live hotspot:
      `北京天气` → `+WEATHER:200 Beijing: ✨ +21°C`；`上海今天天气怎么样？` →
      `+WEATHER:200 Shanghai: ☁️ +25°C`；`深圳天气` → `+WEATHER:200 深圳: ☀️ +28°C`；
      `用 get_news 给我今天的新闻头条` → `+NEWS:1. 8 月 70 城房价出炉…`；
      `把 GPIO 13 拉高` → `GPIO13 set to high (err=0)`；闲聊 →
      LLM-written prose (`RESULT[...]: 你好！我是 mAgent…`).
      Full re-run on 2026-09-16: agent battery **8/8** (including the post-reboot
      first task, which now uses the model), `tools/s3_full_test.py` **33/33**,
      `tools/s3_tls_soak.py` **5/5**; the retry fix is directly visible in the
      log — `mbedtls_ssl_handshake returned -0x0050` on attempt 1 followed by a
      `200` on attempt 2 (previously that turn ended in `Task: Tool error:`).
      Device-side metrics: free heap 2.14 MB (low-water 2.08 MB), `e2e_agent`
      WCET **23.2 s** (LLM ≤10 s + network tool ~15 s), `llm_rt` avg 5.2 s.

### Fixed
- **`fetch_web` rejected any URL with non-ASCII characters (2026-09-15)**: the
  model emits URLs containing raw UTF-8 for Chinese questions
  (`fetch_web url=https://www.google.com/search?q=上海到西藏最便宜机票价格`), which
  `esp_http_client` refuses (`HTTP_CLIENT: Error parse url …` →
  `ESP_ERR_INVALID_ARG`) — the same class of failure that was fixed for
  `get_weather`, but on the generic path. `fetch_web` now percent-encodes the
  URL (`encode_url_preserving_reserved`: URL structure preserved, non-ASCII /
  spaces / control bytes escaped, existing `%` escapes left alone so nothing is
  double-encoded). Verified: `fetch_web url=https://wttr.in/上海?format=3` →
  `+FETCH:200 bytes=24 preview=上海: 🌤️ +27°C` (previously `Task: Tool error:
  request: ESP_ERR_INVALID_ARG`).
- **Agent could fabricate live data / answered in the wrong language
  (2026-09-15)**: the system prompt now (a) requires the model to answer in the
  language the user wrote in, and (b) forbids inventing live data — "Never
  invent live data (prices, availability, stock quotes, weather, news): if a
  tool could not fetch it, say plainly that you could not retrieve it and what
  you tried". Verified with a request the device genuinely cannot serve
  ("查询上海到西藏的最便宜的机票价格"): the agent answered in Chinese that it has
  no flight/booking data source and cannot obtain a live fare, instead of
  quoting a made-up price. (Data-source survey from the board: Travelpayouts →
  `401 Unauthorized`, `flights.ctrip.com` → `432 whaleguard block`,
  `api.skypicker.com` → unparseable URL, `r.jina.ai` / `tianxun.com` →
  unreachable, so no keyless flight feed exists on this network.)

  agent loop feeds itself the internal heartbeat task `read sensor temperature`
  whenever no operator command is pending (~5 s cadence). With a DeepSeek backend
  installed, every one of those cycles went to the cloud — roughly 17k billable
  calls/day on an idle board — and a failure *before* Wi-Fi came up set the
  `LLM_SKIP_CALLS` back-off, which then answered the operator's first commands
  from the local heuristic (observed: "你好" and "北京天气" degraded to a
  temperature reading right after boot). Added
  `MiniAgent::set_llm_local_only()` (magent-core) and the firmware now marks the
  internal heartbeat as local-only while real commands still use the model.
  Verified: 25 s idle window → 0 LLM calls, 5 internal self-tests.
- **`LLM_SKIP_CALLS` 5 → 2 (2026-09-15)**: each LLM attempt is already retried
  once by the backend, so two consecutive failures indicate a real outage.
  Keeping the back-off at 5 muted the model for five further commands after a
  single ~20 s hotspot dropout (a news question got answered with a CPU
  temperature).
- **LLM requests are now retried once (2026-09-15)**: `llm.rs` built its
  connection/request inline with no retry, so the first HTTPS call after boot
  (or the odd later one) failing on the hotspot disabled the model for the next
  five tasks. The `POST /chat/completions` path is extracted into
  `post_chat()` and wrapped in a 2-attempt loop that logs each failure.
- **RT-watchdog `feed()` from an unsubscribed thread spammed errors
  (2026-09-15)**: `esp_task_wdt_reset` is per-task, so the defensive `feed()`
  calls added to the network-tool fetch and the Core-0 LLM worker produced
  `E task_wdt: esp_task_wdt_reset(707): task not found` on every call.
  `rt_watchdog::feed()` is now subscription-aware (thread-local flag set by
  `subscribe_current`), so it is a no-op on threads that are deliberately not
  watched.
- **Agent "possible hang" false positives (2026-09-15)**: the heartbeat monitor
  used a 15 s stale threshold, below the worst-case *legitimate* agent gap (a
  10 s cloud-LLM cap plus a ~15 s network tool), so every weather/news task
  produced a burst of `[health] agent-thread heartbeat stale` errors that drowned
  out real ones. Threshold raised to 30 s with the reasoning documented; the
  real-time hang detector remains `rt_watchdog` (18 s, fed inside the blocking
  hops).
- **`AT+CWLAP` reply was unreadable / data was lost** and **`AT+PING` always
  failed** — see the entries above.

### Changed
- **LLM prompt / decision visibility (2026-09-15)**: the agent's system prompt
  now spells out the two response modes (plain text for greetings / general
  knowledge; JSON tool call only for live or real-world data), which stops the
  model from calling `read_sensor` for "你好"; and `think()` logs its decision
  (`[agent] llm tool call: <name> args=<…>` / `[agent] llm answer (N B): …`) so
  an operator can see *why* a tool was chosen instead of only its result.


  `[wifi-sup] cached scan: N APs visible` it now prints the first 10
  `ssid(rssi)` pairs (throttled with the 60 s scan). This is the single most
  useful fact when the configured AP is reported absent (`reason=201`) — it
  immediately shows whether the radio hears the AP at all, or whether the SSID
  simply does not match (a real case: a hotspot named `arksong` was provisioned
  as `arkong`).
- **`tools/s3_full_test.py` network section (2026-09-15)**: the acceptance sweep
  now exercises the whole network path when the board has an IP — device-side
  `AT+HTTPGET` for both the dashboard (`/`) and `/api/status`, an outbound
  `https://api.deepseek.com` request (proving DNS + TCP + TLS), ICMP via
  `AT+PING` (own address = hard check, internet address = best effort) and
  SNTP wall-clock sync via `AT+NTPSYNC` + `AT+TIME?`. Host-side HTTP checks are
  probed first and downgraded to an explicit SKIP when the host is not on the
  board's subnet.
- **ESP32-S3 flash/console bring-up fixes + full acceptance run (2026-09-15)**:
  partition table taken from `partitions.s3.csv` (a stale 1 MB-factory
  `partitions.bin` in the PIO build tree caused a boot loop) and the native
  USB-Serial/JTAG console registered as a second ingress link
  (`UsbSerialAdapter` + `EspIngressLink`), with replies routed back out of the
  link each command arrived on. On-hardware results with a live hotspot:
  `tools/s3_full_test.py` **33/33**, `tools/s3_tls_soak.py` **5/5** (TLS
  handshake soak over `https://api.deepseek.com`), `tools/wifi_link_test.py`
  PASS (`+CWJAP:"arksong",,0,0,DBO2`, then `+CWSTATE:5,192.168.144.237`;
  after `AT+RST` the boot log shows `connected with arksong, channel 6,
  WPA2-PSK, rssi -38`), SNTP wall-clock sync, and outbound HTTPS status 401
  (DNS + TCP + TLS + HTTP verified). Both new harness switches documented in
  `docs/ESP32_S3_PORT_SUCCESS.md`; `tools/wifi_link_test.py` now honours
  `MAGENT_PORT` so it can target the S3's native USB console.


  `flash-s3.sh` flashed `$PIO_OUT/partitions.bin`, an old copy left at the
  ESP-IDF/PIO build root by a much earlier run. The ESP-IDF/ninja build only
  refreshes `esp-idf/partition_table/partition_table_bin`, so that copy still
  described a **1 MB** `factory` partition while the app image is ~1.5 MB — the
  bootloader rejected it (`Image length 1549152 doesn't fit in partition length
  1048576`, `Factory app partition is not bootable`) and the board sat in a
  reset loop. The script now hands the source-of-truth CSV
  (`partitions.s3.csv`, 2 MB factory) to `espflash --partition-table`, which
  also lets espflash do the ELF → image conversion. `espflash` now reports
  `App/part. size: 1,553,088/2,097,152 bytes, 74.06%`.

### Added
- **ESP32-S3: the native USB-Serial/JTAG console is now a bidirectional ingress
  link (2026-09-15)**: S3 dev boards put the console on the chip's native USB
  peripheral (mirrored via `CONFIG_ESP_CONSOLE_SECONDARY_USB_SERIAL_JTAG`), so
  an operator sees every log line on `/dev/cu.usbmodem*` — but the ingress
  gateway only bound UART0 (re-routed to GPIO10/11), meaning `AT…` commands and
  natural-language agent tasks typed on that console were silently dropped
  unless an external USB-TTL adapter was wired to the UART0 pins.
    - `firmware/esp32-app/src/link_adapters.rs`: new `UsbSerialAdapter`
      (USB-Serial/JTAG driver; non-blocking `poll` via a zero-tick read, bounded
      ~100 ms write loop so a stalled TX FIFO can never block the ingress
      thread) and the `EspIngressLink` sum type (`Uart` | `Usb`) that lets one
      `IngressGateway` own both links.
    - `firmware/esp32-app/src/main.rs`: platform bring-up now hands the ingress
      thread an `IngressLinks` bundle (UART0 + USB parts); the gateway registers
      `UART0` (adapter 0) and `USB-JTAG` (adapter 1) and **replies go back out of
      the link the command arrived on** (routed via `IngressSource::Rs232.port`).
      A failed USB driver install downgrades to UART0-only with a warning.
    - Result on real hardware: `AT+GMR` / `AT+IDENT?` / `read the temperature`
      all round-trip over the plain USB cable
      (`[ingress] frame src=Rs232 { port: "USB-JTAG" }` →
      `[ingress] reply sent to host: RESULT[...]`), no external adapter needed.
- **`tools/s3_full_test.py`** — one-shot ESP32-S3 acceptance sweep over the USB
  console: boot phases + PSRAM heap + both ingress links, the AT command surface
  (incl. `+CMDER` error codes), ingress round-trip, agent ReAct local tool,
  N-turn free-heap drift, and (when the board has an IP) the `web_admin` HTTP
  dashboard, `/api/status` JSON and `AT+HTTPGET` loopback. Verified 26/26 checks
  on hardware (Wi-Fi leg auto-skips without an AP). Runbook + results recorded
  in `docs/ESP32_S3_PORT_SUCCESS.md`.

### Changed
- **Aerospace-grade audit + completion (2026-09-15)**: a DAL-A audit pass over
  `magent-core`, `magent-hal`, and `firmware/` exercised 1379 unit + integration
  tests with **0 failures and 0 clippy warnings** in the audited crates. Specific
  fixes this pass:
    - **recovery** (`magent-core/src/recovery.rs`): added 6 new tests that pin
      every `execute_with_retry` strategy (Abort, Skip, Fallback, Reset, plus
      the security-timeout carve-out). Previously only `RetryWithBackoff`
      had a happy-path test; a future refactor could have silently demoted
      `Fallback`/`Skip`/`Abort` into retry loops. All 13 recovery tests pass.
    - **clippy strictness** (`magent-core/tests/property_tests.rs`): removed
      an `absurd_extreme_comparisons` error on `r >= 0` for a `u32`, refactored
      `t.runs_ok = ok; t.runs_total = total;` blocks into struct-initialiser
      form (`runs_ok: ok, runs_total: total, ..Default::default()`) to silence
      `field_reassign_with_default`, removed dead `Address` imports from
      property tests, and renamed unused `seed` proptest parameters to `_seed`.
    - **bip39** (`magent-core/src/web3/wallet/bip39.rs`): moved the
      test-only `hex_nibble` helper inside `mod tests` (was flagged as
      `items_after_test_module`) and converted a `needless_range_loop`
      into an iterator with `enumerate()`.
    - **english wordlist** (`magent-core/src/web3/wallet/english.rs`):
      documented the deliberate `const WORDS: [&str; 2048]` choice with a
      targeted `#[allow(clippy::large_const_arrays)]` and a `// TRACE` comment.
    - **wallet module** (`magent-core/src/web3/wallet/mod.rs`): annotated
      the `pub mod wallet;` re-export with `#[allow(clippy::module_inception)]`
      and an explanatory comment so the convention is documented.

### Documentation
- New file: `AEROSPACE_AUDIT_2026_09_15.md` — full DO-178C DAL A audit
  report covering (1) subagent findings triage (27 of ~60 subagent
  findings re-verified and determined to be false positives or already
  gated), (2) the 5 net code fixes shipped this pass, (3) the test-coverage
  matrix before/after, (4) the residual-risk list deferred to a follow-up
  firmware-target pass, and (5) the DAL A sign-off checklist.

### Changed (audit follow-up — §8 tooling + §6 firmware residuals)
- **workspace lint hardening** (`Cargo.toml`): promoted
  `clippy::undocumented_unsafe_blocks` from the default (`warn`) to
  `deny` so any `unsafe { … }` block without a `// SAFETY:` comment
  fails CI. Audit pass found **1** offending block
  (`magent-core/src/summary/mod.rs` test helper) and added the
  missing comment; all 7 host `unsafe` blocks now have SAFETY
  documentation. Reference: §8 item 4 of the audit report.
- **firmware atomics — FW-R6 / FW-R7** (`firmware/esp32-app/src/latency_metrics.rs`,
  `at_dispatch.rs`, `sntp_sync.rs`, `main.rs`): promoted `Relaxed`
  → `(Release, Acquire)` on the cross-core counters (`METRICS`,
  `WIFI_RECONNECT_REQUESTED`, `net_up`, `network_up`, `HEAP_LOW_WATER`)
  so the dual-core S3 producer/consumer pair observes writes
  together with the matching handshake state. Test reads updated to
  `Acquire` for consistency.
- **firmware OTA — FW-R2** (`firmware/esp32-app/src/ota.rs`): added
  a `validate_ota_url` gate that rejects non-HTTPS URLs up front so
  `AT+OTA` can no longer be tricked into fetching plaintext
  firmware. The module docstring now lists the 4-step production
  hardening plan (HTTPS-only [done], cert-bundle pin, vendor-host
  allowlist, secure-boot v2) and points to the planned
  `AEROSPACE_AUDIT_FIRMWARE_2026_09_22.md` follow-up.
- **firmware BLE write — FW-R1** (`firmware/esp32-app/src/ble_config.rs`):
  reordered `handle_gatt_write` to verify the write's `handle` is
  registered in `GATT_HANDLES` *before* dereferencing `p.value`,
  added a `SAFETY` comment on the `from_raw_parts` call, and logged
  the MTU-aware refactor as the next step. The audit's MTU-state
  refactor remains the follow-up; today's 512-byte cap is the
  next-best bound.
- **firmware leak registry — FW-R5** (`firmware/esp32-app/src/main.rs`):
  introduced `register_leak()` helper that enforces a 64-entry cap
  on `LEAKED_BOXES` (evicts a sentinel entry on overflow and logs
  an error) so a runaway reconnect / OTA-init loop cannot grow the
  registry without bound. Three call sites (`init_default_nvs`,
  agent boot, wifi init) now go through the helper.
- **firmware panic handler — FW-R4** (`firmware/nrf52-app/Cargo.toml`,
  `src/main.rs`): replaced `panic-probe` (which bkpt()'d in a tight
  loop and wedged the board) with `panic-reset` (jumps to
  `SCB::sys_reset()` on panic). The watchdog-fed main task
  continues to make progress between panics.
- **firmware static-mut status — FW-R3** (`firmware/nrf52-app/src/main.rs`):
  documented the Embassy `pool_size = 1` exclusivity invariant on
  `WATCHDOG` / `BLE_STATE` / `POWER_STATE` / `AGENT` and captured
  the StaticCell migration as a TODO follow-up. The audit's
  refactor is intentionally deferred to keep the blast radius
  small; the new SAFETY comments make the current single-owner
  contract auditable.
- **firmware BLE loop — FW-R8** (`firmware/esp32-app/src/ble_config.rs`):
  re-read confirmed the only loop (`BLE_AGENT_REPLY` waiter) is
  bounded by a 30-second `Instant::now() >= deadline` check, so
  no additional cap is required. The audit's concern was about a
  pre-refactor `loop { heap_watchdog_feed(); … }` in the Bluedroid
  callback path that no longer exists in this tree.

### Changed
- **Documentation refresh** (2026-09-14): brought the platform docs up to date
  with the current state of the codebase (ESP32-S3 bring-up, ESP32-C61 OTA,
  board feature switch):
  - `firmware/README.md` — added ESP32-S3 (verified / flagship) and
    `integration-test` rows, refreshed the project tree, platform comparison
    matrices, build/test commands and roadmap (S3/S3-dual-core/OTA now done).
  - `docs/README.md` — rebuilt the index (S3 guide, dual-core scheduling, Lua
    on S3, Tier-6 validation, TLS/BLE gap analysis, build/ops, backlog) and
    updated the build commands / binary-size tables.
  - `docs/ESP32_BUILD_STATUS.md` — rewritten: the ESP32 targets build on both
    C61 and S3 (was stale "cannot be built"), with the `espup` setup, build
    scripts, warnings and hardware notes.
  - `docs/PLATFORM_COMPARISON.md` — added an ESP32-S3 section + rows, corrected
    the C61 PSRAM/RAM figures, refreshed the toolchain commands and status.
  - `firmware/esp32-app/README.md` — retitled for C61 **and** S3, documented the
    `build-c61.sh`/`build-s3.sh` scripts, board feature matrix, sdkconfig files
    and the `board-*`/`lua` feature set.

### Fixed
- **Placeholder / mock-elimination pass (2026-09-14)**: removed the remaining
  fabricated-data and placeholder-error paths found by the audit sweep, so no
  production path invents an answer and no module ships a dead "stub" body:
    - **BLE client had no real wire and faked LLM replies**
      (`magent-core/src/communication/ble.rs`): `send_request` returned
      hard-coded keyword answers ("The current temperature is 25.5°C", …) and
      `receive_response` returned a canned "Response content". The client now
      has a real request/response contract: requests are serialised onto a
      bounded `outbound` queue (`take_outbound`), gateway replies are delivered
      by the driver (`enqueue_response`), and an empty inbound queue is a real
      `NetworkError::Timeout`. `disconnect()` drops queued frames. TRACE:
      REQ-NET-002. +8 tests (queue round-trip, timeout, disconnect drain).
    - **KV-store integrity was not a CRC** (`magent-core/src/storage.rs`):
      the `crc ^= byte; crc *= 0x1021` checksum (duplicated in 3 places) is
      replaced by a single real **CRC-16/CCITT-FALSE** (`crc16_ccitt`), used by
      both write paths and both read/validate paths. +3 tests, including a
      proof that every single-bit flip is now detected.
    - **`storage.rs` tests never ran**: the test module was gated
      `cfg(all(test, feature = "esp32"))` while using `std::vec::Vec`, so it was
      unreachable from the host matrix. Now gated on `std`, `storage` is
      compiled under `std` (with `dep:embedded-storage`), and all **23** tests
      execute in CI for the first time.
    - **GAP-003 closed — ESP32 transaction polling** (`agent_tools.rs`): the
      ESP32 `backend_poll_transaction` returned `NotConfigured` unconditionally
      (a stale TODO from before the firmware linked `esp-idf-svc`). It now
      drives the real `eth_getTransactionReceipt` through the shared
      `EspHttpClient`/`Transport` and decodes status + block. Revert/pending
      handling covered by 3 new tests.
    - **`Esp32BlockchainClient` placeholder bodies** (`esp32_client.rs`): the
      `get_balance_blocking` / `get_nonce_blocking` / `send_raw_blocking` /
      `check_transaction_status` methods all returned "Use Esp32HttpClient for
      ESP32" errors; they now call the real transport (and fail loudly with
      "no Transport installed" when none is present). The fake time source
      (`static COUNTER` returning synthetic milliseconds) is replaced by an
      optional caller-supplied monotonic clock (`with_clock`), which `poll()`
      now uses to actually enforce `poll_interval_ms`. +5 tests.
    - **`magent-core`'s `esp32` feature was not self-contained**: `once_cell` is
      declared `default-features = false`, so `esp32_http`'s `OnceLock` only
      compiled when the firmware crate's own `once_cell = "1"` happened to
      unify. Added `once_cell/critical-section` to `esp32` and `once_cell/std`
      to `std`, so `cargo check -p magent-core --features esp32,…` works
      standalone (and the ESP32 code path is now host-checked).

### Added


### Added
- **Feature-gated modules made host-testable + tested** (`magent-core/src/lib.rs`):
  the pure-logic embedded modules (`power`, `hardware`, `monitoring`, `ollama`,
  `communication`, plus `config`/`safety`/`wear_leveling`/`security`) are now
  compiled under the `std` feature (matching the existing `security` gate), so
  their unit tests actually run on the host instead of being skipped behind the
  `nrf52`/`esp32`/`embedded` chip features. `storage` stays chip-gated (it needs
  `embedded_storage`). CI's default-feature `cargo clippy -p magent-core
  --all-targets -- -D warnings` stays clean.
- **`power.rs` tests** (8): `PowerManager` state-machine transitions (the
  permissive `Active → Idle → LowPower → DeepSleep → Active` sequence, no-op and
  invalid upward steps, `InvalidStateTransition` `from`/`to` names), battery
  threshold get/set, `should_enter_low_power` against the threshold, and
  `read_battery_status`. Coverage 0% → 96%.
- **`hardware.rs` tests** (11): `I2cSensor`/`SpiSensor` init/read/write and
  not-initialised error, `GpioPin` input-rejects-set + output set/read/toggle,
  and `TemperatureSensor`/`Accelerometer`/`HumiditySensor`/`PressureSensor`
  reads (incl. register fallbacks). Coverage 0% → 93%.
- **`ollama.rs` tests** (12): client construction, `build_request`,
  `add_message` (incl. overflow), `add_tools`, `serialize_request`,
  `parse_response` content + tool-call extraction + error path, tool-definition
  constant, and system-prompt sanity. Coverage 0% → 98%.
- **`communication/ble.rs` tests** (12): `BleClient` connect/disconnect,
  `send_request` keyword responses + disconnected/overlong errors,
  `send_tool_result`, `receive_response`, and `BleMessage` new/to_bytes/
  from_bytes round-trip + garbage rejection. Coverage 0% → 98%.
- **`security.rs` tests** (16): a `std`-only suite (XOR encrypt/decrypt, auth-tag
  round-trip, encryption toggle) and a `web3` suite (real AES-128-GCM
  encrypt/decrypt round-trip, nonce-uniqueness, tamper → `AuthenticationFailed`,
  short-ciphertext → `InvalidCiphertext`, HMAC-SHA-256 16-hex-char tag, oversized
  plaintext → `BufferOverflow`, `constant_time_eq`). Coverage 0% → 90%.
- **Full ReAct-loop tests** (`magent-core/src/agent_runner.rs`): a
  `run_react_loop_tests` module drives `RealAgentRunner::run()` end-to-end
  through a canned `LlmBackend` (tool call → tool execution → terminal result;
  tool-failure recovery; iteration-budget exhaustion on chatter) without any
  network. `agent_runner.rs` line coverage ~68% → 72%.
- **`agent_tools.rs` tests** (+5): `create_blockchain_tools` (8 tools),
  `register_blockchain_tools_only` populates a registry, `run_blockchain_tool`
  dispatch, `switch_chain`/`reset`/getters, and `BlockchainManagerHolder`.
  Coverage 73%.
- **`web.rs` top-level tool validation tests** (+6): `web_search` / `fetch_url` /
  `webpage_summary` / `get_weather` argument-validation and SSRF-refusal error
  paths that return before any network I/O.

- **Web3 error-adaptation tests** (`magent-core/src/web3/error.rs`): 8 unit tests
  covering every `Web3ErrorKind` helper constructor, the `Web3ErrorExt`
  trait (`into_agent` / `with_did`, including `DidKeyMismatch` re-tagging),
  and the `From<Web3ErrorKind> for AgentError` conversion. Line coverage
  39% → 96%.
- **Web3 identity security tests** (`magent-core/src/web3/identity.rs`): 13
  unit tests for the Ed25519 signing/verification surface — sign/verify
  round-trip, tampered-payload and wrong-signer rejection
  (`SignatureVerificationFailed` vs `DidKeyMismatch`), `verify_signature*` /
  `verify_signed_message*` error paths, key hex/base58 serialisation, and
  `Debug` redaction of secret-key material. Line coverage 54% → 94%.
- **AgentError classification tests** (`magent-core/src/error.rs`): 3 unit
  tests pinning `AgentError::category()` / `recovery_strategy()` /
  `is_fatal()` for every variant. Line coverage 58% → 66%.
- **`magent-core` line coverage raised to ≥ 80%** (REQ-VFY-006). Full
  workspace lib + integration suites stay green (~1391 lib + ~1287 core
  integration tests), `cargo clippy -p magent-core --all-targets -- -D
  warnings` clean.
- **`wei_to_eth_string` decimal bug fixed** (`magent-core/src/web3/blockchain/
  agent_tools.rs`): the 6-digit fractional part was computed by dividing the
  wei remainder by `1e15` instead of `1e12`, so e.g. `0.123456 ETH` rendered
  as `"0.000123 ETH"` (and the last three decimals dropped). Now uses
  `frac / 1e12`; the existing test that pinned the wrong output was updated.
- **Blockchain RPC client tests** (`client.rs`): `parse_address` / `parse_hash` /
  `parse_wei` / `parse_transaction_receipt` / `hex_encode`/`hex_decode` error
  paths and JSON-RPC error deserialisation. Coverage 41% → 63%.
- **Tool registry + executor tests** (`tools.rs`): registry query methods
  (`has_tool`/`count`/`names`/`describe`/`all_tools`), register overflow, and
  `read_sensor` across all simulated sensor types. Coverage 56% → 60%.
- **Safety mechanisms tests** (`safety.rs`): `BudgetEnforcer` / `Watchdog` /
  `StackMonitor` / `MemoryGuard` / `FaultDetector` budgets, thresholds,
  overflow rejection and reset. Coverage 21% → 95%.
- **Agent config tests** (`config.rs`): `validate()` boundary checks for every
  field, builder validation, and `to_bytes`/`from_bytes` round-trip. Coverage
  28% → 67%.
- **Event parsing tests** (`events.rs`): `topic_as_address`/`u256`/`u64`/
  `bytes32`, `data_as_u64`, and `EventFilter` `from`/`to`/`with_block_param`.
  Coverage 64% → 76%.
- **HTTP client + agent-tool tests** (`http_client.rs`, `agent_tools.rs`):
  `with_timeout`/`with_backoff`/`from_chain`, wei↔ETH conversions, and
  sign/verify tool round-trips.
- **Simulator tests** (`simulator.rs`): `SimFlashStorage` read/write/erase
  (incl. out-of-bounds), `SimGpioController` set/get/toggle, `SimBleInterface`
  connect/send/disconnect. Coverage 31% → 56%.
- **Wear-leveling tests** (`wear_leveling.rs`): `None` strategy pinning,
  `is_sector_worn`, `get_most_worn_sector`, and `reset_stats`. Coverage 79%.
- **ReAct runner tolerance + state tests** (`agent_runner.rs`): a new
  `parse_tolerance_tests` module covering `parse_tool_call` (JSON, fenced/
  prose-wrapped, Anthropic `<invoke>`/`<antml:invoke>`), `parse_result`
  (envelope, fenced block, embedded-in-prose, plain prose vs chatter),
  `find_json_object` brace-in-string handling, `ensure_system_prompt`
  (no-op / replace / insert), `first_user_message`, `Message` constructors,
  and `ToolResult` success/error. Coverage 61% → 68%.
- **`magent-core` line coverage now 81.2%** (REQ-VFY-006 ≥ 80% exceeded).
  Full workspace lib (~1416) + core integration (~1312) suites stay green,
  `cargo clippy -p magent-core --all-targets -- -D warnings` clean.
- **Web3 signed-message tests** (`web3/signature.rs`): `signature()` /
  `signer_did()` decoding, `from_json` error-path categorisation
  (InvalidJson / SchemaMismatch / HexDecode), `to_json_into` (canonical
  equivalence + small-buffer error), and `Signature` Debug redaction.
  Coverage 81% → 93%.
- **Simulator sensor tests** (`simulator.rs`): `SimSensorManager` returns
  realistic bounded values (temp/humidity/pressure/accel/light) that vary
  across reads. Coverage 57% → 64%.
- **Web tool tests** (`web.rs`): `wmo_description` mapping across every WMO
  weather code. Coverage 80% → 82%.
- **Tool executor priority tests** (`tools.rs`): `voice_output` /
  `send_notification` accept low/high/urgent and normalise unknown priorities.

- **OTA firmware update** (`AT+OTA=<url>`, `firmware/esp32-app/src/ota.rs`):
  streams an image into the inactive OTA slot, verifies it via `esp_ota_end`,
  marks the next boot target, and reboots; every failure path aborts the OTA
  handle and leaves the running firmware untouched. Runs on a worker thread so
  it never blocks the UART/AT ingress (REQ-FW-005). Requires an
  `ota_0`/`ota_1` + `otadata` partition table.
- **`AT+RESTORE` implemented** — full factory reset: `nvs_flash_erase()` + reboot
  (was a placeholder error). **`AT+MACRAND` implemented** — assigns a random
  locally-administered STA MAC via TRNG + `esp_wifi_set_mac` (was a placeholder).
- **`AT+OTA` parser**: new `AtOp::Ota` in `magent-core` (`AT+OTA=<url>`) with
  parsing unit tests (`parses_ota_with_url` / `parses_ota_https_url`).
- **OTA rollback config** (REQ-FW-005): `partitions.ota.csv` (OTA-only, no
  `factory` app) + `sdkconfig.prod.defaults` gains `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`
  pointing at it and enables `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` /
  anti-rollback. Verified to build clean together with Secure Boot v2 + Flash
  Encryption. **`flash-secure.sh` fixed**: it now merges the per-chip base
  sdkconfig with the prod overlay into one file and sets `ESP_IDF_SDKCONFIG_DEFAULTS`
  (ESP-IDF reads a single sdkconfig.defaults; the previous `SDKCONFIG_DEFAULTS`
  list was silently ignored), and no longer double-signs (`BUILD_SIGNED_BINARIES`
  removed).
- **`flash-secure.sh` Flash-Encryption burn path**: esptool `--encrypt` +
  `--flash-encrypt-key` write (auto-detected from the prod overlay); the
  flash-encryption key is now generated with `espsecure.py` (esptool has no such
  subcommand); the flash step is now `--apply`-gated so a dry-run never writes
  the device. Verified end-to-end dry-run (keys → prod build → sign → dry-run).
- **`ota.rs` cleanups**: removed the unreachable `Ok(())`, an unused `Read`
  import, and an unnecessary `mut` — C61 firmware builds with zero firmware-code
  warnings.
- **Test gate**: host-only `cargo test --workspace --lib` (excluding firmware
  crates) is fully green — 1306 passed, 0 failed. Added
  `test_rpc_malformed_response_is_error_not_panic` to `esp32_http` (a hostile /
  truncated RPC response surfaces as `HttpError`, never a panic).
- **AT parser tests**: `parses_restore` / `parses_macrand` for the newly
  implemented commands.
- **BLE AT channel confirmed wired**: BLE `SYS_CMD` (0x2A08) writes already
  dispatch through the full AT engine (`dispatch_ble_command` →
  `dispatch_at_command`), so every AT command (incl. `AT+OTA`/`AT+RESTORE`/
  `AT+MACRAND`) works over BLE with replies on `SYS_RSP`. `ble_at.rs` is a
  superseded, unreferenced skeleton. C61 builds clean with `--features ble`.
  Documented the missing BLE `SYS_CMD` authentication as a security note in
  `SECURITY.md` (see "BLE command channel").
- **Tier-6 validation runbook** (`docs/TIER6_VALIDATION.md`): a step-by-step,
  on-hardware validation plan for the ESP32-C61 — baseline boot/AT, Wi-Fi+HTTP,
  TLS, blockchain RPC, BLE, OTA (+rollback), Secure Boot v2 + Flash Encryption
  (with sacrificial-board + key-backup warnings), TLS-cert verification, and the
  regression sweep. This is the Tier-6 gate that unlocks every on-hardware-only
  backlog item.
- **`AT+PING` IPv6 support**: enabled `CONFIG_LWIP_IPV6` and extended `ping.rs`
  to accept IPv6 literals (e.g. `AT+PING=::1`) via `ip6addr_aton`, building the
  `ip_addr` union (`IPADDR_TYPE_V6`/`V4`) for `esp_ping`. Compile-verified on the
  S3 (zero firmware warnings); device boots cleanly with IPv6 enabled (boot AT
  self-test 6 commands still pass). On-hardware ping awaits Wi-Fi + a physical
  UART0 AT line.
- **`AT+HEAP` now reports `+HEAP:…`**: previously `Heap` reused `sysram_line`
  and echoed `+SYSRAM:…`. Added a dedicated `heap_line` and verified on hardware
  via the boot AT self-test (`[at-self-test] AT+HEAP? => +HEAP:2263435`).
- **`AT+GMR` board label fixed**: the version string no longer hard-codes
  `esp32-c61`; it now reports the actual board (`esp32-s3` / `esp32-c61`) via
  `cfg(feature = "board-…")`. Verified on hardware via the boot-time AT
  self-test (`[at-self-test] AT+GMR => +GMR:mAgent v0.1.0 / AT v0.2 / esp32-s3`).
- **Boot-time AT self-test**: firmware runs `AT+GMR`/`AT+IDENT?`/`AT+SYSRAM?` at
  startup and logs each reply, validating the AT engine (parse + dispatch +
  render) on-device without needing a physical UART0 line. Verified on the S3:
  GMR/IDENT/SYSRAM all return correct replies.
- **Workspace clippy debt cleaned**: `cargo clippy --workspace --all-targets
  -D warnings` is now **clean** (was failing on pre-existing lints). Fixed
  field-reassign-with-default, manual `RangeInclusive::contains`, single-char
  patterns, redundant format args, duplicated attributes, `vec_init_then_push`,
  `assertions_on_constants`, redundant match across `agent.rs`, `agent_runner.rs`,
  `did.rs`, `wear_leveling.rs`, `simulator.rs`, `conversation.rs`,
  `summary/record.rs`, `client.rs`, and the `at_tests`/`comprehensive_agent_tests`/
  `blockchain_tests`/`vc_tests`/`magent-lua` test suites. 1314 host tests still
  green.
- **`HttpClientConfig::from_url` scheme whitelist**: only `http`/`https` URLs are
  parsed; any other scheme (ftp/gopher/ws/garbage) falls back to the safe default
  instead of mis-parsing into a bogus host (defense against scheme confusion).
  +1 test (`test_config_from_url_rejects_non_http_scheme`); esp32_http tests now 38.
- **`AT+PING` implemented** (`firmware/esp32-app/src/ping.rs`): ICMP echo to an
  IPv4 literal **or hostname** (DNS via `lwip_getaddrinfo`) using ESP-IDF's
  `esp_ping` (lwip). `extern "C"` callbacks bridge the async ping task into the
  synchronous AT path with static atomics + a bounded wait; returns
  `+PING: reply=N rtt=Xus`. Previously `+CMDER:4` placeholder. IPv6 (`AT+PING6`)
  is a follow-up. Compile-verified on the S3; awaits on-hardware ping once Wi-Fi
  is associated.
- **OTA anti-rollback startup confirm**: `main` now calls
  `esp_ota_mark_app_valid_cancel_rollback()` once the agent thread is running,
  so a freshly-OTA'd firmware is marked healthy and isn't rolled back on the
  next boot (REQ-FW-005; no-op when anti-rollback is disabled).
- **AT parser tests**: added `parses_ota_without_arg` / `parses_ota_query_kind_is_set`
  (OTA parsing edge cases) — at.rs tests now 73.
- **ESP32-S3 BLE controller params fixed (258 → 257)**: `esp_bt_controller_init`
  failed on the S3 with `ESP_ERR_INVALID_ARG` (258) because the S3 classic
  `esp_bt_controller_config_t` requires `controller_task_prio == 23`
  (`ESP_TASK_BT_CONTROLLER_PRIO = ESP_TASK_PRIO_MAX - 2`) and `ble_max_act > 0`
  (default 6); both were wrong (prio was 5, ble_max_act was 0). Fixed in
  `ble_config.rs` (S3 branch) and verified on hardware — init now passes its
  parameter checks and advances to `ESP_ERR_NO_MEM` (257), i.e. the BLE
  controller needs more internal DRAM than free at init (sdkconfig memory
  tuning, logged in `docs/BACKLOG.md`).
- **ESP32-S3 on-hardware bring-up**: compiled + flashed the S3 and resolved a
  PlatformIO **board flash-size mismatch** — board `4d_systems_esp32s3_gen4_r8n16`
  declares 16MB flash but the connected board is 4MB, so the bootloader was
  16MB while the app was 4MB and the app's flash init asserted. Fixed by setting
  the board's `flash_size` to 4MB (local `~/.platformio` config; backup at
  `/tmp/board-backup.json`), rebuilt the bootloader, and the firmware now boots
  cleanly on the S3 (`[magent] v0.1.0 booting (ESP32-S3 std)`, PSRAM 2048K, agent
  ready, temp sensor 37.6 °C, ingress UART0 registered). Logged two S3-specific
  backlog items: BLE controller init error 258 (needs S3 task_prio/stack) and
  the AT/ingress channel is UART0 (USB-CDC exposes console log only).
- **Blockchain JSON-RPC transport abstraction** (`magent-core::web3::blockchain::esp32_http`):
  the previously-placeholder `EspHttpClient` (`post`/`get`/`post_raw` returned
  hard-coded errors) is now backed by a pluggable `Transport` trait, a
  process-wide default transport (`set_default_transport`), and a
  `StaticTransport` for offline testing. `post_raw`/`post`/`get` now serialise
  and parse JSON-RPC over the injected transport instead of failing (REQ-NET-002,
  REQ-NET-004).
- **Real esp-idf wire backend** (`firmware/esp32-app/src/blockchain_transport.rs`):
  `EspIdfTransport` implements the new `Transport` on ESP-IDF's native HTTP
  client (mbedTLS + CA certificate bundle, bounded 8 s connect timeout, 64 KiB
  response cap, DNS/TCP preflight). Installed as the process-wide default in
  `main`, so the agent's `get_balance`/`send_transaction` blockchain tools now
  actually reach an RPC endpoint from the device. +6 host unit tests; C61
  firmware builds clean with zero firmware-code warnings.
- **Production Secure Boot v2 + Flash Encryption path**:
  `firmware/esp32-app/sdkconfig.prod.defaults` (secure overlay that removes
  `CONFIG_ESP_TLS_INSECURE` and enables Secure Boot v2 / Flash Encryption / OTA
  rollback) and `firmware/esp32-app/flash-secure.sh` (key generation + build +
  sign + optional `--apply` eFuse burn + flash; dry-run by default). Dev
  `sdkconfig*.defaults` now mark `CONFIG_ESP_TLS_INSECURE` as DEV-ONLY.
  Documented in `SECURITY.md` (REQ-FW-004).
- **Host-side Lua scripting host** (`host/lua-app`, crate `magent-lua`): a
  sandboxed Lua 5.4 runtime that binds `hardware.*` (GPIO / I2C / ADC / PWM /
  flash / BLE / power / persistent NVRAM) and `agent.reason()` onto
  `magent-hal` / `magent-core`, plus an `AppRuntime` (boot / reload / tick /
  heartbeat / watchdog / health snapshot / graceful stop). Layered sandbox
  (stdlib allow-list, 512 KiB heap cap, per-script instruction budget,
  binding-layer argument caps), action grammar + dispatcher, persistent NVRAM,
  and a mock LLM backend for host end-to-end tests. **61 host tests**; one-command
  gate `scripts/check-lua.sh`. ESP32-S3 firmware wiring skeleton in
  `firmware/esp32-app/src/lua_task.rs` (`board-s3`-gated) + `docs/LUA_SCRIPTING_S3.md`.
- **`piccolo` pure-Rust engine + `mlua` optional**: `mlua` is now an **optional
  feature** (default on) of `magent-lua`; `--no-default-features --features
  piccolo` builds it **without `mlua`** (pure Rust, Xtensa-capable). The
  `PiccoloVm` covers the full `HardwareBackend` surface (sensor, GPIO I/O, I2C
  read/write, ADC, PWM, flash, NVRAM, BLE, power) + `agent.reason` + `call`/`has`;
  the `LuaEngine` trait + `AppRuntime<T: LuaEngine>` run the full runtime on the
  pure-Rust engine. Verified on host (default mlua 61 tests; mlua+piccolo 67;
  pure-piccolo 6 + `--lib` compiles). The firmware `lua` feature now wires
  `PiccoloVm` and builds `magent-lua` without `mlua`, so the S3 Lua path compiles
  for Xtensa (`--features board-s3,lua`).
- **`magent-core` clippy cleanup**: resolved 12 `clippy::*` warnings that
  surfaced when building with the `esp32` feature (`is_multiple_of`, `div_ceil`,
  `is_some_and`, iterator loops, doc-comment whitespace, `type_complexity`), so
  `magent-core --features esp32 --lib -D warnings` is clean.

### Fixed
- **Workspace feature-unification regression** (`magent-core/src/lib.rs`): the
  host-only modules (`agent_runner`, `conversation`, `summary`, `real_tools`,
  `simulator`, `web`) were gated `#[cfg(all(std, not(esp32)))]`. Adding the
  `magent-lua` workspace member (which enables `magent-core/esp32` for
  `MiniAgent`) flipped these off and broke `cargo build --workspace` for the
  host `cli`. Changed to `#[cfg(all(std, not(target_os = "espidf")))]` — the
  modules are excluded only on the ESP-IDF *device* target (keeping reqwest/ring
  out of firmware), and coexist with the `esp32` feature on a host build.

### Verified
- **ESP32-C61 default firmware build re-verified** after adding the
  `board-s3` Lua wiring: `firmware/esp32-app/build-c61.sh` succeeds, the
  `src/lua_task.rs` module is correctly excluded from the C61 build (the
  `board-s3` `cfg` gate), the optional `magent-lua` dependency is not pulled,
  and `sdkconfig.defaults` is restored. ELF at
  `target/riscv32imac-esp-espidf/release/magent-esp32-app`.
- **ESP32-S3 Lua firmware build verified**: `cargo +esp build --target
  xtensa-esp32s3-espidf --no-default-features --features board-s3,wifi,uart,lua
  --release` compiles the firmware with `PiccoloVm` (pure-Rust `piccolo`, no
  `mlua`) + `Esp32Hardware` + `AppRuntime<PiccoloVm>` into an Xtensa ELF. The
  `SimHardware` host simulator was gated `#[cfg(not(target_os = "espidf"))]` so
  it does not compile into the device build.
- **Full host workspace regression green** after the `mlua`-optional refactor:
  `cargo test --workspace` (excluding device/at targets) passes **1623 tests**,
  zero failures — the engine optionalization is coherent across every crate.
- **`scripts/check-lua.sh` now gates both engines**: clippy, default `mlua`
  tests, a pure-`piccolo` lib build (proves the no-`mlua` path the S3 firmware
  uses compiles), `mlua`+`piccolo` tests, the `lua-run` CLI, and fmt — all green.
- **S3 firmware `Esp32Hardware::pwm_set` wired via raw ESP-IDF LEDC**: timer 0
  (8-bit, 1 kHz, auto clock) configured once; channels 0..7 allocated lazily per
  GPIO; duty `0..=100 %` → 8-bit duty. Compile-verified: the `board-s3,lua`
  Xtensa build (`--features board-s3,wifi,uart,lua --release`) succeeds with the
  new driver (only the pre-existing `web_admin.rs` warning remains). `i2c`/`adc`/
  `flash`/`ble` remain explicit `Err` TODOs pending on-hardware pin/partition
  choices.
- **S3 firmware `Esp32Hardware` now wires ADC + I2C too** (raw ESP-IDF C API):
  - **ADC**: oneshot on **ADC1**, GPIO1..=10 → channels 0..=9, 12-bit, `DB_11`
    attenuation, linear 0..3.3 V scaling (production path documented via
    `adc_cali_*`). ADC2 intentionally unsupported (Wi-Fi conflict).
  - **I2C**: master on `I2C_NUM_0`; `i2c_read`/`i2c_write` are register-style
    (repeated-start write-then-read). SDA/SCL pins are the `I2C_SDA_PIN` /
    `I2C_SCL_PIN` constants (default GPIO9/GPIO8).
- **`PiccoloVm` now uses `Lua::full()`** so the io stdlib (`print`) is available —
  previously scripts using `print` failed with "expected function, found nil".
  Discovery from a new host test that boots the **exact S3 `DEFAULT_MAIN_LUA`
  self-test** through `AppRuntime<PiccoloVm>` + `SimHardware`; also confirmed
  piccolo lacks `string.format`, so the self-test uses `..` concatenation.
- **S3 `DEFAULT_MAIN_LUA` is now a hardware self-test**: it probes every wired
  driver inside `pcall` (temp/adc/pwm/i2c/gpio) and prints `[lua] <driver>
  ok/err` to the console — one failure is reported, never fatal. Host test
  `app_runtime_boots_s3_selftest_script` proves the shipped script boots clean
  on the piccolo engine.
- **`build-s3.sh` now enables the `lua` feature** and prints the
  `espflash flash --monitor ...` command; `docs/LUA_SCRIPTING_S3.md` gains a
  "Flashing & validating on the linked S3" runbook with expected console output.
- **S3 firmware `Esp32Hardware::flash_*` wired via ESP-IDF NVS**: `flash_read`/
  `flash_write`/`flash_erase_sector` map a 32-bit address to a keyed NVS blob
  (`flash_<addr:08x>` in the `magent_lua` namespace, so it persists across
  reboots). NVS is already initialised by `main::init_default_nvs()` before the
  Lua task starts, so the driver just `nvs_open`s. Keys stay ≤15 chars. The
  `DEFAULT_MAIN_LUA` self-test adds a `flash` round-trip probe. Only **BLE**
  (`ble_send`) remains an explicit `Err` (needs the firmware GATT notify handle).
  Fixed a target portability bug: `c_char` is `u8` on Xtensa / `i8` on x86_64,
  so `nvs_key` uses `core::ffi::c_char` instead of a hardcoded `i8`.
- **S3 firmware `Esp32Hardware::ble_send` wired**: adds `pub(crate)
  ble_config::ble_send_payload` (reuses the hardened `notify_char` →
  `esp_ble_gatts_send_indicate` on SYS_RSP; errors when no client is connected)
  and routes the Lua `hardware.ble_send` to it behind `#[cfg(feature = "ble")]`
  with a clear non-`ble` fallback. The `DEFAULT_MAIN_LUA` self-test adds a `ble`
  probe.
- **S3 `ble` feature now compiles for Xtensa** (was broken: 62 E0425/E0560).
  `ble_config::bt_controller_config()` was split into per-board constructors —
  the C61 keeps its newer ESP32-C6-style struct; the ESP32-S3 gets the classic
  ESP32 BLE controller struct (`magic`/`version` + `ESP_BT_MODE_BLE` +
  zero-init defaults via `Default`). Also fixed a device-info bug: the S3 chip
  model byte array was 8 bytes but the field expects 9 (`b"ESP32-S3 "`). Both
  `board-s3,ble,lua` and `board-c61,ble` Xtensa/RISC-V builds succeed.
  `build-s3.sh` now builds `board-s3,wifi,uart,ble,lua` so `ble_send` is active
  on the flashed S3.
- **S3 Lua app loads from NVS at boot with an embedded fallback**: `start_lua_task`
  now calls `load_app_source()`, which prefers an operator-provided `main.lua`
  stored in NVS (key `main.lua`) and otherwise runs the embedded
  `DEFAULT_MAIN_LUA` — enabling app updates without reflashing. A
  `pub set_lua_app_source(&str)` hook persists the app (operator-update entry
  point, ready to wire to an ingress/AT/BLE command). Both `board-s3,ble,lua`
  and `board-s3,lua` Xtensa builds succeed.
- **Production standards installed + comprehensive test pass**:
  - Declared **MSRV** (`rust-version = "1.85"`) in `[workspace.package]` **and
    enforced it** in `magent-core` / `magent-hal` / `magent-lua` via
    `rust-version.workspace = true` (no MSRV warnings).
  - **CI `host` job now gates `magent-lua`** (both the `mlua` and `piccolo`
    engines) and `magent-hal` with `clippy -D warnings`, and runs
    `cargo test -p magent-lua --features piccolo` — so the S3 Lua work is
    regression-protected in CI, not just via `scripts/check-lua.sh`.
  - Fixed a stale CI comment (the repo does ship `deny.toml`).
  - **Comprehensive tests green**: `cargo-deny check` (advisories/bans/licenses/
    sources ok), full host workspace `cargo test` (**1623 passed, 0 failed**),
    `magent-lua` dual-engine gate (clippy + tests + fmt clean), `magent-hal`
    clippy `-D warnings` clean, and the S3/C61 firmware Xtensa+RISC-V builds.
- **S3 Lua agent is now production-functional**: `start_lua_task` installs the
  real `Esp32ToolHandler` (GPIO / temperature tool calls drive actual hardware)
  and, when a DeepSeek model + API key are configured in NVS (`AT+LLMCFG`),
  installs the leaked `&'static mut` DeepSeek backend so `agent.reason()`
  returns real LLM decisions on-device instead of the canned heuristic. Exposed
  `nvs_load_string` / the LLM NVS keys as `pub(crate)` to share `main.rs`'s
  NVS-backed config. Compile-verified with the `board-s3,ble,lua` Xtensa build.
- **Firmware builds are now warning-free for our code** (production clean-build):
  - Removed a redundant `use crate::ble_config::BleServer;` in `main.rs` (it was
    already imported locally where used).
  - Marked the reserved-but-unwired BLE subsystems `ble_at` and `ble_wallet`
    with documented module-level `#![allow(dead_code)]` (kept for future wiring
    into the active `ble_config` dispatch).
  - Added item-level `#[allow(dead_code)]` to the unused `BleState::Connected`
    variant and the `BleServer` `stop_advertising`/`deinit`/`set_state`/
    `is_active` public API methods.
  - Gated `mod llm` behind `board-s3` (DeepSeek is S3-only; it was dead code on
    C61) and `ble_send_payload` behind `lua` (only the Lua task calls it).
  - Fixed an unused-variable warning in `web_admin.rs`.
  The `board-s3,wifi,uart,ble,lua` (Xtensa) and `board-c61,wifi,uart,ble`
  (RISC-V) builds now emit zero firmware-code warnings; the default C61 build
  still succeeds.
- **CRITICAL hardening — the piccolo engine now enforces an instruction budget**:
  `PiccoloVm::run_script`/`call` previously used `Lua::execute`, which runs to
  completion with no budget — so a hostile or buggy `main.lua` (`while true do
  end`) would **hang the `lua-thread` forever on the S3**, contradicting the
  "cannot hang the device" guarantee (the mlua `set_memory_limit`/instruction
  hook does NOT apply to the piccolo engine). Added `PiccoloVm::execute_bounded`
  which drives `Executor::step` with a bounded cumulative `Fuel` (per-step
  refill so GC still runs) and errors with "script instruction budget exceeded"
  once `MAX_FUEL` is spent. Applied to both `run_script` and `call` (the event
  loop path). New host test `piccolo_engine_enforces_instruction_budget` proves
  a `while true do end` and a runaway `on_tick` error instead of hanging, and
  that finite scripts still run normally. Compile-verified with the
  `board-s3,ble,lua` Xtensa build (still zero firmware-code warnings).
- **piccolo engine now enforces a memory cap too**: `execute_bounded` checks
  `Lua::total_memory()` (gc-arena bytes) each step and errors with "script
  memory limit exceeded" once the VM exceeds 8 MB — so a script that grows the
  heap unboundedly (not just an infinite loop) is contained instead of exhausting
  the S3's PSRAM. Together with the instruction budget this completes the
  "cannot hang / exhaust the device" sandbox guarantee on the piccolo engine
  (the mlua-only `set_memory_limit`/instruction hook was never applied to
  piccolo). New host test `piccolo_engine_enforces_memory_limit`; 9 piccolo
  tests total, Xtensa build still clean.
- **Test-coverage tooling + error-type coverage**: added `scripts/check-coverage.sh`
  (runs `cargo llvm-cov` on `magent-lua` for both the `mlua` and `piccolo`
  engines) and wired it into CI as a best-effort step. Coverage: **mlua 81.9% /
  piccolo 79.9% lines**. Added unit tests for `LuaHostError` (all 5 `Display`
  variants, `std::error::Error::source`, the `Result` alias, and the
  `From<mlua::Error>` choke-point) — `error.rs` line coverage went 43% → **100%**.
  Full host workspace test now passes **1628** (up from 1623).
- **`piccolo_vm` (S3 engine) error-path coverage**: added
  `piccolo_vm_hardware_error_paths`, which proves wrong argument types, missing
  arguments, an unknown sensor, and an undefined callable all surface as Lua
  errors (never a panic or hang) through the piccolo `hardware.*`/`agent`
  bindings. `piccolo_vm.rs` line coverage 72.3% → **74.3%** (functions
  44.6% → 51.4%); overall piccolo coverage → **80.5% lines**. 10 piccolo tests.
- **poisoned-lock robustness**: added `piccolo_vm_surfaces_poisoned_hardware_lock`,
  which poisons the `SharedHardware` `Mutex` (panic while held) and proves the
  piccolo `hardware.*` bindings surface it as a Lua error ("hardware lock
  poisoned"), never a host panic. `piccolo_vm.rs` coverage → **76.3% lines /
  54% functions**; overall piccolo coverage → **81.1% lines**. 11 piccolo tests.
  (Evaluated wiring `set_lua_app_source` via an `AT+LUAAPP` command, but the AT
  parser splits args on commas — arbitrary multi-line Lua would not survive
  parsing, so it stays a documented public API hook rather than a lossy wire
  format.)
- **hot-reload test**: added `piccolo_vm_hot_reload_swaps_script`, proving
  `PiccoloVm::reload_state` clears stale globals (functions included) and
  `AppRuntime::reload` drives a changed `main.lua` through the event loop
  without a reboot. `piccolo_vm.rs` coverage → **78.1% lines / 57% functions**;
  overall piccolo coverage → **81.6% lines**. 12 piccolo tests. (This also
  surfaced two real contract facts now locked by tests: `has()` only reports
  callable functions, and the event-loop `on_tick` must return a String.)
- **poisoned-agent robustness**: added `piccolo_vm_surfaces_poisoned_agent_lock`,
  which poisons the `SharedAgent` mutex (panic while held, caught via
  `catch_unwind` since `MiniAgent` is not `Send`) and proves `agent.reason`
  surfaces it as a Lua error, never a host panic. `agent.rs` coverage →
  **93.3% lines**; overall piccolo coverage → **81.7% lines**. 13 piccolo tests.
- **`magent-core` `at_dispatch_outcome` coverage + MSRV fix**: added unit tests
  for `AtOutcome` (`OK`, `ok_line` + truncation, `error`, `Clone`, `Debug`/`Eq`)
  — line coverage 19% → **95.4%**. While running the dual-engine gate, clippy's
  `incompatible_msrv` lint caught a real metadata bug: I had declared MSRV 1.85
  but `magent-core` uses `is_multiple_of` (stable 1.87). Bumped the workspace
  `rust-version` to **1.87** (correct floor). Full host workspace test now
  passes **1634** (up from 1628); magent-core/lua/hal clippy `-D warnings` all
  clean; S3 `board-s3,ble,lua` build succeeds.
- **`magent-core` `recovery` coverage**: added tests for `RecoveryManager`
  (`get_strategy` covering every `AgentError` class incl. the "security"
  `OperationTimeout` special-case, `should_retry` max-retry/strategy logic,
  `DefaultFallback` f32/u32/bool, and the `new`/`default` limits) — `recovery.rs`
  line coverage 24% → **87.9%**. Full host workspace test now passes **1639**
  (up from 1634). (Also confirmed the `web3`-feature arm against the real
  `Web3ErrorKind::InvalidDid { raw }` struct variant.)
- **`magent-core` `health_sensors` audit fix + coverage**: **audit found a real
  bug** — `HeartRateZone::from_hr_and_age` used `Cardio < 85%`, `Peak < 95%`,
  `Danger ≥ 95%` while the documented bands are `Cardio 70–80%`, `Peak 80–90%`,
  `Danger ≥ 90%`. Fixed the thresholds to `< 80`/`< 90`/else and added tests
  that lock the documented bands. Also added tests for `StressLevel`,
  `GlucoseStatus`, `HeartRhythm`, `HeartRateData`/`GlucoseData`/`EcgData`/
  `TemperatureData`, `UserProfile` (incl. error paths), `target_hr`, the
  `HealthSensorManager` history/trend/latest/`is_exercising`/`current_stress`/
  `clear_history`, and the history cap (drops oldest). `health_sensors.rs` line
  coverage 31% → **95.4%**. Full host workspace test now passes **1649** (up from
  1639); magent-core clippy `-D warnings` clean; S3 `board-s3,ble,lua` build OK.
- **`magent-core` `error` Display coverage**: added `display_tests` covering the
  user-facing `AgentError::Display` for every variant (memory/buffer/stack/
  network/storage/sensor/GPIO/validation/budget/timeout/state/config/unknown,
  plus the `web3`-gated `Web3Error`) and asserting no error renders empty.
  `error.rs` line coverage 45.9% → **67.3%**. Full host workspace test now passes
  **1651** (up from 1649); magent-core clippy `-D warnings` clean; `magent-lua`
  dual-engine gate green.
- **`magent-core` `skills` coverage**: added `extra_tests` for `SkillsManager`
  (capacity-limit + validate error paths, `get`/`all`/`all_mut`/`clear`,
  `remove` ok + missing) and `Skill` (empty-field rejection, too-long
  truncation→reject, `increment_usage`/`update_success_rate` saturation at
  `u16::MAX`/`100`/`0`, `to_injection_string` formatting). `skills.rs` line
  coverage 60% → **80.1%** (regions 88.8%). Full host workspace test now passes
  **1659** (up from 1651); magent-core clippy `-D warnings` clean.
- **`magent-core` `early_warning` audit fix + coverage**: **audit found a real
  bug** — `EarlyWarningSystem::add_hospital` claimed to "remove farthest if
  full" but actually removed the *first* hospital farther than the new one,
  so a closer hospital would evict the wrong (nearer) entry and keep a far one.
  Fixed it to evict the max-distance hospital (matching the stated policy).
  Added `extra_tests` for `AlertSeverity` names/emergency, `HealthAlert`
  ack/emergency flags, contacts capacity + priority sort, hospital capacity
  eviction, `nearest_er`/`nearest_with_specialty`, `Hospital::distance_string`
  (m/km/unknown), `GlucoseTrendPrediction::risk_description`,
  `EcgAnomalyType::description`, and the alert-ack/`critical_alerts`/
  `unacknowledged_alerts`/`generate_emergency_message` paths (via a triggered
  critical-low glucose alert). `early_warning.rs` line coverage 80% → **93.5%**
  (regions 95.4%). Full host workspace test now passes **1669** (up from 1659);
  magent-core clippy `-D warnings` clean; S3 `board-s3,ble,lua` build OK.
- **`magent-hal` nRF52 adapter coverage**: the host RAM-backed adapters
  (`NrfGpio`/`NrfFlash`/`NrfBle`/`NrfTemperature`/`NrfPower` — the backend behind
  `magent-lua`'s `SimHardware`) had **0%** own-crate coverage (only exercised
  transitively by magent-lua). Added `nrf52::adapter::tests` covering the GPIO
  mode/write rules, flash read/write/erase + bounds + the flash AND-only
  semantics, BLE connect/send, temperature read, power-profile round-trip, and
  `NrfError` Display. `nrf52/adapter.rs` coverage 0% → **66.6%**; `magent-hal`
  total 54% → **66.1%**. Full host workspace test now passes **1676** (up from
  1669); magent-hal clippy `-D warnings` clean.
- **`magent-hal` ESP32 adapter coverage**: added `esp32::extra_tests` for
  `EspGpio` (pin + output write), `EspFlash` (bounds, erase, the AND-only
  flash semantics), `EspPower` (all profiles round-trip + default), and
  `EspError` (Display + `std::error::Error`). `esp32.rs` coverage 63% →
  **82.2%**; `magent-hal` total → **69.6%**. Noted a minor API gap: `EspBle`
  has no `set_connected` setter, so its send-success path is untestable on the
  host (unlike `NrfBle`). Full host workspace test now passes **1680** (up from
  1676); magent-hal clippy `-D warnings` clean.
- **Features implemented**:
  - **`EspBle::set_connected`** — closed the noted API gap (the ESP32 BLE
    adapter had no way to become connected), enabling the send-success path and
    a test (`ble_send_succeeds_when_connected`). `magent-hal` esp32 coverage now
    ~85%.
  - **`AT+LUAAPP` — operator Lua-app update (no reflash)**: wired `set_lua_app_source`
    (previously a dead hook) into the AT layer with a **URL-safe base64**
    payload so arbitrary Lua survives the comma-splitting parser losslessly.
    Added `AtOp::LuaApp` (+ `name` + parser arm + host test
    `parses_luaapp_set_and_query`) in `magent-core`; a firmware `luaapp_dispatch`
    (set via `base64::URL_SAFE_NO_PAD` → `set_lua_app_source`, query via new
    `lua_task::lua_app_source_len`, unsupported on non-`lua` builds) and a
    `base64 = "0.22"` firmware dep. Added `scripts/luaapp-encode.sh` to turn a
    Lua file into an `AT+LUAAPP=<b64>` line (verified round-trip). Full host
    workspace test now passes **1682** (up from 1680); S3 `board-s3,ble,lua`,
    S3 non-lua, and C61 builds all succeed.




- **`AT+LLMCFG=` validation hardening** (`magent-core::at_validate::validate_llmcfg_set`):
  host-tested validator for the LLM backend model + API key that enforces
  length caps (model ≤64, key ≤128), valid UTF-8, and rejects NUL / control
  bytes / whitespace in the key. Wired into the ESP32 `llmcfg_dispatch` so a
  malformed config is rejected before it is written to NVS. 16 new host tests.
- **`AT+HTTPGET=` URL validation hardening** (`magent-core::at_validate::validate_httpget_set`):
  host-tested validator that whitelists `http://` / `https://` (case-insensitive),
  caps length at 512, and rejects NUL / control bytes / non-HTTP schemes —
  hardening the SSRF-sensitive URL surface before any worker thread is spawned.
  12 new host tests.
- **`AT+BLE=` validation hardening** (`magent-core::at_validate::validate_ble_set`):
  pure, host-tested decision helper that accepts only `ON` / `OFF` / `STATE`
  (case-insensitive) and rejects malformed forms (empty, quoted, `key=val`,
  numeric, unknown verb) with precise `+CMDER:4` / `:7` errors. Wired into the
  ESP32 dispatcher's `ble_dispatch` so a bad BLE control line is rejected
  before reaching the BLE stack. 16 new host unit tests cover every accept /
  reject path.
- **Open-source governance surface** (this PR): `LICENSE` (MIT),
  `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `CONTRIBUTING.md`,
  `SECURITY.md`, issue / PR templates under `.github/`, and a public
  `.github/workflows/ci.yml` covering host checks (fmt, clippy, test) plus
  nRF52840 and ESP32-C61 firmware builds.
- **Internal self-audit disclosure band**: a clear "this is an internal
  AI-assisted self-audit, not a third-party audit" callout at the top of
  `SECURITY_AUDIT.md` and `docs/AUDIT_AEROSPACE_2026.md`, with a written
  commitment that a third-party audit is on the post-funding roadmap
  (Trail of Bits / Cure53 / NCC Group — selection pending).
- CI status / license / audit-status badges in `README.md`.

### Changed
- `README.md` — added CI / License / Audit badges and a confidentiality
  notice; added cross-links to `SECURITY.md`, `SECURITY_AUDIT.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, and `LICENSE`.
- `SECURITY_AUDIT.md` — author is now labeled "Internal AI-assisted
  self-audit, performed by the project owner"; "Auditor Signature" replaced
  with "Internal self-audit signature"; "Next Audit" clarified into
  "Next internal review" + "Next independent audit" commitments.
- `docs/AUDIT_AEROSPACE_2026.md` — same disclosure band added at the top
  and at the conclusion; mapping to DO-178C / ISO 26262 / IEC 61508
  clarified as **informative only**.

### Security
- No code-level security fixes in this release. Disclosure-band changes
  are documentation-only and are not themselves CVEs.

### Fixed
- **`magent-core/src/ollama.rs` `parse_response` off-by-one** — the legacy
  embedded `OllamaClient::parse_response` used `pos + 10` to skip the
  `"content":"` key, but that key is **11** chars long, so every parsed
  `content` started with a spurious leading `"`. The tool-call name extraction
  used `pos + 17` against the 20-char `"function":{"name":"` pattern, so tool
  names parsed as empty. Both offsets corrected (`+ 11` / `+ 20`); new unit
  tests pin the correct values.
- **`magent-core` `property_tests` target would not compile** — it uses
  `web3::wallet::Keystore`, so its `required-features` now includes `wallet`
  (previously only `std` + `web3`, so `cargo test -p magent-core` failed to
  build the target with `could not find 'wallet' in 'web3'`).
- **CLI test suite hung under interactive `cargo test`** — the
  `web3_blockchain::stdin_read_does_not_panic_on_eof` test blocked forever on
  `std::io::stdin().read()`. It now guards with `is_terminal()` so it only
  reads when stdin is piped/closed, and never hangs the suite.
- **CLI `email_executor::debug_impl_covers_all_variants` failed when both
  `web3` and `email-tools` features are enabled** — `CompositeExecutor::new(_)`
  then returns the `Full` variant, not `WithEmailTools`; the assertion now
  expects the variant the active feature set actually produces.
- **nRF52840 firmware (`magent-nrf52-app`) failed to compile** — the `BLE_STATE`
  static was typed as the `BleState` enum but initialised with `BleStateManager`
  struct fields (fixed to `BleStateManager` with a const-compatible literal);
  `handle_characteristic_read` used `Vec<u8>` in a `no_std` crate (added
  `extern crate alloc`); and `info!("{:?}", char_idx)` required the
  `defmt::Format` derive, which is not enabled (replaced with a `&'static str`
  name helper). The firmware now builds.
- **nRF52840 integration-test firmware failed to link** — it was missing the
  `memory.x` linker script and the `build.rs` that exposes it via
  `cargo:rustc-link-search` (plus `-Tdefmt.x`); both added so `rust-lld` can
  resolve the `INCLUDE memory.x` in `cortex-m-rt`'s `link.x`.

---

## [0.1.0] — Initial open release

First public release of the workspace. See [`README.md`](README.md) for the
overall architecture, [`docs/NRF52_BUILD_GUIDE.md`](docs/NRF52_BUILD_GUIDE.md)
and [`docs/ESP32_C61_BUILD.md`](docs/ESP32_C61_BUILD.md) for build
instructions, and [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) for the
self-audit baseline.

[Unreleased]: https://github.com/arkCyber/mAgent/compare/main...HEAD
[0.1.0]: https://github.com/arkCyber/mAgent/releases/tag/v0.1.0
