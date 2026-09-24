# mAgent Backlog — Known Gaps (Post-Audit)

> Status as of the **2026-08-28 production-hardening pass**. This file tracks the
> feature/security gaps surfaced by the code audit that are **not yet resolved**,
> prioritised P0 → P2. The P0 items listed below as *done* were addressed in that
> same pass (see `CHANGELOG.md`); everything else is outstanding work.

## ✅ Done this pass (P0)
- [x] **Firmware blockchain HTTP transport** — `EspHttpClient` (was placeholder)
      now serialises/parses JSON-RPC over a pluggable `Transport`; real esp-idf
      wire backend (`firmware/esp32-app/src/blockchain_transport.rs`) installed as
      process-wide default in `main`. (REQ-NET-002 / REQ-NET-004)
- [x] **Secure Boot v2 + Flash Encryption production path** — `sdkconfig.prod.defaults`
      + `flash-secure.sh`; dev `CONFIG_ESP_TLS_INSECURE` marked DEV-ONLY.
      **Awaiting Tier-6 on-hardware validation** (eFuse burn is irreversible).
      (REQ-FW-004)

## ✅ Done (P1 follow-up pass)
- [x] **OTA upgrade logic** — `firmware/esp32-app/src/ota.rs` + `AT+OTA=<url>`:
      stream → `esp_ota_begin/write/end` verify → `set_boot_partition` → reboot;
      every failure aborts the handle (running firmware untouched). Runs on a
      worker thread. **Awaiting Tier-6 on-hardware validation.** (REQ-FW-005)
- [x] **`AT+RESTORE`** — full NVS erase (`nvs_flash_erase`) + reboot (factory reset).
- [x] **`AT+MACRAND`** — random locally-administered STA MAC via TRNG +
      `esp_wifi_set_mac` (requires the Wi-Fi interface to be stopped, e.g. safe mode).

## ✅ Done (2026-09-20) — BLE removed outright
- [x] **BLE deleted from the firmware (both boards).** The Bluedroid controller
      needs ~244 KB of *contiguous DMA-capable internal DRAM*
      (`MALLOC_CAP_8BIT|DMA|INTERNAL`; PSRAM is not usable for it), and neither
      target can supply that alongside Wi-Fi + the agent threads:
      - **ESP32-S3**: `esp_bt_controller_init` fails with 257 `ESP_ERR_NO_MEM`
        in `btdm_controller_mem_init` at ~244–253 KB free internal DRAM.
        `CONFIG_BT_ALLOCATION_FROM_SPIRAM_FIRST` and disabling BLE 5.0 did not
        free enough. The S3's Bluedroid controller exposes **no
        `BTDM_CONTROLLER_*` Kconfig to shrink it**.
      - **ESP32-C61**: ~134 KiB internal DRAM total; BLE init asserts in
        `btm_ble_init` once Wi-Fi has taken its ~56 KiB.
      Removed: `ble_config.rs` / `ble_at.rs` / `ble_wallet.rs` / `ble_gatt.rs`,
      the `ble` Cargo feature, the `#[cfg(feature = "ble")]` paths in
      `main.rs` / `at_dispatch.rs` / `lua_task.rs`, and `CONFIG_BT_*=y` in
      `sdkconfig.c61.defaults` / `sdkconfig.s3.defaults`.
      **`AT+BLE=…` is still parsed and answers `+CMDER:9`** rather than being
      dropped, so the command surface stays predictable. The Lua
      `hardware.ble_send` always returns `Err` (no silent payload drop).
      **Why this is not a limitation:** the firmware's provisioning/command
      channel is UART0 (`AT+CWJAP=`, `AT+LLMCFG=`) or the web admin panel; BLE
      was never the only path.
      #### Correction: the "110 errors" was an invocation bug, not a regression
      An earlier note in this file claimed `--features ble` had regressed under
      ESP-IDF 5.5.5. That was wrong. `build-c61.sh` **swaps
      `sdkconfig.c61.defaults` into `sdkconfig.defaults`** before invoking
      cargo, because the pio build driver only ever reads the standard
      `sdkconfig.defaults` filename. The failing probes ran a bare
      `cargo check --features ble`, which read `sdkconfig.defaults` as-is — the
      **S3 file, where `CONFIG_BT_ENABLED=n`** — so bindgen legitimately emitted
      no BLE symbols. The "evidence the defaults file is honoured" cited here
      was worthless: every value used to argue it (`SPIRAM=y`,
      `LWIP_TCP_MSS=1200`, `FREERTOS_HZ=1000`, `ESP_MAIN_TASK_STACK_SIZE=16384`)
      is byte-identical in both files, so it was equally consistent with the S3
      file being read. Confirmed by `diff sdkconfig.defaults
      sdkconfig.c61.defaults`, where `CONFIG_BT_ENABLED` is the only relevant
      divergence. There was never an ESP-IDF-version regression.

## 🔴 P1 — outstanding
- [ ] **Firmware TLS stability** — HTTPS on the C61 currently fails fast with a
      memory error; only HTTP is reliable. Blocking real certificate-verified
      TLS on device (pre-requisite for secure RPC / DeepSeek / OTA in prod).
      Tracked against `CONFIG_ESP_TLS_INSECURE` removal.
- [x] **`AT+PING` (IPv4 + IPv6 ICMP)** — implemented via esp_ping in
      `firmware/esp32-app/src/ping.rs` (async→sync callback bridge). IPv4
      (literal + DNS hostname) and IPv6 literals (`::1`, `fe80::…`) supported;
      `CONFIG_LWIP_IPV6=y` enabled. **Correction (2026-09-21):** the original
      "compile-verified" claim held only for the S3. It was enabled in
      `sdkconfig.defaults` (S3) but **not** in `sdkconfig.c61.defaults`, so the
      C61 — the default board — did not compile at all (`ip6_addr` /
      `ip6addr_aton` / `ip_addr` are not generated under `CONFIG_LWIP_IPV6=n`),
      and CI masked it by building the C61 against the S3 config. Both are fixed
      (C61 now sets `=y`; the IPv6 path is additionally gated on an `ipv6_ping`
      cfg so it builds either way) — see `CHANGELOG.md`, *Fixed (2026-09-21)*.
      **Awaiting on-hardware ping** (needs Wi-Fi associated — safe mode must
      clear — and a physical UART0 AT line).
- [x] **OTA rollback** — `partitions.ota.csv` (OTA-only, no `factory`) +
      `sdkconfig.prod.defaults` (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` +
      anti-rollback + OTA-only table). **Verified**: the full production config
      (Secure Boot v2 + Flash Enc + OTA-only + rollback) builds clean. `OTA`
      code already calls `esp_ota_mark_app_valid_cancel_rollback`.
      **Awaiting Tier-6 on-hardware validation.** (REQ-FW-005)
- [x] **Flash-Encryption burn in `flash-secure.sh`** — added esptool `--encrypt`
      + `--flash-encrypt-key` write path (auto-detected from the prod overlay);
      fixed flash-encryption-key generation to use `espsecure.py` (esptool has
      no such subcommand); made the flash step `--apply`-gated (dry-run no longer
      writes the device). Verified end-to-end dry-run.
      **Awaiting Tier-6 on-hardware validation.**

## 🟡 P2 — outstanding

### 🟢 P2 — REPL line editing and ragged boxes, both done (2026-09-24)
- [x] **The REPL prompt no longer leaks into a redirected stdout** (fixed
      2026-09-24). The line editor wrote its prompt and every redraw to
      stdout, so `magent run --repl > session.log` (stdin still a terminal)
      logged one line per keystroke with a `\r\x1b[K` in front of it.
      Terminal UI now goes through `lineedit::ui_on_stdout` — stdout while
      stdout is the terminal, stderr otherwise — and `clear`/`cls` follows
      the same rule (and draws nothing when neither stream is a terminal).
      Covered by `repl_prompt_goes_to_stderr_not_stdout`; all four
      stdin/stdout combinations verified against a real pty.
- [ ] **Ctrl-C leaves the terminal in raw mode if the shell does not
      restore it.** `RawMode`'s `Drop` restores the saved `termios`, but
      Ctrl-C is deliberately still `SIGINT` (the module keeps `ISIG` set so
      it "kills the REPL the way it always has"), and a signal-terminated
      process never unwinds, so the guard does not run. Verified with zsh:
      the shell's job control restores the terminal on the next prompt, so
      this is invisible interactively — but a pty driven by a program with
      no such restore is left with `ICANON`/`ECHO` cleared. Options if an
      owner wants it airtight: **(a)** install a `SIGINT` handler
      (via `sigaction`) that restores `termios` and re-raises, keeping the
      exit status; **(b)** ignore `SIGINT` while a line is being edited and
      treat `0x03` as "clear the current line" the way readline does;
      **(c)** leave it and document. **Owner's call** — (b) changes
      behaviour, (a) needs a signal-safe global.
- [x] **`run --repl` now sees the config-file layer** (fixed 2026-09-24, see
      CHANGELOG). The REPL check moved in `execute()` to *after*
      `config::load()` + `apply_config_overrides` and the prompt resolution,
      so provider/model/URLs and `--prompt-name` provider/model hints apply to
      `--repl` exactly as they do to `run "<task>"`. Covered by
      `cli/tests/repl.rs::repl_honours_config_file_provider`.
- [x] **Slash-prefixed commands work** (`/help`, `/quit`, …) and `tools`
      only lists the Email tools when built with `--features email-tools`
      (fixed 2026-09-24). `retry`'s dead `!h.starts_with('/')` filter was
      reduced to `session.history.back()`.
- [x] **`--quiet`, `tools`, `env` and `alias` tell the truth** (fixed
      2026-09-24). `--quiet` now drops the REPL trace sink as the one-shot
      path does; `tools` is generated from `TOOL_DEFINITIONS` +
      the executors' cfg-aware catalogs (no more fictional `ble_scan`);
      `env` lists the variables actually read; `alias` matches the
      dispatcher. Covered by `repl_quiet_suppresses_tool_trace` and
      `repl_tools_lists_only_real_tools`.
- [x] **`--repl` shares the one-shot credential + backend reporting**
      (fixed 2026-09-24). `execute_repl` now resolves the DeepSeek key via
      `resolve_api_key_with_config` (so `DEEPSEEK_API_KEY` / configured
      `api_key_env` work under `--repl`, not just `--api-key`), reuses the
      prompt/config resolved by the caller, and reports the real backend
      label in `backend`/`stats` instead of `mock/deepseek`. `clear` no
      longer writes escapes into a piped stdout. Covered by
      `repl_honours_env_deepseek_api_key`,
      `repl_backend_command_matches_banner`,
      `repl_clear_writes_no_escape_when_piped`.
- [x] **`--repl` hands the optional tools to the model, and `stats` counts
      the whole session** (fixed 2026-09-24). The REPL's `RunnerConfig` had
      an empty `tool_descriptions`, so the Email/Blockchain tools it listed
      were invisible to the LLM; both paths now share
      `optional_tool_descriptions()`. `Total iterations` / `Tool calls` are
      accumulated on the session instead of showing the last run only.
      Covered by `repl_hands_optional_tools_to_the_model` and
      `repl_stats_accumulates_across_tasks`.
- [x] **Module docs no longer claim a `run`-level env layer** (fixed
      2026-09-24). `OLLAMA_HOST` / `DEEPSEEK_HOST` / `MAGENT_PROVIDER` are
      read only by `magent doctor`; the config-file path table in
      `config.rs` pointed at a nonexistent `~/.magent/config.json`. Both doc
      blocks now match `config_path()` / `apply_config_overrides`.
- [x] **`--repl` now keeps conversation context across turns** (fixed
      2026-09-24). The loop called `RealAgentRunner::run`, which clears the
      history at the top of every call, so each turn was context-free and
      `context` never grew — contradicting the documented contract on
      `repl_mode`. `magent-core` grew `run_continuing()` (sharing
      `run_inner` with `run`) and the REPL uses it. Covered by
      `repl_preserves_context_across_tasks` (CLI) and
      `run_continuing_keeps_history_while_run_resets_it` (core).
- [x] **The prompt no longer advertises blockchain tools with no backend**
      (fixed 2026-09-24). `optional_tool_descriptions()` injected
      `get_balance` / `send_transaction` whenever the `web3` feature was
      compiled in, but `CompositeExecutor::new(None)` returns
      `SimulatorOnly` — no blockchain backend — so those calls could only
      fail with `ConfigurationError`. The catalog and the REPL `tools`
      listing now follow `has_blockchain()`, and the listing states the
      `--email-tools` requirement.
- [ ] **Decision: should the blockchain executor be auto-wired without
      `--email-tools`?** Today the only path that constructs a
      `BlockchainExecutor` is `CompositeExecutor::new`'s `--email-tools`
      branch — `new_with_features(email, blockchain_rpc, chain_id)` and
      `with_blockchain()` have **no callers**. So a `--features
      web3,blockchain` build with no `--email-tools` cannot make any web3
      call at all, and the CHANGELOG's honesty fix above only documents
      that. The two coherent options: **(a)** keep it gated on
      `--email-tools` (web3 stays opt-in, but the flag name is misleading
      for a web3-only user), or **(b)** move the auto-wire out of the email
      branch so compiling `web3` is enough — matching the in-code comment
      ("so the LLM can make web3 calls without extra CLI flags") but
      defaulting a **live Ethereum-mainnet** executor on for every run.
      (b) is a privacy/security default change, so it needs an explicit
      owner call; until then behavior is deliberately left as-is.
- [x] **Multi-word tasks are no longer hijacked by a command word** (fixed
      2026-09-24). The dispatcher matched the first word alone, so `reset
      the temperature sensor` reset the conversation and `tools list` printed
      the catalog. A line with arguments is now a task (`alias` excepted),
      and the REPL warns when it is handed a flag it cannot honour
      (`--save-summary` / `--sign` / `--signed-output`), which it used to
      accept silently. Covered by `repl_multi_word_input_is_a_task_not_a_command`
      and `repl_warns_about_flags_it_cannot_honour`. **Behaviour change:**
      `quit now` is a task now; type `quit`.
- [x] **`--repl` names every flag it drops, and `env` lists every variable
      it reads** (fixed 2026-09-24). `--load-summary` was consumed after the
      REPL dispatch, so it vanished even more quietly than
      `--save-summary` / `--sign` — all four are now named in one startup
      warning. `env` gained a `Stores` group (`MAGENT_PROMPTS_DIR`,
      `MAGENT_SUMMARIES_DIR`, `XDG_DATA_HOME`). The dead `ReplOptions` struct
      (which documented a nonexistent `magent repl` subcommand) is gone.
      Covered by `repl_warns_about_flags_it_cannot_honour` and
      `repl_builtin_commands_all_respond`.
- [x] **`help` lists every command and states the parser rules** (fixed
      2026-09-24). The box was missing `alias` and `history`'s `hist` alias,
      and never said that a leading `/` is accepted or that a command must
      be the whole line. Covered by `repl_help_lists_every_command`, which
      anchors each name to its command column (a bare `contains` matched the
      prose `` `alias` alone takes an argument`` and passed even with the row
      gone).
- [x] **A non-UTF-8 byte on stdin no longer ends the session** (fixed
      2026-09-24). `read_repl_line` conflated a decode error with EOF
      (`Err(_) => None`), so one bad byte discarded the rest of the input
      and exited 0 with no message. Errors are now reported and skipped.
      Covered by `repl_skips_a_non_utf8_line_and_continues`.
- [x] **A slash command with arguments no longer claims to be unknown**
      (fixed 2026-09-24). `/help extra` said `Unknown command: /help`; the
      message now states the whole-line rule. Covered by
      `repl_slash_command_with_args_names_the_real_rule`.
- [x] **`prompt` shows the prompt from the first command** (fixed
      2026-09-24). It read `messages[0]`, which is empty until the first
      turn installs the system prompt, so a fresh session answered
      "(no messages)". Now falls back to the runner's configured prompt.
      Covered by `repl_prompt_shows_the_prompt_before_the_first_task`.
- [x] **`--max-messages` is enforced again for a small budget** (fixed
      2026-09-24). `slice_messages` returned early when a single system
      prompt plus the task anchor filled the budget, so `max_messages <= 2`
      dropped nothing and a four-task `--repl` session held ~17 messages.
      The early return is gone; the tail trim now always runs. Covered by
      `repl_honours_the_max_messages_cap` and two `magent-core` unit tests.
- [x] **`context` / `stats` size is labelled and computed correctly** (fixed
      2026-09-24). Both printed `tokens * 4` as "chars", but `approx_tokens`
      is `bytes / 4`, so the figure was a byte count — ~3x wrong for CJK.
      Now uses `approx_total_bytes()` under a `bytes` label. Covered by
      `repl_context_size_is_labelled_bytes`.
- [x] **`history` is documented as a task history** (fixed 2026-09-24). The
      store only receives tasks (built-in commands are never pushed, so
      `retry` can reuse the last entry), but the help box, the listing
      header and the field doc all called it a "command history" — typing
      `help`/`stats`/`tools` then `history` printed `(empty)`. All three now
      say task history. Covered by `repl_history_lists_tasks_not_commands`.
- [x] **A scripted `--repl` session exits non-zero after a failed task**
      (fixed 2026-09-24, option **(b)** below). A failed task printed
      `✗ Error: …` to stderr and the loop carried on; when the user quit,
      `execute_repl` always returned `Ok(RunReport)`, and `main` maps `Ok`
      to `ExitCode::SUCCESS`. So `printf 'do the thing\nq\n' | magent run
      --repl` exited 0 even though the task errored — unlike the one-shot
      path, where the same failure propagates and exits 1. Reachable only
      with a *live* backend that fails mid-session (`run_continuing`
      returns `Err` on a chat error / unreachable backend; mock mode has no
      error path). The session now counts failures and, **only when stdin
      is not a terminal**, returns `RunError::ReplTaskFailed { failed,
      total }` at the end. A human at a terminal keeps exiting 0 — quitting
      an interactive session is success, and the transcript already showed
      the failure. Rationale for the split: a scripted caller has no
      transcript, so the exit code is its only signal. Regression tests:
      `scripted_repl_exits_non_zero_after_a_failed_task` (piped stdin,
      `--ollama http://127.0.0.1:1 --probe-ollama`) and
      `scripted_repl_exits_zero_when_every_task_succeeds` (a successful
      scripted session must stay at 0, so the signal is real). Interactive
      exit-0 was verified under a real pty.
- [x] **The multi-turn task anchor is now the session's *current* task**
      (fixed 2026-09-24). `magent-core/src/conversation.rs::slice_messages`
      pinned the **oldest** user message as the "task anchor". That is right
      for `run()` — the one-shot path pushes exactly one user message, so
      oldest == newest — but the REPL is the only production caller of
      `run_continuing()` (`cli/src/runner.rs`), which appends one user
      message **per turn**, so the anchor froze the session's first task and
      the tail was budgeted *after* it. At `--max-messages 2` the turn-2
      window was `[system, task A]` — the current question was dropped
      *before the model ever saw it*, while the stale task and the answers
      to the new one stayed. The anchor is now the **last** user message
      (`next_back`), so `run()` is bit-identical and the REPL keeps the
      current turn. `slice_keeps_system_prompt`'s second assertion was
      updated to assert `u2` (the newest) survives. Regression tests:
      `slice_keeps_the_current_task_in_a_multi_turn_session`,
      `slice_keeps_the_current_task_at_a_two_message_cap` (both fail against
      the old anchor) and the end-to-end `repl_max_messages_keeps_the_current_task`
      (drives the REPL at `--max-messages 2` and asserts the mock's
      keyword-driven reply for the *second* task, which only appears if that
      task reached the model).
- [x] **`history` records tasks only — resolved and documented**
      (2026-09-24). It stays tasks-only: recording commands
      would need `retry` to skip them (or a separate store) and would
      desynchronise `history`'s numbering from `Tasks run` in `stats`, for
      no user benefit. No behaviour change — the `ReplSession::history`
      field doc already stated this; the help box now says it too
      (`history, hist Show task history (tasks, not commands)` plus a prose
      note that `retry` therefore always reruns the last task).
- [x] **A lone `/` reports itself instead of doing nothing** (fixed
      2026-09-24). It stripped to an empty command and hit the empty-input
      arm, returning silently, while `/nosuch` and `/help extra` both
      reported. Now prints `Unknown command: /.` Covered by
      `repl_slash_prefixed_commands_are_recognised`.
- [x] **The `--repl ignores …` warning names every signing flag** (fixed
      2026-09-24). `--not-before` / `--not-after` (read only by
      `sign_run_report`) and `--passphrase-env` (read only by the vault
      lookup in `sign_after_run`) are all consumed on the one-shot signing
      path, which the REPL never reaches — but were missing from the list
      while `--sign` / `--signed-output` were present. All three added
      under the `web3_app` gate. Covered by
      `repl_names_every_signing_flag_it_drops` (all five flags).
- [x] **`run --help` names every `--repl` mode constraint** (fixed
      2026-09-24). The help line listed only "takes no <TASK>, rejects
      --json" while the parser also rejects `--repl` + `--verify-signed`;
      it now names all three. Covered by
      `run_help_text_documents_repl_constraints`.
- [x] **`io.json_default = true` + `--repl` is rejected** (fixed
      2026-09-24). The parser refuses the explicit `--json` + `--repl` pair
      (`ParseError::ReplJsonIncompatible`) so that "JSON mode ⇒ a JSON
      envelope on stdout" holds, but the config's `io.json_default` put
      `Output` in the same JSON mode with no flag for the parser to see —
      the REPL started, printed its banner/transcript to stdout, emitted no
      envelope, and returned 0. `execute_repl` now checks `out.kind()`
      before touching stdout and returns `RunError::ReplJsonMode` (exit 1),
      naming `magent config set io.json_default false` as the remedy.
      Covered by `repl_is_rejected_when_config_defaults_to_json`.
- [x] **`--repl --verify-signed` is rejected instead of dropping `--repl`**
      (fixed 2026-09-24). `--verify-signed` returns from `execute()` before
      the REPL check, so `--repl` was silently ignored. The parser already
      rejects the analogous `--repl` + task and `--repl` + `--json` pairs;
      this one now errors too (`ParseError::ReplVerifySigned`,
      `web3_app`-gated). Covered by `repl_with_verify_signed_is_rejected`.
- [x] **The REPL `env` listing covers both passphrase sources** (fixed
      2026-09-24). The `Credentials` block listed `MAGENT_AGENT_IDENTITY`
      (read only by `sign_after_run`) but omitted `MAGENT_WEB3_PASSPHRASE_ENV`
      and `MAGENT_WEB3_PASSPHRASE`, which `sign_after_run` reads at the same
      place — despite the block's promise to list "every variable the CLI
      consults". The indirection var is shown verbatim (its value is a
      name), the secret redacted as `set (***)`. Covered by
      `repl_env_lists_passphrase_vars`.
- [x] **The REPL aliases and case-folding have dispatch tests** (fixed
      2026-09-24). The aliases live in the `match` patterns, not a table, so
      nothing forced an advertised alias to still dispatch — the suite only
      typed `q`, `clear`, `retry` … lowercase and mostly canonical. Three
      new tests: `repl_aliases_reach_the_same_arms_as_their_canonical_spellings`
      (`?`/`cls`/`hist`/`rerun`/`exit`; fails when `| "?"` is removed),
      `repl_commands_are_case_insensitive` (`STATS`/`Hist`/`Quit`; fails when
      `cmd.to_lowercase()` is dropped) and
      `repl_exit_stops_reading_further_input` (a task after `exit` must not
      run; fails when the quit arm returns `Continue`). 28 → 31 tests.
- [x] **`--probe-ollama` was a no-op — now it decides whether a failed probe
      is an error** (fixed 2026-09-24). The flag (and
      `config.runner.probe_ollama_on_run`, which `apply_config_overrides`
      mirrored onto it) never reached the runner: both `build_runner` and
      `execute_repl` hard-coded `probe_ollama_on_run: !opts.mock`, so the only
      runs where it could matter were the ones where it was already true.
      Chosen semantics — the documented one: "probe on every `run()` call",
      and a failed probe **errors** rather than falling back to the canned
      responder. Both paths now skip `force_disable_backend()` on a failed
      startup probe when the flag is set (that call also cleared
      `probe_ollama_on_run`, which is what froze a REPL session into
      simulation for good), and the REPL calls the new
      `RealAgentRunner::reset_backend_probe()` before each task so the
      reachability verdict is not latched. `--probe-ollama --mock` is now
      `ParseError::ProbeWithMock` instead of a silent contradiction. An
      un-honourable probe (no key / unbuildable client) warns on stderr.
      **Behaviour change**: `runner.probe_ollama_on_run = true` in a config
      now inherits the strict behaviour — the CLI already treated the two as
      one switch, and the config default is `false`, so only explicit opt-ins
      change.
- [x] **`alias <arg>` accepts an argument but does nothing with it** (fixed
      2026-09-24). Took option (a): `alias <name>` expands the name via the
      new `alias_lookup` (`hist → history (alias)`, `help is a command (no
      alias needed).`, and a miss is named with a pointer back to `alias`).
      The "command must be the whole line" exception is now backed by a real
      argument form, which is what the dispatcher's comment claimed.
- [x] **The REPL exits cleanly on a closed stdout** (fixed 2026-09-24).
      Took option (b): `main()` calls `restore_default_sigpipe()` (a
      `libc::signal(SIGPIPE, SIG_DFL)`) as its first statement, and `libc`
      is now a `cfg(unix)` dependency of the `cli` crate. `magent … | head`
      now dies on SIGPIPE (status 141) as Unix tools do, with no panic and
      no `exit 101`. The owner accepted the CLI-wide scope: the same latent
      panic sat on the one-shot path's `println!` trace writes. Option (a)
      (routing every REPL print through `Output`) was rejected as far more
      churn for no extra correctness. Covered by
      `repl_is_not_killed_by_a_rust_panic_when_stdout_closes`.
- [x] **`--load-summary` is supported in the REPL** (fixed 2026-09-24) —
      and fixing it uncovered that the flag **never worked on either path**.
      The note was inserted at step 3.5, before the runner started, and did
      not survive to the first request: on the one-shot path `run()`'s
      `reset()` cleared the conversation first; on the REPL path
      `insert_after_system_prompt` put the note at index 0 (the conversation
      was still empty), where `ensure_system_prompt` read it as a stale
      prompt and overwrote it. Insertion now happens inside `run_inner` via
      the new `RealAgentRunner::seed_system_context(body)`, which queues the
      note and applies it at index 1 once the prompt is reconciled. Verified
      end-to-end against a local stand-in Ollama logging request bodies:
      one-shot and REPL turns 1 and 2 each show `system prompt | note | task`
      with exactly one copy of the note. The REPL additionally drops
      `--load-summary` from its `--repl ignores` list. `--save-summary` is
      still warned about — a REPL has no "end of run" to hang a save off.
- [x] **A task that is exactly one built-in command word cannot be run**
      (fixed 2026-09-24). Took the forcing-prefix option: a leading `!` makes
      the rest of the line a task (`!help` runs `"help"`), and a bare `!`
      explains itself rather than being sent to the model. The alternative
      rule ("`/` always command, bare always task") was rejected — it would
      silently turn every bare `help` / `quit` / `stats` into an LLM call and
      break every existing script. Documented in the `help` box; pinned by
      `repl_bang_prefix_forces_a_builtin_word_to_be_a_task`.
- [x] **The REPL has line editing** (built 2026-09-24). New
      `cli/src/lineedit.rs`: a pure `LineBuffer` (char-indexed cursor,
      insert / backspace / forward-delete / motion / kill) plus a thin
      termios loop that clears `ICANON | ECHO` — leaving `ISIG` alone so
      Ctrl-C keeps working — and redraws after every key. ↑/↓ recall the
      task history, so an arrow key is no longer submitted to the LLM as a
      task. Piped stdin is detected with `io::stdin().is_terminal()` and
      falls back to the original `read_line`, which is how every existing
      test still drives the REPL; a failed `tcgetattr` falls back the same
      way. `ReplSession::get_history_entry` and `history_pos` were deleted
      — the browse position is per-read state now. Verified under a real
      pty (insertion, Ctrl-A/E, ↑ recall, Ctrl-C as a signal) and by 37
      unit tests over `tty::edit`, which is generic over `Read`/`Write` so
      no pty is needed in CI.
      **Left open:** the redraw assumes the line fits the terminal width —
      a wrapped line redraws progressively more wrongly. Cosmetic only, and
      the fix is real row/column tracking across wraps.
- [x] **Cosmetic: the REPL's box-drawing was ragged** (fixed 2026-09-24).
      Settled on the `pad_to(width)` route rather than dropping the boxes:
      the four printers now share `box_open` / `box_line` / `box_separator` /
      `box_close`, which pad through a `display_width` helper that sums
      per-char display width. Every row of a `help` + `stats` + `tools`
      session is exactly 64 columns, asserted by `repl_boxes_are_rectangular`
      (fails both on a wrong pad and on a byte-length `display_width`). The
      helper covers only the ranges the REPL prints, which keeps
      `unicode-width` out of the CLI's dependency set.

- [x] **BLE removed (2026-09-20)** — the whole GATT config service, the BLE AT
      channel (`SYS_CMD`/`SYS_RSP`), the BLE wallet service (0x1851) and the Lua
      `hardware.ble_send` bridge are **gone**, not pending. The controller never
      fit in internal DRAM on either board — see the *BLE removed outright*
      entry above for the numbers. `AT+BLE=…` now uniformly answers `+CMDER:9`.
      **If BLE is ever wanted again**, the two viable routes are (a) NimBLE
      (substantially smaller than Bluedroid, and the S3's `CONFIG_BT_NIMBLE_*`
      knobs let you shrink the controller) or (b) an explicit internal-DRAM
      budget that evicts something else. The deleted service is recoverable
      from git history (`src/ble_config.rs` as of commit `77d9f3d`).
- [ ] **Email MCP on ESP32** — `--email-tools` is host-only; no SMTP client on
      firmware for device-originated alerts.
- [ ] **Zigbee / 802.15.4** — nRF52840 advertises the `thread` feature; Zigbee and
      a real 802.15.4 stack are still on the roadmap.
- [ ] **Lua host on C61** — `lua_task` is ESP32-S3-only (`board-s3` gate); no Lua
      App runtime on the primary C61 board.
- [ ] **S3 Lua VM crashes Core0 on hardware (2026-09-23).** The `lua` feature is
      OFF by default for a *runtime* reason, not a build one: the build is
      verified end-to-end (`cargo check/build --features board-s3,wifi,uart,lua`
      is clean — see `docs/LUA_SCRIPTING_S3.md`), but booting it crashes Core0
      even after the I2C pin fix (`firmware/esp32-app/build-s3.sh:17`). Until
      this is root-caused the on-device Lua app and `Esp32Hardware` drivers are
      unvalidated. The doc/build-hint drift that described this as a build
      failure was corrected 2026-09-23.
- [ ] **Platform coverage** — ESP32-C3/C6 "compatible" but unverified; ESP32/S3
      Xtensa path in progress; Secure Boot prod path needs real-hardware sign-off.
- [x] **nRF52840 scaffold warning debt — cleared (2026-09-20).**
      `cargo check -p magent-nrf52-app --target thumbv7em-none-eabihf` went from
      **67 warnings to 0** across all three feature combos (default/`ble`,
      `--no-default-features`, `thread`). Two distinct problems were fixed:

      1. **Real soundness bug — `static_mut_refs` (FW-R3).** `main.rs` held four
         `static mut` globals (`WATCHDOG`, `BLE_STATE`, `POWER_STATE`, `AGENT`)
         accessed through `unsafe` blocks justified only by a comment
         ("Embassy serialises the two tasks on one core"). Taking `&`/`&mut` to
         a `static mut` is precisely the aliasing hazard Rust forbids, and the
         invariant would have broken *silently* if a second core or a second
         `main_task` spawn were ever added. They are now
         `Mutex<CriticalSectionRawMutex, RefCell<T>>` — a critical-section mutex,
         `lock()` of which masks interrupts for the closure — so the compiler
         enforces the exclusivity the comment used to assert. Every `lock()`
         closure is synchronous (no borrow held across an `.await`), so the
         `RefCell` cannot be re-entered by a task interleave.
         `MiniAgent` owns `dyn LlmBackend` / `dyn ToolHandler`, which are
         deliberately `!Send`, so its slot needs one `AgentSlot` newtype
         carrying an `unsafe impl Send + Sync` with the single-core
         justification written out — **one auditable `unsafe` assertion
         replacing five unsafe access sites**.
      2. **Scaffold dead-code noise.** The remaining 57 warnings were all in the
         four scaffold modules (`ble.rs` 33, `watchdog.rs` 8, `sensors.rs` 8,
         `power.rs` 7) and **zero in `main.rs`**. Silenced with a module-scoped
         `#![allow(dead_code)]` + comment in exactly those four files, so
         `main.rs` (the live path) stays strict and a genuinely dead item there
         still warns. The modules' unimplemented surface is unchanged and still
         documented (README: "compiles, scaffold only").

      **Still open on this platform** (unchanged by the above): the nRF BLE GATT
      service only logs, `agent_task` never drives `MiniAgent`, no SAADC, and the
      hardware WDT is never fed.
      **Updated 2026-09-23:** `agent_task` no longer *pretends* to drive it. It
      used to log `Running task: <task>` / `Agent is running` / `Budget check
      passed` for four canned task strings without calling the agent; it now
      states once that the loop is a scaffold and is otherwise silent. The
      battery figure `main_task` logs is likewise marked `(simulated)` — the
      underlying gap (no SAADC, no LLM backend) is what remains open, not the
      reporting of it. **Further 2026-09-23:** the same `main_task` fed the
      software watchdog a *constant* (`WATCHDOG_TIMEOUT_MS`) where `feed` expects
      the current time, so `remaining_ms` always computed zero elapsed and the
      `[MAIN] Watchdog: {}ms remaining` line printed a fixed `60000` that never
      counted down. Both call sites now pass `Instant::now().as_millis()`, so the
      figure is real. That still does **not** make the watchdog a watchdog:
      `watchdog::check_timeout` / `HealthMonitor::check_all` have no caller, so
      nothing acts on an expiry, and the hardware WDT is still never fed (see
      "Still open" above). See `CHANGELOG.md`, *Fixed (2026-09-23)*. Note also that the nRF target is **not** covered
      by the `clippy --all-targets` gate (firmware crates are excluded from
      `default-members`, because `nrf52840-pac` cannot build on a macOS host), so
      this cleanup will not regress loudly in CI.

### 🟡 P2 — the device identity seed is sealed with the MAC-less DBO1 format (2026-09-22)
- [ ] Two **live write paths** still produce DBO1, the pre-HKDF format with **no
      integrity check at all**, while the format's own upgrade command
      (`AT+WIFIPASSUPGRADE`) exists precisely to move data off it:
      - `firmware/esp32-app/src/device_key.rs:303` seals the **32-byte device
        identity seed** (the root secret for the device's DID / wallet — the
        highest-value secret on the device, TRNG-generated at first boot and
        never present in the image) via `wifi_pass_seal::seal_str`, i.e. DBO1.
      - `firmware/esp32-app/src/main.rs:1524` (`seal_and_store_wifi_pass`) seals
        the env-var default AP password via the same call. This one is *milder
        than it looks*: the plaintext comes from `option_env!("MAGENT_WIFI_PASS")`,
        so it is compiled into the firmware image regardless of seal format —
        the seal adds nothing to protect a secret that ships in the binary. That
        is also the likely origin of the `Ark314159` value committed in
        `a823f04`.
      The doc comment on `open_stored_wifi_pass` (`main.rs:1535-1547`) already
      states the DBO1-write / DBO2-write split as intended, so this is a known
      design state, not a regression.
- [ ] **Why DBO1 is the weaker format.** `seal_str` is a hand-rolled repeating
      XOR: `cipher[i] = plain[i] ^ key[i % key.len()] ^ nonce[i % nonce.len()]`
      — the device key used directly as a repeating pad. The key itself *is*
      derived (`boot_key::derive` = HKDF over eFuse material), but `seal_str`
      applies no stream cipher, no per-message key schedule, no AEAD and
      **no MAC**. Confirmed
      against the code, the construction does hold up structurally for the sizes
      in use (pad period `lcm(32, 12) = 96 > 64` = `MAX_PLAINTEXT`, so the pad
      never repeats within one message, and a fresh TRNG nonce per write means
      it does not repeat across messages). The concrete defect is therefore
      **not** confidentiality but **malleability**: `open_sealed_bytes` returns
      `Ok(DecodedBytes)` for any well-formed input, so a bit-flip in the stored
      `magent:dev_identity` value is not detected — it silently yields a
      *different seed*, hence a different device DID / wallet, with no error
      anywhere. DBO2 would reject the same tampering with `BadMac`.
      DBO1 is also unauthenticated on read: nothing binds the ciphertext to the
      key, so wrong-key and tampered-ciphertext are indistinguishable.
- [ ] **Not reachable remotely**, which is why this is P2 and not P0: writing
      `magent:dev_identity` requires NVS write access — physical flash access or
      an AT command over UART0/USB. There is no network writer (`web_admin.rs`
      is read-only: two GET handlers, no state-changing endpoint, and it exposes
      only version/state/IP/SSID/RSSI/heap/uptime — no secrets, so its lack of
      auth is a telemetry-disclosure question, not a credential one). Physical
      access is already game-over on a device with no Secure Boot eFuse burned
      in dev config.
- [ ] **Fix, if wanted**: switch both write paths to
      `wifi_pass_seal_v2::seal_str` (a drop-in — same `device_key` input, and
      `open_stored_wifi_pass` already dispatches on the prefix and opens DBO2
      **and** DBO1, so already-provisioned devices keep working). Not applied
      here because it changes the wire format persisted to NVS for the identity
      seed on a security-critical path with host-only test coverage, and
      firmware changes cannot be validated on hardware in this environment
      (`cargo check` only). If it is applied, the DBO1 read fallback must stay,
      and `magent-core/tests/at_tests.rs:333-350`
      (`wifi_credential_provisioning_round_trips_for_default_ap`) — which
      asserts `sealed.starts_with("DBO1:")` — has to be repointed at DBO2, plus
      the `main.rs:1535-1547` doc comment corrected.

### 🟡 P2 — `AT+BLE` validator is dead code (2026-09-21)
- [ ] `validate_ble_set` / `BleAction` / `BleValidated` in
      `magent-core/src/at_validate.rs` (~17 tests) now have **zero callers**
      anywhere in the workspace — BLE was removed 2026-09-20 and `AT+BLE=…`
      answers `+CMDER:9` before reaching a validator. Left in place because the
      *BLE removed outright* entry above contemplates BLE returning via NimBLE,
      and this is the validating half of that surface. Delete if that route is
      abandoned. `grep -rn validate_ble_set --include=*.rs` is the check.
- [ ] **`ollama_react_e2e` is load-sensitive.** Observed once: both tests in
      `magent-core/tests/ollama_react_e2e.rs` failed when the suite ran
      concurrently with a 6-minute firmware build, then passed on rerun and pass
      serially. The mock server is per-test on an ephemeral port, so the trigger
      is timing under CPU starvation, not a port clash. The *silent simulated
      fallback* that made it fail quietly is now fixed (see `CHANGELOG.md`,
      *Changed (2026-09-21)*) — a backend error surfaces as `Err` instead of a
      canned answer — but the underlying scheduling sensitivity is not
      addressed.

### 🟡 P2 — seal/open disagree about the nonce width (2026-09-22)
- [ ] Both seal functions accept a nonce of **any** length — they reject only
      the empty case — while both open functions require **exactly**
      `NONCE_LEN` bytes:
      - `wifi_pass_seal.rs` (DBO1): `seal_str` XORs `nonce[i % nonce.len()]`
        into the keystream and hex-encodes however many nonce bytes it was
        handed; `open_sealed_bytes` then does `payload.split_at(NONCE_LEN)` and
        ignores the rest. A 5-byte nonce therefore round-trips **silently to
        garbage** — the keystream is misaligned, not rejected, so no error is
        raised.
      - `wifi_pass_seal_v2.rs` (DBO2): `seal_str` / `seal_secret` write
        `hex(nonce)` at the front of the blob regardless of width, and
        `open_secret` decodes exactly `2 * NONCE_LEN` hex chars from it. A
        short nonce makes the blob unopenable (`BadMac`), which at least fails
        loudly.
      So a `seal_*` caller can construct a value its own `open_*` cannot
      correctly read — an invariant of the wire format that is enforced on one
      side only. **No in-tree caller can trigger this today**: every call site
      passes a full-width `[0u8; NONCE_LEN]`, and `at_dispatch::
      seal_wifi_password` draws it from `getrandom` and hard-errors if the TRNG
      is unavailable (no zero-nonce fallback). Left unfixed because closing it
      means either widening a public `SealError` variant's documented meaning or
      adding a variant (a breaking change for an exhaustive `match` downstream),
      and the modules are security-critical with host-only test coverage. The
      cheap fix, if it is wanted: reject `nonce.len() != NONCE_LEN` at the top of
      all three `seal_*` functions.

### 🟡 P2 — `WifiManager`'s state machine is never entered (2026-09-22, line refs refreshed 2026-09-23)
- [ ] `firmware/esp32-app/src/wifi_manager.rs` is constructed and held in an
      `Arc` by the supervisor, but nothing ever calls `mark_connected()` from
      outside the manager: `main.rs` associates at boot through `connect_wifi()`
      directly (by design — see the doc comment on `WifiManagerConfig`) and only
      ever calls `manager.reset()` (on `AT+CWQAP`, in `run_wifi_supervisor`). So
      the manager's state sits at `ReconnectState::Initial` for the process
      lifetime.
      Consequences, all verified by reading the call graph:
      - `should_reconnect()` (`wifi_manager.rs:148`) has **no arm for `Initial`**
        — it falls to `_ => false`. The only call site,
        `check_and_reconnect()` → `reconnect()`, therefore returns
        `ESP_ERR_INVALID_STATE` every time and the manager **never re-associates**.
      - `check_and_reconnect` is called only on the *down-edge* (in the
        supervisor's `Some(true) if !connected` arm, the `#[cfg(feature =
        "wifi")] if let Err(e) = manager.check_and_reconnect(…)` block), so it
        fires once per outage and its exponential backoff / `max_retries` /
        `Failed` state never govern anything. The real reconnect is the
        supervisor's own classifier below it (`CREDENTIAL_BACKOFF_S` /
        `AP_ABSENT_MAX_BACKOFF_S` / `TRANSIENT_MAX_BACKOFF_S`, in the
        `if !connected && !reconnect_suppressed` block), which does work.
      - Net user-visible effect: a spurious `[wifi-sup] 自动重连失败: …` warning
        on every outage, and the `retries= / connects= / disconnects=`
        counters in the 60 s heartbeat (`get_state_summary()`) are **permanently
        zero**, i.e. the diagnostic they exist to provide is dead.
      The link still recovers, so this is diagnostics, not availability — hence
      P2, not P0. Two candidate fixes, in increasing order of blast radius:
      (a) add `ReconnectState::Initial => true` to `should_reconnect()` so the
      first attempt is actually made (it is the pre-first-reconnect state; the
      `Disconnected => true` arm is commented "立即尝试第一次重连"); (b) make
      `mark_connected()` public and call it after every successful associate in
      `main.rs`. **(b) changes when the radio is touched** — `reconnect()` would
      then run a real `wifi.connect()` plus a 15 s wait *in the same tick* as the
      supervisor's own attempt — so it needs on-hardware validation and was
      **not** applied in the 2026-09-22 pass. `WifiManagerState::new()` and
      `mark_connected()` also hardcode `Duration::from_secs(2)` where `reset()`
      uses `self.config.initial_backoff`; that one is cosmetic — `current_backoff`
      is recomputed by `calculate_backoff()` on every attempt, so the hardcoded
      value is a dead store only observable in one debug log line.
- [ ] **Partial update (2026-09-23):** `AT+CWRECONNCFG` is now consumed — the
      supervisor reads `reconn_int` / `reconn_rep` at start and applies them to
      its *own* reconnect loop (`interval` caps the adaptive backoff, `repeat`
      caps attempts). Because the manager's state machine is still never entered
      (above), the `WifiManagerConfig::max_retries` / `max_backoff` values built
      from the same keys remain inert — the effective policy comes from the
      supervisor, not the manager. Fixing the state machine (option (a)/(b))
      would make the manager side live too.

### 🟡 P2 — the KV store's compaction erases before it knows the rewrite will land (2026-09-23)
- [ ] `magent-core/src/storage.rs:541` `KvStore::compact` is `collect_live` →
      `erase_all` → rewrite loop. The whole store is erased **before** the first
      entry is rewritten, and the rewrite loop propagates the first
      `write_entry_at` failure with `?`. A write that returns `Err` therefore
      leaves the store holding only the entries rewritten up to that point —
      every surviving entry *after* the failure is gone, and the `Err` the
      caller sees does not say that data was lost. The entries are all still in
      `live` in RAM at that moment, so the loss is not forced, just unrecovered.
      `set` (the append path) is unaffected: it writes into erased space only.
- [ ] **Why it is P2 and not P0.** `delete` → `compact(Some(key))` and
      `garbage_collect` → `compact(None)` are the only callers; a *successful*
      rewrite cannot lose anything, and no in-tree caller drives compaction on
      a path where a transient flash write fault is expected. But the failure
      mode is silent-total-loss rather than a reported partial write, which is
      the same class of defect this pass has been closing elsewhere.
- [ ] **Candidate fixes** (needs an owner decision on the durability contract
      before any is applied): (a) retry the failed `write_entry_at` a bounded
      number of times, then re-erase and redo the whole pass once — recovers a
      transient fault, changes nothing on the success path, but does not make
      the operation atomic; (b) write the compacted image to a staging region
      and erase the primary only after the staging pass succeeds — makes it
      crash-safe, but needs 2× the live set's capacity, which is exactly what a
      store being compacted does not have; (c) leave the algorithm and make the
      error message state that the store may be incomplete. `(b)` is the only
      one that survives a power loss mid-compaction, and it is a store-format
      change. Not applied here: it is a durability-semantics change to a
      security-adjacent persisted store with host-only test coverage, and the
      mock flash can inject the fault for a regression test whenever it is
      wanted (`MockFlash` at `storage.rs:721`).

### 🟡 P2 — device `write_gpio`: the silent pin default, and how far the HAL already protects us (2026-09-23)
- [ ] **Corrected finding — ESP-IDF already bounds-checks the pin, so this is
      narrower than it first looked.** `firmware/esp32-app/src/local_tools.rs:70`
      takes the pin straight from LLM-generated text
      (`let pin: i32 = kv(&pairs, "pin", "13").parse().unwrap_or(13);`) and
      passes it to `gpio_reset_pin`. That call is **not** a memory-safety hazard:
      `gpio_reset_pin` opens with
      `GPIO_CHECK(GPIO_IS_VALID_GPIO(gpio_num), "GPIO number error", ESP_ERR_INVALID_ARG)`,
      where `GPIO_IS_VALID_GPIO` is a `(1ULL << SOC_GPIO_PIN_COUNT) - 1` mask
      check. So `pin=999`, `pin=-5` and any out-of-range value are rejected by
      the HAL and surface as `success: false` with the `err=` from
      `gpio_reset_pin` — verified against
      `components/esp_driver_gpio/src/gpio.c:462` and
      `soc/esp32c61/include/soc/soc_caps.h:181` (`SOC_GPIO_PIN_COUNT` is 30 on
      the C61). The earlier worry that an arbitrary pin index would be read out
      of bounds in the GPIO mux arrays is **not** correct.
- [ ] **What is left.** (1) A pin that does not parse as an integer —
      `pin=abc`, or `pin` omitted entirely — silently falls back to the `"13"`
      default at `local_tools.rs:70`, so a malformed argument drives GPIO13
      rather than failing. That is the same "substitute a value instead of
      failing loudly" class fixed for the tool registry this pass, and it is the
      one part of this item that a plain code change closes. Note that
      `magent-core`'s *simulated* `execute_write_gpio` deliberately keeps a
      documented default (`execute_write_gpio_missing_pin_defaults_to_zero`,
      `tools.rs:1252`), so the two paths are intentionally not identical here;
      changing the firmware path does not imply changing that test. (2) ESP-IDF's
      mask is `GPIO 0..=29` on the C61 — it does **not** exclude the SPI-flash or
      strapping pins, so the HAL check says nothing about whether a given pin is
      *safe to drive* on this board. An allow-list of truly-safe output pins
      needs the board's flash/JTAG/USB assignment, which is not recorded
      anywhere in this repo (`firmware/esp32-app/BOARDS.md` documents only the
      S3 console pins and UART0). Not applied here because it is a
      hardware-behaviour change the `cargo check`-only verification cannot
      validate, and the safe-pin set is a board fact, not something to guess.

### 🟡 P2 — the CLI's `magent web3 chain|bind|balance|tx|vc` commands are documented but not implemented (2026-09-23)
- [ ] `cli/src/web3_blockchain.rs` (577 lines, `#![cfg(feature = "blockchain")]`)
      declares `BlockchainAction` (ChainList / ChainInfo / BindCreate / BindVerify
      / Balance / TxBuild / VcIssue / VcVerify / VcPresent), the option and
      display structs for each, and a `render_*` function per display type. Its
      module doc advertises nine commands:
      `magent web3 chain list|info`, `bind create|verify`, `balance`,
      `tx build`, `vc issue|verify|present`.
- [ ] **Nothing consumes it.** `grep -rn BlockchainAction --include='*.rs'` finds
      only its own definition; `cli/src/lib.rs:43` is the sole reference to the
      module. The real parser, `parse_web3_args` (`cli/src/cli.rs:1124`), has
      arms for `new` / `identity` / `did` / `pubkey` / `sign` / `verify` /
      `list` / `export` / `delete` and none for `chain`, `bind`, `balance`,
      `tx` or `vc`, so every advertised command is rejected as an unknown
      subcommand. There is also no executor: the renderers take the display
      structs, nothing produces them, and no code path calls a chain client or
      `IdentityBinding` on their behalf.
- [ ] **Why it is not a live bug.** `cli/Cargo.toml:99` is `default = []`, so
      `blockchain` (`= ["web3_app", "magent-core/blockchain"]`) is opt-in and the
      module is not in a default build; it compiles (feature-matrix checked) and
      is simply unreachable. But `Cargo.toml:116-122` documents the feature as
      "Blockchain executor for the agent work loop", so a user who enables it has
      a reasonable expectation of a `magent web3 chain …` surface.
- [ ] **Two honest resolutions, owner's call.** (a) Implement: add the `chain` /
      `bind` / `balance` / `tx` / `vc` arms to `parse_web3_args` and an executor
      that produces the display structs (balance and tx build go through
      `HttpRpcClient`; bind through `IdentityBinding`; vc through
      `verifiable_credentials`) — this is a real feature build, not a P0 fix, so
      it is out of the currently agreed fix scope. (b) Delete the module and drop
      the nine commands from the doc comment, as was done for the dead BLE
      surface. Not applied here because (a) is feature work and (b) is a
      ~577-line deletion of a module someone may intend to finish.

### 🟡 P2 — `safety::Watchdog` cannot trip, and the budget's time axis is inert (2026-09-23)
- [ ] `magent-core/src/safety.rs` `Watchdog` has no clock and no hardware
      access. `feed()` only sets `fed = true`; the sole writer of `false` is the
      test hook `simulate_timeout()`. So `needs_feed()` is `false` on any
      running device — confirmed by scope: `grep -rn "needs_feed" --include='*.rs'`
      yields only the definition (safety.rs:184) and the unit test
      (safety.rs:417-421); **no production caller**. `MiniAgent` calls
      `self.watchdog.feed()` once per ReAct iteration (`agent.rs:692`) and never
      reads the latch, so the agent has no software stuck-loop detector in core.
- [ ] The telemetry field it nominally feeds is likewise unreachable:
      `AgentTelemetry::watchdog_trips` increments only on
      `AgentError::OperationTimeout` (`agent.rs:1104`), and **nothing in the
      workspace raises that variant** — `grep -rn OperationTimeout --include='*.rs'`
      outside `error.rs`/`recovery.rs` finds only that one `matches!`. The
      iteration budget raises `IterationBudgetExhausted` instead
      (`safety.rs:83`). So `watchdog_trips` is permanently 0.
- [ ] Same module, same shape: `BudgetEnforcer::time_budget` / `time_limit_ms`
      are declared and `reset_time()` stores 0 into the counter, but nothing
      increments or reads it — there is no `consume_time` / `time_usage`
      (`time_limit_ms` even carries `#[allow(dead_code)]`, safety.rs:53). The
      time axis of the "budget enforcement" is inert; only iterations and bytes
      are enforced.
- [ ] **The memory axis is inert on the device too (2026-09-23).** The S3
      firmware raises the ceiling with
      `with_max_memory(magent_core::MAX_DYNAMIC_CONTEXT_BYTES as u32)` (2 MiB on
      `s3-8mb-psram`), and `MiniAgent::new` now actually forwards it to
      `BudgetEnforcer` (it previously ignored the config — fixed 2026-09-23).
      But `memory_limit` only binds if `consume_memory` is called, and **no
      production path calls it**: `grep -rn "consume_memory" --include='*.rs'`
      finds only the definition (safety.rs) and its tests. The embedded
      `MiniAgent` conversation is count-bounded
      (`MAX_CONVERSATION_MESSAGES` 20 × `MAX_BUFFER_SIZE` 2 KiB ≈ 40 KiB), not
      byte-GC'd. The one real consumer of `MAX_DYNAMIC_CONTEXT_BYTES`,
      `conversation::gc_to_budget`, is invoked only by the host
      `RealAgentRunner` (`agent_runner.rs`), never by the firmware. So on an S3
      the 2 MiB figure is a configured ceiling, not an enforced one. Docs now
      say so (lib.rs `MAX_DYNAMIC_CONTEXT_BYTES`, `agent.rs` config comment).
      Resolution options: wire `consume_memory` into the agent loop (needs a
      per-turn byte-accounting call), or drop the S3 `with_max_memory` call as
      decorative. Owner's call.
- [ ] **Why it is not a live bug.** The firmware — which owns a clock and the
      hardware counter — already has the *real* watchdog and it works:
      `firmware/esp32-app/src/rt_watchdog.rs` arms the ESP-IDF task WDT
      (panic-on-trigger, fed on every RT loop and long operation) and
      `firmware/nrf52-app/src/watchdog.rs` wraps the nRF WDT. Core's
      `Watchdog` is fed but never consulted, so nothing safety-relevant depends
      on it. In the `2026-09-23` pass the false claims were corrected instead of
      the mechanism: the struct/method docs now say it is a clockless heartbeat
      latch (not a "hardware watchdog timer interface"), `agent.rs`'s
      `watchdog_trips` docs no longer imply the latch or the iteration budget
      feed it, and `docs/API.md` matches.
- [ ] **Resolution options (owner's call).** (a) Implement a real core
      stuck-loop detector — requires threading a monotonic clock into
      `no_std` core (the `agent.rs` loop would stamp each iteration and trip on
      a deadline); this is a new dependency/contract, not a P0 fix. (b) Delete
      `Watchdog` from `MiniAgent` and from the public API, leaving the firmware
      WDT as the single watchdog — smaller surface, but a breaking change to a
      documented API (`docs/API.md`) that something may be compiling against.
      (c) Keep as-is now that the docs are honest. Option (b) or (a) needs the
      owner's sign-off; (c) is the current state.

### 🟡 P2 — `ATE0` / `ATE1` are accepted but do nothing (2026-09-23)
- [ ] The parser turns `ATE0` / `ATE1` into `AtOp::SetEcho { on }`, and the
      ESP32 dispatcher answers `OK` and stores nothing:
      `AtOp::SetEcho { on: _ } => AtOutcome::NoReply` (`at_dispatch.rs:258`).
      So the echo-toggle command is inert — the firmware never gates the UART
      echo register on it. This is deliberate-ish (the host already sees its own
      bytes) but was previously *also* described as "State kept by the
      dispatcher" in `docs/AT_COMMAND_REFERENCE.md`, which is false; that row
      now discloses the inert behaviour.
- [ ] **Why it is not a live bug.** Real ESP-AT scripts send `ATE0` purely to
      silence the module's echo in their own capture; answering `OK` satisfies
      them. Nothing in provisioning depends on echo state. Kept here rather than
      removed because dropping the verb would break the command-surface
      compatibility the parser deliberately keeps (same reasoning as `AT+BLE`).
      Implementing it for real (gate the TX echo path on a stored flag) is a
      feature, not a P0 fix.

### 🟢 P2 — data subcommands wrote human output to stderr — fixed (2026-09-24)
- [x] **Owner decision: option (a).** Human-mode *data* output now goes to
      stdout; stderr keeps the labelled trace, warnings and confirmations.
      `config where`, `config show`, `config list`, `config get`,
      `set-prompt show` and `set-prompt list` render through the new
      `Output::stdout_fmt_line` instead of `stderr_fmt_line`, so
      `magent config show > backup.json` produces a real (parseable) file and
      `magent config list | grep ollama` matches. The stream no longer depends
      on the mode: `--json` and human mode both put the payload on stdout.
      Confirmations (`config set`, `config init`), the `config validate`
      verdict and the `scheduler status` trace stay on stderr — they are
      diagnostics, not data.
- [x] `Output::stdout_fmt_line` mirrors `stderr_fmt_line` exactly (same
      `format_args!` contract, same no-op in JSON mode) so the two are
      interchangeable at a call site and the choice is the *only* thing that
      encodes data-vs-diagnostic. Documented in `config --help`
      ("OUTPUT STREAMS"), `set-prompt --help` and `docs/CONFIG.md`.
- [x] Guarded by `cli/tests/streams.rs` (5 tests, spawns the real binary and
      asserts on the two streams separately — the unit tests can't, because
      `Output` holds the process's real `StdoutLock`/`StderrLock`). Mutation-
      checked: reverting `config where` to `stderr_fmt_line` fails
      `human_mode_config_data_goes_to_stdout` and nothing else.
- [x] **Deliberately unchanged:** `web3 list` / `identity` / `did` / `pubkey`
      still render human output to stderr (they have `--json`, and their
      human form is a display table, not a wire format); `scheduler status`
      uses `trace_labeled`, which is a labelled trace by construction.
      `config export` / `set-prompt export` keep working as before.

### 🟢 P2 — `--prompt <missing-file>` printed the path twice — fixed (2026-09-24)
- [x] `magent run --prompt /nope.txt "hi"` used to fail with
      `could not load system prompt: /nope.txt: /nope.txt: No such file or
      directory (os error 2)`. `runner::load_prompt_file` now returns the bare
      `std::io::Error` instead of stringifying it as `"{path}: {io}"`, so
      `PromptError::PromptFileLoad` (which carries the path) adds it exactly
      once. The runner's `PromptFileLoad` arm is unchanged and now yields
      `could not load system prompt: /nope.txt: No such file or directory
      (os error 2)`. Pinned by
      `prompt::tests::prompt_file_load_error_names_the_path_exactly_once`,
      which asserts the path appears once and the message still says what
      failed.

### 🟢 P2 — `ParseError::UnknownFlag` was the CLI's catch-all — fixed (2026-09-24)
- [x] **Option (a): added `ParseError::Usage(String)`, which renders verbatim.**
      `UnknownFlag` used to carry *any* usage error — not just an unrecognised
      flag — while its `Display` prefixes `unknown flag: `, so two mutually
      exclusive scheduler flags produced
      `error: unknown flag: scheduler daemon: --interval, --cron, and --at are mutually exclusive; pass exactly one`.
      That was the "nonsense sentence" case. Note `web3 new: missing <NAME>`
      was already fixed earlier under `Web3CliError::Usage`, so it was not a
      live example by the time of this change.
- [x] The ~85 non-flag call sites were migrated to `Usage`; the genuinely
      flag-shaped sites (`format!("--{}", other)`, the `-温度` short-flag trap,
      `doctor`/`version` stray tokens) keep `UnknownFlag` and still print
      `error: unknown flag: --bogus`. Verified live: `run --bogus`,
      `run -温度 hi`, `doctor x`, `version x` all still say `unknown flag:`.
- [x] **Feature-gated builds are fixed too.** A default `cargo build -p magent`
      (no `web3`) now answers `magent web3 …` with
      `unknown subcommand: web3 (this binary was built without the \`web3\` feature)`
      via the new `unknown_subcommand_message` helper, instead of reading as if
      `web3` were a mistyped flag. Any other unknown token gets
      `unknown subcommand: <token> (try \`help\` for the list of subcommands)`.
- [x] **`help <topic>`** for an unknown topic now says
      `no help for \`<topic>\` — try \`help run\`, …`, not `unknown flag: help …`.
- Exit code was 2 before and after; this was always diagnostics-only. Pinned by
  four unit tests in `cli/src/cli.rs` (`usage_errors_render_verbatim_…`,
  `unknown_subcommand_is_named_as_a_subcommand`,
  `help_for_an_unknown_topic_lists_the_known_ones`,
  `web3_on_a_non_web3_build_explains_it_was_not_built_in`).

### 🟡 P2 — an empty `search_emails` query matches the whole INBOX, and `shutdown()` cannot be called from async code (2026-09-23)
- [ ] **`search_emails` with an empty (or whitespace-only, or all-quote)
      `query` returns every message in `INBOX`.** The criteria string is
      `OR SUBJECT "{q}" FROM "{q}"`, and IMAP's `SUBJECT ""` / `FROM ""` match
      any message — so `{"query": ""}` degenerates into "list everything",
      buffered through the search path rather than `list_inbox`. The advertised
      schema has no `minLength` on `query`, and the handler does not check, so
      this is reachable from the LLM without any misuse.
- [ ] **Not a P0:** the same caller can already call `list_inbox` for a full
      listing, and `search_emails` results are capped by `limit` (now
      range-checked). It is a semantic surprise, not an escalation of access.
- [ ] **Owner's call.** (a) Reject an empty query with
      `\`query\` must not be empty`; (b) add `"minLength": 1` to the schema and
      rely on clients (which is what we just stopped doing for `limit`); or
      (c) leave it and document that an empty query lists the inbox.
- [ ] **`McpToolExecutor::shutdown()` is best-effort and *blocking*.** It now
      works without a runtime (see CHANGELOG), but like
      `execute_async_blocking` it cannot be called from inside an async context
      — `Handle::block_on` panics there. The CLI never calls it (the child is
      `kill_on_drop`), so today it is exercised only from tests.
- [ ] **Owner's call.** Options: (a) keep the sync signature and document the
      constraint (current state); (b) add an `async fn shutdown_async()` for
      async callers; or (c) delete it, since `kill_on_drop(true)` already
      guarantees the subprocess dies with the host. Deferred — it is an API
      shape decision, not a defect.

### 🟡 P2 — `magent-email-mcp` reports tool *failures* as JSON-RPC protocol errors, and `mark_read` confirms work it did not verify (2026-09-23)
- [ ] **A tool that fails returns a JSON-RPC `error` envelope, not
      `result.isError: true`.** `jsonrpc::dispatch` wraps any `Err` from
      `registry.call` into `{"error": {"code": -32000, …}}`, and the success
      path hardcodes `"isError": false`. The MCP spec reserves JSON-RPC errors
      for *protocol* faults (unknown method/tool, malformed params) and asks
      tool-level failures to come back as a normal `tools/call` result with
      `isError: true`. So a transient `send_email` rejection (SMTP 5xx) looks to
      a strict client like a broken session rather than a failed send.
      `mcp-tool-executor` happens to paper over this (it maps `resp.error` to a
      `ServerError` string for the LLM), which is why it has not bitten in
      practice.
- [ ] **Owner's call.** (a) Keep the mapping but only for genuinely unknown
      tools/methods and return `isError: true` for handler failures; or (b)
      leave as-is and document it. Deferred: it is a protocol-conformance
      decision, not a crash or data-loss bug.
- [ ] **`mark_read` answers `{"status":"marked_read"}` for a UID that does not
      exist.** IMAP `UID STORE +FLAGS (\Seen)` on an absent UID is not an error —
      the server replies `OK` and the untagged `FETCH` simply never mentions it —
      and `mark_read` drains the response stream without looking at it. So
      `{"uid": 999999}` reports success for a message that was never touched
      (same "reported success for work that did not happen" shape as the
      2026-09-23 firmware entries). Fixing it means inspecting the untagged
      `FETCH` for the requested UID, which `async-imap` exposes only through the
      parsed response stream.
- [ ] **Owner's call.** (a) Parse the `STORE` response and report
      `not found` when the UID is absent; (b) do a cheap `UID FETCH <uid> (UID)`
      existence probe first; or (c) accept it, since the caller obtained the UID
      from a listing moments earlier. Deferred: the response-parsing change is
      non-trivial and risks false negatives on servers that omit the untagged
      `FETCH`.

### 🟡 P2 — `magent config set` type-checks but does not validate, so it can write a config `validate` rejects (2026-09-23)
- [ ] `config set <KEY> <VALUE>` performs only a *type* coercion
      (`as_usize` / `as_f32` / …) and writes the result; it does not run
      `validate_record`. So `magent config set sampling.temperature 9`
      succeeds (`sampling.temperature = 9`, exit 0) but the very next
      `magent config validate` reports
      `✗ sampling.temperature: temperature 9 is outside the conventional 0.0–2.0
      range`. The same holds for `provider.default bogus` and
      `provider.ollama.url localhost:1234` (no scheme).
- [ ] **Why it is not a P0.** `config validate` exists precisely to catch
      these, and `run` degrades gracefully (it warns and falls back to built-in
      defaults rather than crashing). Nothing is corrupted — the file is just
      left in a state `validate` will flag.
- [ ] **Owner's call.** (a) Have `set` run `validate_record` on the mutated
      record and refuse (with the same message) when the single change
      introduces a violation — the most surprising-to-a-user option, since a
      `set` that was previously a pure write would start failing. (b) Leave the
      separation as-is (type-check on write, semantic-check on `validate`) and
      document it in `magent config --help`. (c) Also print a `config validate`
      hint after any `set` that changes a validated key. Currently (b)-ish by
      default with no doc mention. Not changed here because it is a UX
      decision, not a correctness fix.

### 🟡 P2 — two CLI surface inconsistencies left after the `--json` fix (2026-09-23)
- [x] **`scheduler --timezone <TZ>` silently accepted any string** — fixed
      (2026-09-24). `resolve_timezone()` now returns a `Result`: `utc`/`UTC` →
      `Utc`, `local`/`LOCAL`/absent → `Local`, anything else →
      `ParseError::Usage("scheduler daemon: --timezone expects \`utc\` or
      \`local\`, got \`X\`")` (exit 2). Also removed two lies: the
      `SchedulerTimezone::Local` doc claimed it read `$MAGENT_TIMEZONE` (an IANA
      name), and `scheduler --help` advertised `<utc|local>` while the next
      clause promised IANA names — neither was implemented. The help now states
      the two accepted values and that a bogus one is rejected. The daemon usage
      synopsis also gained `--cron`/`--at`/`--timezone`, which it was missing.
      Pinned by `cli::tests::scheduler_rejects_a_timezone_it_cannot_honour`.
- [x] **Left alone, deliberately:** `--timezone` on `scheduler run-once` is still
      parsed and then ignored (only `daemon` consumes it). Rejecting it there
      would break anyone who passes it harmlessly today, and `run-once` runs the
      queue immediately regardless of zone.
- [x] **`magent version --json` emits a bare JSON string** — already fixed.
      This entry was stale: the live binary emits
      `{"name":"magent","version":"0.1.0"}`, so `jq '.version'` works. A test
      pins the object shape in `cli/tests/json_output.rs`, so the "bare string"
      description above (verified 2026-09-24) no longer describes the code.
- [ ] **`--json` never emits an *error* envelope.** On failure every command still
      prints human text to stderr and exits non-zero; there is no
      `{"error":…,"code":…}` object on stdout. A CI consumer that pipes stdout to
      `jq` therefore gets a parse error instead of a structured failure. Adding one
      is a cross-cutting change to the dispatch layer, so it is left to the owner.
- [x] **`--json help` still prints human text** — deliberate, now documented
      (2026-09-24). `help`, `run --help`, and the per-subcommand `--help` bypass
      `Output` (they `print!` a static string), so `magent --json help` is not
      valid JSON. Help is prose, not data. The invariant is now stated on
      `GlobalFlags::json` in `cli/src/cli.rs`: "`--json` ⇒ parseable stdout"
      applies to *data* commands, not all commands, so scripts should only rely
      on stdout JSON for commands that document a `--json` shape. Every data
      command satisfies it (verified in `cli/tests/json_output.rs`).

### 🟡 P2 — `--tool-max-chars` truncation could grow the payload (2026-09-24)
- [x] **The cap grew the content it capped, at small budgets** (fixed
      2026-09-24). `truncate_tool_content` writes `head + "\n[...truncated N
      bytes...]\n" + tail`; the marker's fixed ~29-byte cost is charged to the
      budget without being part of it, so for `max_chars` below that, the
      "truncated" string was *longer* than the original. The contract ("Cap
      each tool result content") inverted: lower cap ⇒ larger context.
      Measured via `magent run --repl --mock --tool-max-chars N` reading the
      `context` size — `1` → 3302 B, `10` → 3311 B, versus 3293 B with
      truncation off; all now 3293 B. The candidate is kept only when
      strictly shorter. Covered by `truncate_never_grows_the_content`.
- [x] **`CompressionStats::bytes_saved` reported the marker length** (fixed
      2026-09-24). It was `new_len - tool_content_max_chars` — the marker's
      own byte count, not the bytes removed — so the telemetry in
      `cli/src/summary.rs` and the `[Compress]` trace line was meaningless.
      It now sums `before - after`, matching the field doc and
      `examples/topic-watcher`. Covered by
      `compress_reports_the_net_bytes_it_removed`.

### 🟢 P2 — `--tool-max-chars` capped *bytes*, not chars — fixed (2026-09-24)
- [x] **Owner decision: option (b) — make the limit count characters.**
      `truncate_tool_content` compared `msg.content.len() <= max_chars`, i.e.
      bytes, so an 800 "char" budget kept only ≈266 CJK characters. It now
      counts `msg.content.chars().count()` and translates the 60/40 head/tail
      budgets into byte offsets on code-point boundaries
      (`byte_offset_after_chars`). Every name is now truthful:
      `--tool-max-chars`, `compression.tool_content_max_chars`, and the core
      parameter all mean characters. The `[...truncated N bytes...]` marker and
      `CompressionStats::bytes_saved` still report **bytes**, which is what
      they say.
      **Why (b) and not (a):** the flag name is not the only surface that says
      "chars" — the config key and the summary record's `policy.tool_content_max_chars`
      are on-disk formats (`config get/set`, saved `.json` summaries). Renaming
      them would silently discard existing values, whereas counting chars fixes
      the advertised contract and, for ASCII, changes nothing. The cost is that
      non-ASCII tool output now keeps ~3× more bytes — which is the intent of a
      character budget. Pinned by `truncate_caps_characters_not_bytes` and
      `byte_offset_after_chars_lands_on_code_point_boundaries`.
- [x] Docs realigned: `cli/src/cli.rs` (module doc + `run --help` + `config --help`),
      `docs/CONFIG.md` (the key table now says characters and notes the marker's
      bytes), `docs/CONTEXT_MANAGEMENT.md` (its "`content.len() > tool_content_max_chars`"
      description was the clearest statement of the old byte semantics).

### 🟠 P2 — the ESP32 firmware crate's five `#[cfg(test)]` modules can never run (2026-09-23)
- [ ] `firmware/esp32-app` is an `esp-idf` **bin** crate. Its inline test
- [ ] `firmware/esp32-app` is an `esp-idf` **bin** crate. Its inline test
      modules — `text_util.rs:25`, `wifi_manager.rs:333`, `latency_metrics.rs:229`,
      `main.rs:499`, `main.rs:529` — compile only for an Xtensa/`riscv` *device*
      target, and no host `cargo test` target can build them. `test-workspace.sh:7`
      builds the firmware with `cargo +esp build` and **runs no test step**, so the
      assertions in those modules have never executed in CI or locally. The
      `text_util` append regression fixed this pass was one such test: harmless to
      add, but it would not have caught the bug on its own.
- [ ] **Why it matters.** A green `./test-workspace.sh` says nothing about the
      firmware crate's pure logic (truncation math, latency accumulation, Wi-Fi
      state transitions). Those are exactly the sort of host-testable units where a
      regression is otherwise caught only on hardware.
- [ ] **Fix options** (needs owner buy-in — none is a P0):
      (a) **Extract the pure logic** (`text_util`, `latency_metrics`, the
      `wifi_manager` state machine) into a small `no_std` helper crate that both
      the firmware and a host `cargo test` can build — the cleanest, matches how
      `magent-core` already hosts the tested `boot_key` / `wifi_pass_seal`
      primitives; costs a workspace member.
      (b) **Run the device tests on an emulator** (`esp-idf` QEMU target) — adds
      real infrastructure and a QEMU toolchain to CI.
      (c) Leave as-is and rely on review + the host-tested `magent-core`
      primitives the firmware calls. This is the current state; the code comments
      in `device_key.rs` and `text_util.rs` now say so instead of implying
      coverage that does not exist.
- [ ] Related: `device_key.rs` has **zero** test coverage (its FFI helpers
      `read_btdk_material` / `derive_btdk` / `seal_dev_identity` / the sealed
      branch of `open_dev_identity` rely on review plus the host-tested
      `magent_core::boot_key` / `wifi_pass_seal` under them).

## 🟠 Verification backlog
- [ ] **USB-JTAG console risk (S3)**: switching the console primary to
      `CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG` **hijacks the native USB-JTAG**, making
      the board unflashable via USB (`esptool` fails with "No serial data
      received") — matching the warning already in `sdkconfig.defaults`. Reverted.
      **UPDATED 2026-09-16 — the same symptom is reproducible with the app-side
      USB-JTAG *ingress driver* (`UsbSerialAdapter::new` →
      `usb_serial_jtag_driver_install`), which the S3 build needs so `AT…` /
      agent commands work over the plain USB cable.** Observed once after ~10
      open/close + DTR/RTS reset cycles from the host: the port stayed
      enumerated but the console went silent and neither `esptool` (with or
      without `no_reset`) nor a fresh reset sequence could reach the ROM
      (`A fatal error occurred: Failed to connect to ESP32-S3: No serial data
      received`). It flashed fine dozens of times before and after that, so it
      is a race, not a hard failure.
      **Recovery**: press the board's **RST/EN** button (or unplug/replug USB) —
      the ROM then owns the peripheral again and flashing works. Host-side
      hygiene that lowers the probability: de-assert DTR/RTS before closing the
      port and avoid bursting open/close cycles (both now done by
      `tools/s3_full_test.py`).
      **Follow-up**: consider making the USB-JTAG ingress link opt-in
      (`AT+USBCONSOLE=` or a Cargo feature) for production images where UART0
      ingress or BLE is the primary command channel.
      **If done again**: enter download mode (hold BOOT + RST) to recover. The
      reliable AT path on this board is UART0 (physical) or the UART0-ingress
      over USB-CDC if the board bridges UART0 (verify).
- [x] **Workspace clippy debt** — **cleaned**: `cargo clippy --all-targets
      -- -D warnings` is clean, and `cargo test` is green (1978 passed, 0 failed),
      both over the workspace's `default-members`. Corrected 2026-09-20: the
      invocation is **not** `--workspace` — that pulls in the firmware crates and
      dies in `nrf52840-pac` (`error: invalid Mach-O section specifier`, from
      `#[link_section = ".vector_table.interrupts"]`), which is precisely why
      `default-members` excludes them. The earlier "1314 tests" figure was stale.
- [ ] REQ-VFY-005 — `cargo kani` 0 unknown (LLM HTTP offline verification).
- [ ] REQ-VFY-006 — workspace code coverage ≥ 80% (`cargo llvm-cov`; currently only
      `magent-lua` meets it).
- [ ] REQ-VFY-007 — hardware fuzz (`cargo-fuzz`) 0 crash in 1 h, per release.
- [ ] Tier 6 — on-hardware validation per `docs/TIER6_VALIDATION.md`
      (clean-boot log, OTA + rollback, Secure Boot eFuse, Flash Enc, TLS,
      ping) on a real ESP32-C61 board. (No BLE step — BLE was removed.)
