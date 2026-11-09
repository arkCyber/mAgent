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

### Fixed (2026-11-09, round 3) — web3: real RPC payloads, typed-tx encoding, VC proofs
- **Real JSON-RPC responses deserialise.** `Address`, `Hash`, and `Wei` had
  derive-only serde (20/32-element number arrays, JSON numbers), and
  `TransactionReceipt` / `EventLog` / `Block` / `BlockTransaction` used
  snake_case keys. No Ethereum node sends either shape, so every
  `eth_getTransactionReceipt`, `eth_getBlockByNumber`, and `eth_getLogs`
  response failed to parse on both the host and ESP32 backends. They now use
  hex-string serde (`0x…`) and `rename_all = "camelCase"`; the legacy array
  form is still accepted. `erc721`/`didset` placeholder topic constants were
  corrected/removed.
- **EIP-1559 transactions encode an empty access list correctly.** The `None`
  branch emitted `0x80` (RLP empty *string*); it must be `0xc0` (empty
  *list*). Every `.eip1559(…)` transaction was rejected by nodes with
  "expected input list".
- **`personal_sign_hash` hashes the raw message bytes.** It rebuilt the
  message through `String::from_utf8_lossy`, replacing every non-UTF-8 byte
  with 3-byte U+FFFD while keeping the original length prefix — so the digest
  disagreed with what the signer signed.
- **Verifiable Credentials now verify their Ed25519 proof.**
  `VerifiableCredential::verify_proof()` resolves the issuer `did:key`,
  requires `proof.verification_method` to name that issuer, and checks the
  signature over `encode_for_signing()`. `vc verify` (CLI) calls it, so a
  tampered `credentialSubject` is rejected instead of exiting 0.
- **`BindingProof::verify()` binds DID / address / chain to the signed
  bytes.** Editing `proof.did` (or `chain_id`) while keeping a valid
  signature used to return `Ok(true)`; it now returns `Ok(false)`.
- **ESP32 gas price is returned in wei,** matching the host backend and the
  shared `Gwei` formatter (it used to divide by 1e9 twice, showing
  `0.00 Gwei`).
- **ESP32 transaction polling can complete.** `check_transaction_status`
  overwrote `state` with `CheckingStatus`, making `poll`'s
  `WaitingForConfirmation` completion arm unreachable, so a mined transaction
  never reported `Completed`.

### Fixed (2026-11-09, round 3) — core state, summaries, and ingress
- **Signed ingress envelopes no longer drop large frames.** The envelope is
  `MAX_PAYLOAD` bytes but hex-encodes the payload (~2×) plus DID/signature
  overhead; a ~900 B frame failed serialisation and reported a misleading
  `PayloadTooLarge`. A new `IngressError::EnvelopeTooLarge` rejects it up
  front with the real bound.
- **`summary rollback` restores the snapshot's content.** `HistoryEntry` only
  stored `kept`, so rollback changed a counter and left the newest window and
  LLM summary active. Entries now persist `head_tail_window`, `llm_summary`,
  and full `stats`; legacy entries keep the old counter-only behaviour.
- **Summary locks/temp files recover from a crash.** A stale `.lock` (SIGKILL)
  or a leftover `<topic>.json.tmp` used to block every later save for that
  topic permanently. Both are now reclaimed/removed (30 s staleness, and the
  temp file is cleared under the held lock; `rename` failures clean up).
- **Wear-levelling accounting follows the chosen sector.** Static leveling
  never advanced `current_sector`, so every write counted against sector 0 and
  `is_worn_out()` fired after `max_writes` *total* writes despite an even
  spread.
- **`MemoryBackend` overwrite no longer resurrects deleted values.** A key
  sitting after a hole was re-written into the hole, leaving the original; a
  later `delete` removed the new copy and reads returned the stale one.
- **The stress log evicts rather than freezing at 100 entries,** and
  `Skill::to_injection_string` no longer truncates the content tail (its
  buffer now includes the Markdown header).

### Fixed (2026-11-09, round 3) — CLI concurrency, prompt store, vault, editor
- **`web3 new` / `web3 delete` and `config set` take an exclusive lock.** All
  three are whole-file read-modify-write operations; concurrent invocations
  silently kept only one change (four concurrent `web3 new` runs left one
  identity, losing the other private keys). A 30 s stale lock is reclaimed.
- **The web3 vault is written 0600** (was 0644 via plain `fs::write`) and uses
  a pid-unique temp file, so concurrent saves cannot rename each other's file.
- **`set-prompt list` skips non-record JSON files.** `set-prompt sign` writes
  `<name>.signed.json` into the same directory; one such file made the entire
  listing fail. `set-prompt import` now rejects a newer `schema_version`
  instead of rewriting it as v1 and dropping unknown fields.
- **`run --save-summary-overwrite` keeps history.** It deleted the previous
  record first, so run-produced summaries could never be rolled back.
- **A CSI sequence with modifiers is swallowed.** `ESC [ 1 ; 5 C` (Ctrl-Right)
  inserted `5C` into the line; the editor now consumes the full sequence.
- **`agent.reason` accepts text at exactly `MAX_REASON_LEN`** on both engines
  (the separator space is now included in the cap).
- **Examples:** `topic-watcher` truncation counts characters and never grows
  the body / underflows `bytes_saved`; `email-mqtt-bridge` escapes `from`,
  `to`, and `subject` (a quote in the subject produced invalid JSON).

### Fixed (2026-11-09, round 3) — Lua runtime and bundled apps
- **NVRAM works on the ESP32.** The store read a 2-byte header at `BASE` and
  the body at `BASE + 2`, but the device flash backend keys each exact address
  to its own NVS blob — a fresh device returned "key not found" (aborting
  `datalogger.lua` at boot) and every `get` after a `set` missed. The whole
  store is now one blob, a missing blob is an empty store, and short reads are
  bounds-checked instead of panicking.
- **Both engines expose the same sensor names and flash semantics.**
  `SimHardware` now accepts the firmware's substring aliases (`temp`/`die`,
  `mem`/`heap`/`sram`, …) and trims a flash read to the written extent, so
  `selftest.lua` boots and passes on the host.
- **`alarm.lua` escalates when the device boots out of band** (the IDLE state
  had no else-arm and never left IDLE).
- **`greenhouse.lua` clears its buzzer** when the temperature drops, and the
  bundled apps no longer double-actuate (a direct `hardware.*` call plus a
  recognised action string made `AppRuntime` drive a second pin).
- **A failed hot-reload is recorded, not silently "healthy",** and a reload
  clears a previous stop request.

### Fixed (2026-11-09, round 3) — firmware
Verified with `cargo check` for S3 (with `lua`) and C61.
- **UART ingress reassembles lines.** Each `poll` returned whatever was in the
  RX buffer, so a long `AT+LUAAPP=` / `AT+LLMCFG=` split across polls was
  rejected as a truncated command and its tail routed to the LLM as prose,
  while two back-to-back commands merged into one unparseable frame. The
  adapter now buffers and returns exactly one terminated line (bounded at
  1 KiB).
- **OTA anti-rollback confirmation moved to `mark_stable_boot()`.** Marking the
  image valid at agent-thread spawn meant a firmware that crashed during
  Wi-Fi/LLM/Lua init was never rolled back.
- **The Wi-Fi supervisor honours AT commands during backoff.** A credential
  backoff of up to 600 s slept in one call, so `AT+CWJAP=`/`AT+CWQAP=`/
  `AT+CWLAP` appeared to do nothing for ten minutes. The sleep is sliced at
  250 ms and checks all three request flags.
- **`AT+NTPSYNC` no longer records the 1970 boot clock** as an SNTP sample,
  and `read_rtc_unix` rejects implausible pre-2020 timestamps.
- **`AT+PING` reports milliseconds.** `ESP_PING_PROF_TIMEGAP` carries
  `elapsed_time_ms`; the reply labelled it `us` (25 ms read as "25us").
- **A bad NVS `main.lua` falls back to the embedded bootstrap** instead of
  ending the Lua thread, and a failed agent task now replies over the ingress
  link that issued it.

### Security (2026-11-09, round 2) — dependency advisories and a real SSRF bypass
- **`cargo deny check advisories` is clean.** Upgraded `webpki-roots` 1.0.9
  (contained a distrusted root whose private key is public) to 1.0.11,
  `rustls` 0.23.39 → 0.23.46, `rustls-webpki` 0.103.15 → 0.103.16, `tokio`
  1.53.1 → 1.53.3, and `rumqttc` 0.24 → 0.25.1 (which drops rustls 0.22 /
  tokio-rustls 0.25). `pin-utils` (maintenance-status advisory, no
  replacement path) is documented in `deny.toml`. MQTT tests pass on 0.25.
- **The host SSRF classifier had several bypasses.** It accepted
  `http://localhost./` (FQDN trailing dot), `0.0.0.0`/`::` (unspecified),
  IPv6 unique-local `fc00::/7`, and parsed
  `http://127.0.0.1\@example.com/` as `example.com` while the HTTP client
  dialled loopback. `public_fetch_target_refusal` now normalises the trailing
  dot and rejects unspecified / 0.0.0.0-8 / broadcast / multicast / ULA
  addresses; `parse_http_authority` treats `\` as an authority terminator
  (WHATWG rules). A new `validate_resolved_fetch_target` resolves the host and
  vets every address before dialling, closing name-based bypasses such as
  `127.0.0.1.nip.io` and numeric spellings.
- **Redirects are vetted before they are sent.** `web`'s reqwest client used
  `Policy::limited(5)`, which dials every hop internally; the guard could only
  re-check the final URL *after* the GET (and its side effects). The client now
  uses `FollowNone` and `blocking_get_with_meta` walks up to 5 hops with
  `resolve_http_redirect` + the resolver guard on each one. `AT+OTA` has the
  same fix (`FollowNone`, a 3xx fails the `status != 200` check), so an
  `https://` URL can no longer 302 the download onto plaintext HTTP.
- **AES-GCM nonces no longer restart from zero every boot.** The 96-bit nonce
  was `counter(4) || msg_len(8)` over a compile-time key; the first message of
  every boot therefore reused the same (key, nonce) pair for the same length.
  It is now `boot_salt(4) || counter(4) || msg_len(4)`, with the salt mixed
  from the process id/sub-second clock on hosted builds (documented as zero on
  the unused `no_std` path).
- **Empty vault passphrases are refused.** `MAGENT_WEB3_PASSPHRASE=""` was fed
  to Argon2id and created a "protected" vault under a well-known empty key;
  both the create and sign paths now return a usage error.

### Fixed (2026-11-09, round 2) — core data integrity and agent I/O
- **`KvStore::set` updates were permanently invisible.** `set` appends and
  `get` returned the *first* match, so overwriting a key stored the new value
  but every read kept returning the original (and duplicates accumulated until
  `OutOfSpace`). `get` now returns the newest match.
- **`unacknowledged_alerts()` / `critical_alerts()` panicked with >16/8
  entries.** They `collect()`ed into fixed `heapless::Vec`s whose capacity is
  smaller than the 50-entry history, so 17 abnormal heart-rate samples aborted
  the device (`Vec::from_iter overflow`). They now push and stop at capacity.
- **Heart-rate reserve and exercise threshold saturated.** `age > 220` gives
  `max_hr == 0`; `max_hr - resting_hr` panicked in debug and wrapped in
  release, producing nonsense target zones. Both subtractions are saturating.
- **Sleep cooldown arithmetic saturated.** A backwards clock (reboot / u32 ms
  wrap) made `current - last_offer` wrap and bypass the 15-minute cooldown.
- **Monitoring's rolling average no longer overflows `u32`.** The running sum
  is computed in `u64`.
- **`AT+TIMEZONE` survives a reboot again.** `restore_from_nvs` replaced the
  live `TimeSync` (built with the NVS offset) with one parsed from a record
  format that hardcodes `tz = 0`; it now carries the live offset across.
- **The Ollama request serializer escapes JSON and emits tools.**
  `serialize_request` concatenated model output verbatim, so a quote or newline
  produced invalid JSON, and it dropped `request.tools` entirely — `add_tools`
  was a no-op. Both are fixed and tested against `serde_json`.
- **A URL with `=` is no longer parsed as a named AT argument.**
  `AT+HTTPGET=http://x/?q=1` became `Named { key: "http://x/?q", val: "1" }`
  and was rejected by the validator; the `key=value` heuristic now requires a
  bare identifier (alphanumeric/underscore, ≤32 chars).

### Fixed (2026-11-09, round 2) — MCP transports and tool results
- **MQTT's event loop survives an error and reconnects.** `while let Ok(poll)`
  exited on the first network error, dropping the event loop and its request
  channel — every publish/subscribe failed for the rest of the process after a
  single broker blip. The loop now logs, waits 1 s, and polls again.
- **A timed-out email call restarts the MCP subprocess.** `McpExecutor` only
  reset the child on `ChildExited`; a hung child stayed "alive" and every
  later call paid another 30 s timeout. `Timeout` now clears the slot too.
- **`--show-config` no longer prints the MQTT password** (`skip_serializing`),
  partial `mqtt-mcp.toml` files now load (`serde(default)`; an explicit
  `qos_default = 0` survives, and an invalid file value is rejected rather
  than silently ignored).
- **The MQTT demo protocol decoders no longer panic on truncated packets.**
  `PUBLISH`/`SUBSCRIBE` bodies shorter than two bytes indexed `body[1]`;
  both `protocol.rs` copies now return `Truncated` (regression test in each).
- **The sync `ToolExecutor` works inside a tokio runtime.** It called
  `Handle::block_on` on the *current* runtime, which panics ("Cannot start a
  runtime from within a runtime"). It now always uses the dedicated runtime,
  driving it from a scoped helper thread when the caller is already inside one
  (tested with `#[tokio::test]`). The stdin writer is borrowed rather than
  `take()`n, removing the cancellation path that left `stdin == None`.
- **MCP result truncation counts characters**, matching its name and docs
  (CJK results no longer lose ~⅔ of their budget); `email-mcp` parses `to:`
  with `lettre::Mailboxes`, so `"Doe, John" <john@example.com>` is accepted.

### Fixed (2026-11-09, round 2) — CLI contracts
- **`web3 bind verify` / `vc verify` exit non-zero on an invalid proof.** They
  printed `✗` and returned `Ok(())`, so CI treated a tampered binding or an
  expired credential as success.
- **`summary rollback` actually removes the restored entry.**
  `FileSummaryStore` merges the on-disk history into every save; rollback's
  carefully built post-state was merged back, re-adding the entry and growing
  history. A new `save_overwrite` (no merge) is used by rollback.
- **`run --save-summary-overwrite` keeps the previous snapshot in history.**
  It deleted the record first, so run-produced summaries could never
  accumulate history and `summary rollback` was unusable on them. It now takes
  the same merge path as `summary save --overwrite`
  (`run_overwrite_keeps_history_for_rollback`).
- **Scheduler tasks are passed after `--`.** A task beginning with `-`
  (`"--help"`, Markdown lists) was parsed as a flag by the child; `--help`
  printed usage and exited 0, so the scheduler reported success without
  running the agent.
- **`--json scheduler daemon` writes one document.** Each tick emitted its own
  JSON envelope; stdout now carries only the final outcome envelope when the
  daemon stops (tested by parsing the stream).
- **`doctor` accepts `llama3.2:latest` for `OLLAMA_MODEL=llama3.2`** (the tag
  `ollama pull` actually installs), instead of a false `model=FAIL`.
- **`config set` types by target field.** `provider.ollama.model 1234` /
  `metadata.author true` are stored as strings; a type-mismatch guess retries
  as a literal string. Numeric/boolean fields still parse normally.
- **Config and prompt writes are atomic** (temp file → fsync → rename),
  so a crash cannot leave a truncated JSON file that silently falls back to
  defaults. The config file is re-chmodded 0600 on every save.
- **The CSI parameter parser saturates** instead of overflowing `u32` on
  `ESC [ 99999999999 ~` (debug-build panic).

### Fixed (2026-11-09, round 2) — Lua engines and HAL
- **`PiccoloVm::call` converts nil/number/boolean returns** like the mlua
  engine. It demanded `String`, so an `on_tick` that returns nil (the normal
  action-hook shape) raised a TypeError every tick and never dispatched.
- **Both engines expose the same `hardware.*` surface**:
  `flash_erase_sector` added to mlua, `i2c_transfer` added to piccolo, and
  piccolo now enforces the same I2C/BLE/`agent.reason` payload caps.
- **Flash writes report a non-erased target.** NOR flash can only clear bits;
  the HAL simulators silently AND-ed, so writing `"WORLD"` over `"HELLO"`
  returned success and read back `"@E@LD"`. All four write paths now return
  `NotErased` (or the simulator equivalent); `flash_erase_sector` then makes
  the write legal. HAL flash bounds use `checked_add` (32-bit wrap) and GPIO
  adapters reject pins beyond the chip's range (nRF 48, ESP32-S3 49).
- **piccolo's `string.match` is bounded and more correct.** A comparison
  budget stops the O(n·m) scan that fuel could not preempt (two ~200 KiB
  strings froze the S3 Lua thread). Group captures span the whole group
  (`(ab)` → `"ab"`), group quantifiers work (`(ab)+`), and `%$` is a literal
  rather than the end anchor. `string.format` precision is capped at 32
  (`%.1000000000f` tried to allocate ~1 GiB outside the arena limit).
- **The nRF52 simulator's step counter no longer latches at 1.** The pulse
  condition was always true (`fract()` of an integer is 0) and the release
  threshold (9.75) sat below the resting value, so only the first pulse ever
  counted and activity stayed `Walking`; `spo2_sensor.update()` was missing
  from `tick()`. Steps now accumulate (~1 per 10 ticks).

### Fixed (2026-11-09, round 2) — firmware
Verified with `cargo check` for S3 (with and without `lua`) and C61.
- **`AT+OTA`'s worker has a 24 KiB stack.** It ran on the ~3 KiB default
  pthread stack and then performed a TLS download with a 2 KiB chunk buffer —
  a guaranteed stack-protection reset during the handshake.
- **`AT+OTA` refuses redirects** (`FollowNone`), closing the `https://` →
  `http://` downgrade described above.
- **The LLM retry chain fits its caller's deadline.** Two 8 s attempts plus a
  300 ms pause is ~16.3 s; the caller gave up at 10 s, so the retry could never
  deliver its result. The wait budget is now 20 s (under the 30 s heartbeat).
- **A completed SNTP sync is recorded before the status is reset.** IDF's
  `sntp_get_sync_status()` consumes `COMPLETED`; the link-up path reset it and
  waited for a fresh poll, discarding a sample that could take an hour to
  repeat. It now records the completed status first.
- **`spawn_with_profile` returns `io::Result` instead of panicking.** An ENOMEM
  thread spawn (known on the S3) aborted the board from the ingress thread;
  `AT+RST`/`AT+RESTORE` fall back to an immediate reboot, `AT+OTA` answers
  `+CMDER:6`.

### Docs / verification (2026-11-09, round 2)
- **`REQ-SCHED-001` is registered in `docs/SRS.md`.** The SRS trace checker
  failed with "requirement markers not registered" even though the code,
  `docs/DUAL_CORE_SCHEDULING.md`, and the firmware watchdog header all cite it;
  the register row now describes the Core-1 isolation / fed-watchdog /
  byte-GC contract. `docs/SRS_TRACE.md` was regenerated.
- **`cargo deny check advisories` and `tools/ci/dangerous_patterns.sh` are
  green** (the only non-fatal line is Miri's known macOS `kqueue` FFI refusal).

### Security (2026-11-09) — the Lua sandbox's base-library escape and the identity-seed integrity gap
- **`load` / `loadfile` / `dofile` are no longer reachable from a sandboxed
  script.** mlua cannot deselect the Lua *base* library (`luaopen_base` is
  unconditional), and Lua 5.4's base library defines all three chunk loaders:
  `dofile`/`loadfile` read and execute files through C `fopen` (bypassing the
  absent `io`/`os` libs), and `load` accepts *binary* chunks by default, which
  re-opened the malicious-bytecode path that `ChunkMode::Text` closes for the
  outer chunk. `sandbox::new_sandbox` now sets all three globals to `nil`
  (`sandbox_blocks_runtime_chunk_loaders`).
- **The instruction budget can no longer be swallowed.** The hook debited with
  `fetch_sub`; a zero counter wrapped to `u64::MAX`, so a script that caught
  the first budget error with `pcall` disabled the budget for ~1.8e19
  instructions. The debit is saturating and the exhausted state is sticky, and
  `pcall`/`xpcall` are wrapped so they re-raise a budget error instead of
  returning it to the script (`pcall_cannot_swallow_the_instruction_budget`).
- **The device identity seed and the default AP passphrase are now sealed with
  DBO2 (integrity-checked), not DBO1.** `device_key::seal_dev_identity` and
  `main::seal_and_store_wifi_pass` wrote DBO1, whose XOR stream has no MAC: a
  bit-flip in `magent:dev_identity` silently yielded a different seed — hence a
  different device DID/wallet — with no error anywhere. Both write paths now
  use `wifi_pass_seal_v2::seal_str`; the BTDK1 opener accepts both formats and
  migrates a DBO1 payload forward in place. `DEV_IDENTITY_STORED_MAX` grew from
  163 to 195 bytes accordingly.
- **`seal_*` now rejects a non-full-width nonce.** Both seal functions accepted
  any non-empty nonce while both openers decode exactly `NONCE_LEN` bytes, so a
  short nonce round-tripped silently to garbage (DBO1) or produced an
  unopenable value (DBO2). `SealError::BadNonceLength` is returned at the seal
  boundary (`non_full_width_nonce_rejected_on_seal`, plus the v2 and
  `seal_secret` variants).

### Fixed (2026-11-09) — CLI: argument handling, cron startup, JSON contracts
- **A flag token is no longer swallowed as another flag's value.**
  `magent run --model --mock "hi"` consumed `--mock` as the model name and
  contacted the real backend instead of the simulator; `-m --mock`,
  `--api-key --mock`, etc. had the same defect. The generic long/short value
  paths now reject a following `-…` token (`MissingValue`); `--flag=value` and
  a lone `-` still work (`a_following_flag_is_not_swallowed_as_a_value`).
- **`--` ends option parsing.** A task beginning with `-` (`magent run --
  "-5 degrees"`) was unrepresentable — `--` was read as the empty long-flag key.
  Everything after it is now positional (`double_dash_ends_option_parsing`).
- **Non-UTF-8 argv exits 2 instead of panicking (101).** `std::env::args()`
  panicked on an invalid-Unicode argument and the hook reported it as an
  internal bug. `args_os()` + explicit rejection routes it through the
  documented usage-error path.
- **`scheduler daemon --cron` no longer runs the queue at startup.** Both
  schedule kinds ran a tick before computing the first sleep, so
  `--cron "0 0 1 1 *"` fired today and then again on schedule. The first Cron
  tick is now the next matching instant (Interval remains immediate).
- **`scheduler run-once` rejects `--interval` / `--cron` / `--at`.** They were
  parsed and dropped, so a scheduled-looking invocation silently ran once.
  (`--timezone` is still accepted; it has no effect on an immediate run.)
- **A failing `--sign` step no longer leaves a success-shaped JSON envelope.**
  Signing ran after the success document was flushed, so exit 1 came with
  `{"answer":…}` on stdout and no `ok:false` (the output latch suppressed it).
  Signing now happens before any stdout document is written.
- **`--json summary rollback` emits a document.** It had only a Human branch and
  exited 0 with empty stdout.
- **`--email-tools` is no longer silently ignored on a build without the
  feature.** The backend label and tool catalogue now come from
  `executor.has_email_tools()` (the cargo feature), and the flag prints a
  warning when it cannot be honoured.
- **A missing/unreadable `--prompt` file exits 2**, matching the documented
  "rejected input" taxonomy (it exited 1).
- **`run --verify-signed <PATH> <task>` is rejected.** Verification returns
  before the task is read, so the task was silently discarded.
- **`--temperature nan` / `inf` is rejected.** It parsed as `f32` and reached
  the core's hand-written JSON encoder as the literal `NaN`, which providers
  reject.
- **A minimal hand-written config now loads.** `schema_version`,
  `created_at`, and `updated_at` lacked `serde(default)` although the module
  documents every field as optional, so omitting any of them rejected the whole
  file. `save` also now `chmod`s an existing config to 0600 — the old
  `OpenOptions::mode(0600)` only applied at creation, leaving a pre-existing
  0644 file world-readable.

### Fixed (2026-11-09) — the agent loop lost answers, dropped the current request, and executed data
- **An over-long model reply is an error, not an empty answer.**
  `add_message` coerced a >`MAX_BUFFER_SIZE` reply to `""`; `think` still marked
  the task `Finished` and `run()` returned `Ok("")`. Over-long LLM tool names /
  arguments had the same `unwrap_or_default()` shape, which executed the tool
  with no arguments (e.g. `fetch_web` with no URL). Both now return
  `InputValidationFailed`.
- **The generic JSON tool-call fallback no longer executes arbitrary object
  keys.** `{"result":"25.5 C","data":{…}}` was parsed as a call to a tool named
  `data`, and tool parsing runs before result parsing, so the real answer was
  discarded. A response carrying `result`/`error`/`reasoning`/`thought` is no
  longer scanned for format-3 calls, and a bare key must be an advertised tool
  or carry an explicit `"args"` object. The common OpenAI single-call shape
  `{"name":…,"arguments":…}` is now parsed correctly (the fallback picked
  `arguments` as the tool name).
- **A malformed tool call no longer aborts the whole embedded task.** A
  registry `Err` (missing argument, unknown tool) propagated out of `run()`,
  so the model never saw what it got wrong. It is recorded as a failed tool
  result and the loop continues, matching `RealAgentRunner`.
- **`ToolResult.error` is included in the conversation.** Refusals carry their
  reason in `error` with empty `data`; the agent recorded a bare
  `Tool error: ` and repeated it as the final answer.
- **Byte-GC preserves the current user request.** `gc_to_budget` dropped the
  oldest non-system message, which is the active user turn; with compression
  disabled and a >budget tool output, the next model call had only the system
  prompt. The last user message is now preserved alongside the newest message.
- **The prompt advertises `sensor=memory`**, which the executor already served
  but the tool description omitted. An over-long tool description is now
  truncated + logged instead of silently becoming `""` (which removed the whole
  tool from the prompt).
- **Out-of-range GPIO pins are rejected, not redirected.** The natural-language
  heuristic collapsed "pin absent" and "pin invalid" into `None`, so "set gpio
  500 high" drove the default pin 13; accepted pins are now bounded to
  0..=48. The simulator no longer casts `pin`/`address` with `as u8`/`as u32`
  (`pin=256` became pin 0; a negative address wrapped).

### Fixed (2026-11-09) — Lua host, simulator and HAL bounds
- **`hardware.i2c_transfer` bounds its receive length.** It took `rx_len`
  straight to `Vec::with_capacity`; a huge value aborted the host
  (`out_of_range_read_length_is_an_error` now covers it).
- **piccolo honours `^`/`$`.** The anchors were parsed and ignored, so
  `string.match("xabc", "^abc")` returned a match and validation patterns could
  be bypassed.
- **piccolo range-checks its integer arguments.** `as u8`/`as u32` truncated
  them, so `gpio_write(256,1)` drove pin 0 and `i2c_write(0x148,…)` addressed
  0x48 — different hardware actions from the same script on the mlua engine.
- **The nRF52 simulator reads nested `"args"` objects.** Its generated calls
  put every argument under `"args"` while the parser read only top-level keys,
  so "turn the LED off" drove GPIO13 **high**, "unlock the door" locked it, and
  `read_sensor sensor=heart_rate` read the temperature. It also parses
  `{"done":true}` (it looked for the uppercase `"DONE"`), so completed tasks no
  longer run to `max_iterations` (~20 extra LLM calls in Ollama mode).
- **`Trng::next_range` handles inverted/full ranges** (it underflowed and then
  divided by zero), and `Trng::new(0)` no longer returns zero forever.
- **`magent-email-mcp` conforms to the MCP tool-error contract.** A handler
  failure returns a normal `tools/call` result with `"isError": true`; JSON-RPC
  errors are reserved for an unknown method/tool. `search_emails` rejects a
  query that reduces to empty after IMAP escaping (it degenerated into "list
  the whole INBOX"), and `mark_read` probes the UID first instead of reporting
  success for a message that does not exist.

### Fixed (2026-11-09) — firmware: destructive queries, migration, and I/O bounds
Verified with `cargo check` for ESP32-S3 (`board-s3`, with and without `lua`)
and ESP32-C61 (`board-c61`) against the real ESP-IDF bindings.
- **A `?` query no longer triggers a destructive action.** No-argument action
  arms ignored `AtCommandKind`, so `AT+RESTORE?` erased NVS, `AT+IDENTROT?`
  rotated the identity, `AT+RST?` rebooted, and `AT+CWQAP?` disconnected.
  `dispatch_inner` now rejects a query on an op with no documented query form;
  `AT+MACRAND?` reports the current MAC instead of randomising it.
- **`AT+WIFIPASSUPGRADE=1` migrates a plaintext entry.** `open_sealed_v2`
  returns `LegacyPlaintext(s)` with an empty buffer, so the old
  empty-buffer check answered `+CMDER:4` — the exact case the command exists
  for (the WPA2 passphrase stayed unsealed).
- **The HTTPGET worker cap bounds live workers.** The RAII slot guard lived in
  the dispatcher, so it was released when the 12 s timeout returned while the
  worker kept running; each timeout could start another 24 KiB-stack thread.
  The guard is now owned by the worker closure.
- **An intentional reboot no longer counts toward the crash loop.**
  `check_and_advance_crash_counter` advanced on every boot, so three `AT+RST`s
  (the documented provisioning sequence) inside the 60 s stable window tripped
  safe mode and the next boot skipped Wi-Fi. Only panic/watchdog resets count;
  cold and software resets clear the counter.
- **`fetch_web` vets the resolved address, not just the hostname.**
  `http://127.0.0.1.nip.io/` is not an IP literal and resolves to loopback; the
  guard now rejects any resolved address that is non-public (DNS rebinding
  between the check and the client's own resolution remains theoretical).
- **`AT+SIGN` reports the payload bound it actually enforces.** The fixed
  `1..=64` check promised requests that can never fit the 256-byte reply line
  (DID + keys + 128-hex signature leaves ~11 payload bytes). The bound is now
  derived from the identity's serialised form and named in the error.
- **`flash_read` returns a prefix for a short read.** ESP-IDF returns
  `ESP_ERR_NVS_INVALID_LENGTH` when the buffer is smaller than the stored blob,
  so `length=5` over a 100-byte blob failed instead of returning 5 bytes. The
  read now uses a full-blob buffer (capped at 4 KiB) and truncates.
- **`AT+CWJAP=` writes the password before the SSID** and restores it if the
  SSID write fails, so a failed second write no longer leaves the new SSID
  beside the old passphrase (AUTH_FAIL loop).

### Added (2026-11-08) — a run can now be bounded in wall-clock time, not just in steps
- **`max_iterations` and `max_tool_calls` bound the *count* of steps, so a run
  against a slow backend could take `iterations × (1 + retries) × client
  timeout` — 30 minutes with the defaults — before `magent run` gave up.**
  `RunnerConfig` grew `max_run_duration: Option<Duration>` (default `None`:
  the historical behaviour, no time bound), checked at the head of every ReAct
  iteration. When it fires it emits
  `BudgetExhausted { kind: "wall_clock_ms" }` and the no-answer error names the
  budget (`… (iterations: 0/10, tool calls: 0/8), wall-clock budget of 0s
  exhausted`) instead of reporting only the untouched counters.
- The CLI exposes it as `--timeout <SECS>` (`0` and non-numbers are usage
  errors, like the other budgets) on both the one-shot and `--repl` paths; a
  REPL turn gets a fresh budget, because the timer is per `run_continuing()`
  call. It is deliberately **not** in the config file: the wall clock is a
  per-invocation property, not a persisted default.
- Scope: the check runs between steps, so it bounds many slow model replies; an
  in-flight HTTP request is still bounded by the client's own timeout, as
  before. Regression tests:
  `a_zero_wall_clock_budget_stops_before_the_first_call` (asserts the mock
  backend was never called, the trace event carries `wall_clock_ms`, and the
  error names the budget), `a_generous_wall_clock_budget_leaves_a_run_alone`,
  `parses_timeout_seconds`, `rejects_a_timeout_that_cannot_bound_a_run`, and
  `build_runner_propagates_the_wall_clock_budget` (without it the flag would be
  decorative).

### Added (2026-11-08) — the agent config is now persisted in ESP32 NVS
- **`config_store` had no production caller.** It now backs the ESP32's boot
  configuration: `firmware/esp32-app/src/config_nvs.rs` implements
  `config_store::StorageBackend` over ESP-IDF NVS (namespace `magent_cfg`,
  keys are the store's names, each op opens/closes its handle so a failed
  write leaks nothing), and `run_agent_loop` boots from
  `config_nvs::load_or_default()` instead of a hard-coded builder.
- **New operator surface `AT+AGENTCFG`** (parsed in `magent_core::at`,
  validated in `magent_core::at_validate`):
  - `AT+AGENTCFG?` → `+AGENTCFG:<name>,<max_iter>,<max_mem>,<watchdog_secs>,<ble_secs>,<skills>,<max_skills>,<debug>`;
  - `AT+AGENTCFG=<eight fields>` → validate (`AgentConfig::validate`, so the
    ranges and the board memory ceiling apply) and persist; **applies on the
    next boot**, matching `AT+LLMCFG`'s contract;
  - `AT+AGENTCFG=RESET` → delete the entry, returning to board defaults.
  Booleans are `0`/`1`; a partial form is `+CMDER:4`/`:8` rather than a
  silent "keep the default". A corrupt stored blob is logged and falls back
  to the board defaults at boot (so it cannot brick the agent thread), while
  `AT+AGENTCFG?` reports the failure instead of presenting defaults as the
  stored value.
- **Fixed the AT op-table guards while adding the variant.** The
  `classify_op_covers_every_atop_variant` table was already missing `OTA`,
  `FLIGHTCFG` and `LUAAPP`; its fixed count of 34 happened to equal
  "table entries + the two parser-special ops", so the guard could not
  detect the drift. The table now lists every variant and asserts 38.
  `tests/at_tests.rs::all_documented_verbs_parse` had likewise drifted to 31
  entries and omitted eight documented verbs (including `OTA`, `TIME`,
  `TIMEZONE` and `LUAAPP`); it now covers 39. Also removed the unused
  `MAX_PARAM_LEN` constant — its doc claimed a 64-byte clamp the parser never
  applied (and the real per-command caps are larger: API keys 128, URLs 512). New
  parser/validator tests: `parses_agentcfg_query_and_set`,
  `agentcfg_accepts_the_documented_eight_field_form`,
  `agentcfg_rejects_a_partial_form`,
  `agentcfg_rejects_values_agent_config_would_refuse`,
  `agentcfg_rejects_a_non_numeric_field`,
  `agentcfg_reset_sentinel_is_exactly_one_reset_token`.
- **Deliberately not touched:** the CLI's `magent config` layer. It persists
  its own `ConfigRecord` schema as JSON (provider/sampling/runner/…) and never
  consumed the core `AgentConfig`; folding this store in would add a second
  schema rather than share one. Recorded in `docs/BACKLOG.md`, along with the
  nRF52 follow-up (needs its own `StorageBackend`).
- **Verification:** the core parser/validator/store logic is host-tested
  (`cargo test -p magent-core --features std`, clippy `-D warnings` clean).
  The firmware-side NVS glue could not be compiled in this environment (no
  `esp` toolchain / `riscv32imac-esp-espidf` artifacts); it needs the CI
  `cargo check` for C61/S3 before this is called hardware-verified.

### Fixed (2026-11-08) — the ESP32 tool path substituted a pin/address instead of failing
- **`write_gpio` read its pin as `kv(…, "pin", "13").parse().unwrap_or(13)`, so
  a model that omitted `pin` or sent `pin=abc` drove GPIO13 on real hardware.**
  `flash_write` / `flash_read` did the same with `address=0` — a real, writable
  NVS key — so a malformed call read or overwrote it. The core simulator had
  already been fixed to treat both as `MissingField` / `InvalidValue` errors
  (`a_missing_pin_or_flash_address_is_an_error_not_a_zero`,
  `required_arg_parse_reports_an_invalid_value`); the firmware had not.
- `write_gpio` now refuses a missing or non-numeric pin with a failed tool
  result, accepts only `state=high|low|1|0` (with `on`/`off` aliases), and
  rejects anything else — previously every unrecognised state was silently
  driven low. `flash_write` / `flash_read` require `address`.
- Verification is `cargo check` only (the firmware has no host test target);
  the change is argument validation in `local_tools.rs`, touching neither the
  GPIO call sequence nor the NVS API. Recorded in `docs/BACKLOG.md`, including
  what is still open (a board-specific safe-pin allow-list needs a hardware
  fact this repo does not record).

### Fixed (2026-11-08) — AT op keywords are case-insensitive, and `AGENTCFG` rejects a comma in the name
- **The parser folded only the `AT` prefix, not the op keyword**, although
  `is_at_line`/the module docs describe the command surface as
  case-insensitive: `at+gmr` and `AT+CwMode=1` were rejected as unknown ops
  while `at`/`ate1` worked. `classify_op` now folds a bounded copy of the
  keyword (`MAX_OP_KEYWORD_LEN = 24`); arguments are untouched, so a model
  name or API key keeps its case. Pinned by `op_keywords_are_case_insensitive`
  (including a mixed-case `AT+LLMCFG=DeepSeek,Sk-AbC` whose args must survive
  verbatim).
- **`AT+AGENTCFG` now rejects a comma in `name`.** The query reply
  (`+AGENTCFG:<name>,<7 fields>`) is comma-separated and unquoted, so a stored
  name containing a comma could never be read back and re-sent. It is a
  `+CMDER:4` at the validation boundary rather than a name that silently
  breaks the command's own round-trip. Pinned by
  `agentcfg_rejects_a_comma_in_the_name`.
- **ESP-AT's `=?` test form is now classified as `Test`, not a zero-argument
  `Set`.** The parser consumed the `?` inside `parse_set_args` and returned
  `Set`, so `AT+CWMODE=?` ran through the set path with no arguments (every
  current handler rejected it, but the shape was wrong and
  `AtCommandKind::Test` had no producer). The firmware dispatcher now answers
  the form uniformly with `+CMDER:4`; no command implements parameter
  discovery, and the docs say so instead of implying the values would be
  listed. Pinned by `parses_the_test_form_distinctly_from_a_set`.

### Changed (2026-11-08) — ESP32-S3 focus: board/profile split, real USB link state, Lua guards
- **`board-s3` no longer implies the 8 MB `s3-8mb-psram` memory profile.** The
  shipped `build-s3.sh` board is the **quad-PSRAM 2 MB** dev board
  (`sdkconfig.defaults`), but the feature mapped to `magent-core/s3-8mb-psram`
  (4 MiB configurable / 2 MiB dynamic-context ceilings) — a ceiling above the
  physical pool. `board-s3` now selects only the chip (`esp-println/esp32s3`);
  the separate firmware feature `s3-8mb-psram` enables the large-heap profile
  for the **N8R8 8 MB octal** module, which also needs `sdkconfig.s3.defaults`.
  `BOARDS.md`, `README.md`, `docs/ESP32_C61_BUILD.md`,
  `docs/DUAL_CORE_SCHEDULING.md` and `docs/LUA_SCRIPTING_S3.md` now state the
  board/profile pairing instead of claiming 8 MB everywhere. The quad board
  therefore uses the same 1 MiB / 512 KiB ceilings as the C61 — correct for
  both boards' 2 MB PSRAM.
- **`UsbSerialAdapter::is_connected` returns the peripheral's real state**
  (`self.inner.is_connected()` → `usb_serial_jtag_is_connected()`) instead of a
  constant `true`. `IngressGateway::ingest` skips disconnected adapters, so the
  constant made it poll a port whose host was gone (benign for input, but the
  diagnostics claimed a live link).
- **`configure_psram_thread_stacks()` → `log_stack_allocation_policy()`.** The
  old name promised an `esp_pthread` configuration the function deliberately
  never performs (stacks stay in internal DRAM to avoid the PSRAM-vs-Wi-Fi
  `CPU_LOCKUP`). Behaviour unchanged; the name and doc now match.
- **`lua` is compile-time S3-only.** The Lua host was gated on the `lua` feature
  alone, so `board-c61,lua` would have started a runtime the docs call
  S3-only. `main.rs` now emits `compile_error!` for `lua && !board-s3`.
- **New `firmware/esp32-app/check-s3.sh`.** One-command cross-target S3
  compile check: sources espup's `export-esp.sh` when present, sets
  `MCU`/`SDKCONFIG_DEFAULTS`/`CC`/`CXX`/`BINDGEN_EXTRA_CLANG_ARGS`, and runs
  `cargo check --target xtensa-esp32s3-espidf` on the first usable Xtensa
  toolchain (`+esp`, then `+esp-rust`). `--lua` also compiles the Lua host;
  `--octal8mb` swaps in `sdkconfig.s3.defaults` and adds `s3-8mb-psram`.
  `build-s3.sh` gained the same `--octal8mb` mode and toolchain detection, and
  prefers `xtensa-esp32s3-elf-gcc` from `PATH` over the macOS PlatformIO path.
  Documented in `BOARDS.md` / `README.md`, including the manual
  `rust-1.97.0.0` + `rust-src-1.97.0.0` fallback when `espup install` is
  interrupted by a proxy.
- **The reserved-pin map now follows the PSRAM profile.** On the N8R8 octal
  module, GPIO33–37 carry the PSRAM; driving one breaks the bus exactly like a
  flash pin. `local_tools::reserved_pin_reason` refuses `33..=37` under
  `#[cfg(feature = "s3-8mb-psram")]` (the `--octal8mb` builds), while the quad
  dev board keeps them as ordinary GPIOs. Both the agent `write_gpio` tool and
  the Lua GPIO/PWM/ADC bindings consult the same map.
- **`AT+LUAAPP` can now upload a real app.** One AT line is capped at 256
  bytes, so the documented "encode the file, paste the line" flow could only
  carry ~190 bytes of Lua. The value form `AT+LUAAPP=+<base64>` now **appends**
  a chunk (a leading `+` is outside the URL-safe alphabet), while the existing
  `AT+LUAAPP=<base64>` still replaces the stored source. The stored app is
  capped at `MAX_LUA_APP_BYTES` (16 KiB); each chunk is a read-modify-write +
  NVS commit. `scripts/luaapp-encode.sh` was rewritten to emit the ordered
  chunk commands (UTF-8-safe chunking, 150 source bytes per line) instead of
  one over-limit line, and `docs/LUA_SCRIPTING_S3.md` / the AT reference
  document the protocol. Also fixed those docs' stale I2C pin defaults
  (GPIO5/4, matching `I2C_SDA_PIN`/`I2C_SCL_PIN`).
- **Two suspected causes of the S3 Lua Core0 crash removed from the boot path**
  (hardware root-cause still open):
  - The Lua host spawned its **own** 24 KiB `llm-worker` on Core 0 running a
    second DeepSeek/mbedTLS client. It now installs a
    `ChannelLlmBackend` over the main agent's single worker via
    `llm::publish_shared_sender` / `llm::shared_sender` (bounded 2 s wait for
    the agent thread to publish it). One TLS client per device reduces Core-0
    stack/heap pressure; the worker already serialises requests. If no model
    is configured, the Lua host keeps the local heuristic.
  - The default `main.lua` no longer probes hardware at boot (the previous
    script did a flash write plus ADC/I2C/GPIO reconfiguration while Wi-Fi/TLS
    was still starting). It now boots passively and defines `selftest()`;
    operators call it from an uploaded app once the link is up.
  Neither change is claimed as the proven root cause — the crash still needs a
  hardware boot log (the headroom instrumentation above is what to read) — but
  both remove strictly riskier work from the failing path.
- **The Piccolo VM's arena cap now follows the board's PSRAM.** It was a flat
  8 MiB "for the 8 MB PSRAM", but the shipped quad dev board has **2 MB total**
  (shared with Wi-Fi/TLS/the agent), so a runaway script could exhaust the pool
  and abort the process. `magent-lua` now uses 512 KiB (the same bound as the
  mlua sandbox) by default and 2 MiB under `s3-8mb-psram`; the firmware's
  profile feature forwards it via `magent-lua?/s3-8mb-psram` (weak, only when
  the Lua host is built). Tests pass under both profiles
  (`piccolo_engine_enforces_memory_limit`).
- **The firmware's exact Lua feature combo is now clippy-clean.** CI ran
  `cargo clippy -p magent-lua --all-targets --features piccolo`, which leaves
  the default `mlua` feature on; the firmware builds
  `--no-default-features --features piccolo`, where `lua-run`'s mlua-only
  imports and the `demo`/`apps`/`event_loop` examples produced unused-import
  errors. The bin imports are `cfg(feature = "mlua")`-gated and the three
  examples carry `required-features = ["mlua"]`; both clippy invocations are
  clean.
- **New `firmware/esp32-app/qemu-s3.sh` for no-hardware S3 debugging.** It
  builds a release `board-s3,uart,lua` image, merges a 4 MB flash image with
  the release bootloader + `partitions.s3.csv`, and runs Espressif's QEMU
  `esp32s3` machine with `-no-reboot` (a panic/reset loop exits immediately and
  the serial log is kept). QEMU's machine has no PSRAM model, so the script
  swaps in `CONFIG_SPIRAM=n` for the build and restores the tracked sdkconfig
  on exit (`--psram` keeps it; `--no-build`/`--timeout` supported). This was
  used to boot the S3 image to IDF `app_init` locally; the emulator lacks the
  clock/entropy/PSRAM behaviour needed to reach `main`, so it does not yet
  reproduce the Lua Core0 fault.
- **`config set` now points at a config it just made invalid.** The write
  behaviour is unchanged (type coercion, no semantic refusal), but human mode
  runs `validate_record` on the mutated record and prints
  `warning: this value leaves N issue(s) for config validate` when the next
  `magent config validate` would fail — e.g. `sampling.temperature 9`. JSON
  mode's envelope is unchanged. (BACKLOG option (c); refusing on write remains
  an owner decision.) Config tests: 93 passed.
- **`KvStore::compact` recovers a transient rewrite failure.** It erased the
  store and propagated the first `write_entry_at` error, leaving every surviving
  entry after the failure unrewritten while the caller saw only a generic
  `StorageWriteFailed`. It now retries the whole pass once (the live set is
  still in RAM), and a second failure logs how many entries were rewritten and
  that the store may be incomplete before returning the error. Regression test
  `compact_recovers_from_a_transient_write_fault` injects a one-shot write
  fault (`MockFlash::fail_next_write_at`) and asserts both keys survive;
  storage tests: 44 passed.
- **`qemu-s3.sh --gdb`.** Starts QEMU with `-s -S` (GDB server on :1234, paused
  at reset) and prints the `xtensa-esp32s3-elf-gdb` / `target remote` recipe.
  The timeout watchdog is skipped in this mode because the user drives QEMU.
- **`build-s3.sh` prints a path that exists.** Its success message pointed at
  `target/…` relative to the firmware member, but Cargo's target dir is the
  workspace root; it now derives its own directory and prints the actual
  workspace-root ELF/flash path (the sdkconfig env is also script-relative).
- **`board-s3,uart,lua` (no Wi-Fi) is now a real build configuration.**
  `sntp_sync` was gated on `wifi` while `main`/`at_dispatch` referenced it
  unconditionally, producing 10 unresolved-module errors. The module is always
  compiled now (it only needs lwIP's SNTP client), and the local-only S3 build
  compiles cleanly.
- **The Lua thread gets a 64 KiB stack (with a 32 KiB fallback).** The old
  32 KiB had to host Piccolo's parser/VM frames *and* a nested
  `agent.reason()` ReAct loop; the source comment already called the headroom
  "thinner than the agent thread's". `start_lua_task` now tries 64 KiB first
  and retries with 32 KiB if FreeRTOS/internal DRAM refuses the larger stack
  (the thread body was extracted to `run_lua_host` so the same `FnOnce` can be
  handed to both attempts).
- **The Lua host is not started in safe mode.** Safe mode (3 failed boots) is
  the recovery path, but `start_lua_task()` ran unconditionally, so a
  Lua-induced boot fault re-entered itself on every boot and left no working
  AT/OTA surface. It now logs and skips the host when `safe_mode` is set, making
  the crash loop recoverable.
- **Lua boot diagnostics for the open Core0 crash.** The Lua thread logs
  `internal_free` / `psram_free` / `stack_min_free` at thread start, before and
  after `PiccoloVm` construction, and after `main.lua` boot
  (`lua_task::log_headroom`). The crash is still root-cause-pending on
  hardware; the log now shows the last completed phase and whether internal
  DRAM or the 32 KiB stack was the constraint.
- Verified (2026-11-08): the three S3 cross-checks all pass on Xtensa —
  `check-s3.sh` (quad, exit 0), `check-s3.sh --lua` (compiles
  `magent-lua`/`piccolo`, `lua_task`, the headroom logging and the chunked
  `AT+LUAAPP` path, exit 0), and
  `check-s3.sh --octal8mb` (`s3-8mb-psram` profile + `sdkconfig.s3.defaults`
  swap, exit 0). C61 `cargo check --release` is also exit 0 (the `lua`
  compile-error arm is not taken). Host: `cargo clippy --all-targets
  -- -D warnings` and the full `cargo test` sweep are green on Rust 1.98, and
  the 38 `esp32_http` tests pass under `--features esp32,web3`. The esp-rs
  toolchain had to be installed manually (`esp-rust`) because `espup install`
  is interrupted by the local proxy while fetching the stable RISC-V targets;
  the scripts detect `+esp` and `+esp-rust` automatically.

### Fixed (2026-11-08) — a blockchain HTTP "connect" test could never fail
- **`test_esp_http_client_connect` asserted `result.is_ok() || result.is_err()`**,
  which is true for every `Result` — a test that cannot detect a regression.
  `EspHttpClient::connect()`'s comment also still said "in production, implement
  actual TCP/TLS" although the actual wire I/O moved to the injected
  `Transport` (per request). The method is now documented as what it is — a
  readiness flag with no I/O; a transport failure still surfaces from
  `rpc()`/`post_raw`, never as success. The test now pins the flag
  transitions (`false → connect → true → disconnect → false`).
  Pinned by `connect_toggles_the_readiness_flag`.

### Fixed (2026-11-08) — the config store could report writes it did not keep
- **`config_store::MemoryBackend` implemented `write`/`delete` as no-ops that
  returned `Ok(())`**, so `ConfigStore::set_agent_config` reported success and
  the value was `None` on the very next read. A second type,
  `MutableMemoryBackend`, held the working implementation — the name suggested
  the opposite of the behaviour. There was no reason for the split: the
  `StorageBackend` trait takes `&mut self`, so the "immutable" variant was
  never needed. `MemoryBackend` now stores, and `MutableMemoryBackend` is a
  deprecated alias.
- **`StoreEntry::version` was written but never checked.** An entry from a
  newer schema was deserialised against this build's field set, or failed with
  a generic type error; `get_agent_config` now returns
  `ConfigurationError { field: "agent_config.version" }` when
  `version > STORE_VERSION`, so a caller can upgrade instead of running on a
  misread config. Pinned by `store_rejects_a_newer_schema_version`.
- **`export_json` truncated a too-long document to 2048 bytes**, producing
  invalid JSON with no error. It now fails with `export.size`/`TooLong`; a cut
  mid-document is not a shorter export.
- **The envelope and its payload shared one 512-byte cap.** `StoreEntry` is
  serialised with a `u16` version and a length prefix, so a payload in the last
  few bytes of `MAX_ENTRY_SIZE` produced a 513+-byte envelope that every
  backend rejected. The stored unit is now `MAX_STORED_VALUE`
  (`MAX_ENTRY_SIZE + 8`); pinned by `store_envelope_fits_a_full_size_payload`.
- Also removed the unused `MAX_VERSION_HISTORY` (nothing tracked version
  history) and corrected two false claims in the module docs: `update_count`
  is per-process and is not a flash-wear bound, and a failed write leaves the
  store unchanged only because each backend guarantees it (the in-memory
  backend replaces a slot in one assignment).

### Fixed (2026-09-28) — a script or model could drive an S3 pin the board already owns
- **Neither the Lua `hardware.gpio_*` / `pwm_set` / `adc_read` bindings nor the
  agent's `write_gpio` tool checked the pin number, so a Lua app or an LLM tool
  call could pick GPIO26–32 — the SPI0/1 pins to the in-package flash.** On the
  ESP32-S3 `gpio_reset_pin` on those pins detaches the SPI0/1 function, and the
  next flash cache miss faults: a `StoreProhibited` reported on whichever core
  fetches first (often Core 0 running Wi-Fi/lwIP), with the real cause on the
  thread that touched the pin. The same class of mistake is why the Lua I2C bus
  was moved off GPIO8/9 (`lua_task.rs`). GPIO19/20 (USB-Serial/JTAG), GPIO10/11
  (UART0) and GPIO4/5 (I2C0) are likewise owned by the ingress links and the Lua
  I2C bus.
- Fix: a single `local_tools::reserved_pin_reason(pin)` (S3-only; `None`
  elsewhere) is consulted by both call sites — `write_gpio` returns a failed
  tool result, the Lua bindings return `Err` — so the attempt is a reported
  error instead of a crash. Out-of-range pins are still left to ESP-IDF, which
  already returns `ESP_ERR_INVALID_ARG`.
- This closes a real hole and is a **candidate** contributor to the reported
  Core-0 `StoreProhibited`; it is not proven to be that crash's root cause (which
  is still tracked in `docs/BACKLOG.md`). The C61 reserved-pin map is not encoded
  yet, and GPIO33–37 (octal-PSRAM pins) are deliberately left free because the
  shipped `board-s3` build targets the quad-PSRAM dev board — both recorded as
  follow-ups in `docs/BACKLOG.md`.

### Fixed (2026-09-28) — two S3 threads could swap each other's core and priority while spawning
- **`core_affinity` set the thread profile and spawned as two separate steps.**
  The profile lives in a process-global `esp_pthread_cfg_t` written by
  `esp_pthread_set_cfg` and read back by the *next* `pthread_create`, so
  "`apply_profile`, then spawn" is only atomic while a single thread is doing
  it. The S3 spawns from two threads — the boot path (and, later, `AT+OTA` /
  `AT+RST` on the ingress thread) and the Lua host thread spawning its
  `llm-worker` — so an interleaving could land the ReAct agent on Core 0 below
  lwIP, or a long TLS download on the Core-1 real-time core. C61 is unaffected
  (`apply_profile` is a compile-time no-op there).
- Fix: a `SPAWN_LOCK` mutex serialises the set-and-spawn pair. `spawn_thread`
  takes it around `apply_profile` + `Builder::spawn`, and a new
  `spawn_with_profile` does the same for the three `at_dispatch` sites that
  spawn on the default stack. `apply_profile` is now module-private so the
  unprotected pattern cannot come back.

### Fixed (2026-09-28) — the S3 USB ingress link could crash Core 1 on a bad byte count
- **The native USB-Serial/JTAG ingress adapter trusted a count the vendored HAL
  can return as `usize::MAX`.** `.cargo-patches/esp-idf-hal-0.46.2/src/usb_serial.rs`
  returns `Ok(len as _)` from the C `int` in both `read` and `write` without
  mapping the error case (`-1`), unlike `UartRxDriver::read` which maps `-1 | 0`
  to `Err`. A driver error therefore reached `UsbSerialAdapter::poll` as
  `Ok(usize::MAX)`, and `magent_core::ingress::IngressGateway::ingest` sliced
  `&scratch[..n]` — an out-of-range panic on the **Core-1 real-time ingress
  thread** (chip reset). On the write side `send`'s `offset += n` with that
  value ended the loop and returned `Ok(())`: a reply that was never
  transmitted was reported as sent.
- Fix (defence in depth): `ingest` now accepts `Ok(n)` only when
  `n <= scratch.len()` and otherwise logs + discards the frame — the
  `LinkAdapter` contract already promised `n <= buf.len()`, the gateway just
  never enforced it. `UsbSerialAdapter::poll`/`send` reject an over-buffer count
  as `AdapterError::Io` before it reaches the gateway. USB's legitimate `Ok(0)`
  ("nothing buffered", relied on by the non-blocking poll) is preserved.
- Tests (`ingress_tests`, `std,ingress,mqtt,web3`):
  `overreported_frame_is_discarded_not_panicked` and
  `overreported_count_alone_yields_no_frame`.

### Fixed (2026-09-28) — the device's `fetch_web` SSRF guard was bypassable by a redirect
- **The ESP32-S3's (and C61's) on-device `fetch_web` vetted only the first host,
  then let the HTTP client follow `302`s behind its back.** `local_tools::http_get_once`
  screened the initial host with `magent_core::at_validate::public_fetch_target_refusal`
  but then used `HttpConfig { ..Default::default() }`, whose `follow_redirects_policy`
  is `FollowRedirectsPolicy::FollowGetHead`. The esp-idf client follows a GET
  redirect *inside C* (`esp_http_client_set_redirection`) and the `esp-idf-svc`
  wrapper exposes **no** post-redirect URL, so the guard never saw the host that
  was actually dialled. A model-supplied public URL that `302`s to
  `http://192.168.x.x/`, `http://169.254.169.254/` or the device's own web-admin
  port was read and echoed back into the transcript — the exact hole the guard
  exists to close (`fetch_web`'s URL is model-generated, and a fetched page's
  text can steer the next turn). The **host** tools (`magent_core::web`) already
  re-validated after redirects, so the two SSRF paths had drifted apart.
- Fix: `http_get_once` now sets `FollowRedirectsPolicy::FollowNone` and returns the
  `Location` header; a new `http_get_following_redirects` walks the chain in Rust
  (3 hops), re-entering `http_get_once` per hop so the SSRF guard, the DNS/TCP
  preflight and the RT-watchdog feeding all re-run on every new host. The next URL
  comes from a new **host-testable** helper,
  `magent_core::at_validate::resolve_http_redirect`, which accepts only absolute,
  protocol-relative and root-relative locations and returns `None` otherwise (a
  relative/opaque `Location` is refused, not guessed).
- Tests (`magent-core`, run by the default sweep):
  `resolve_redirect_accepts_the_three_safe_forms`,
  `resolve_redirect_refuses_what_it_cannot_revet`, and
  `resolved_redirect_targets_are_classified`.

### Tested (2026-09-27) — the ESP32-S3 memory profile's test suite was red and never run
- **`s3-8mb-psram` (the ESP32-S3's 8 MB PSRAM profile) had a failing test that
  no sweep ever executed.** `magent-core`'s
  `agent_runner::compression_tests::compress_now_bounds_byte_footprint` sized
  its fixture at a hard-coded `6 × 200 KiB ≈ 1.2 MiB` and then asserted
  `estimate_bytes(..) > MAX_DYNAMIC_CONTEXT_BYTES` before exercising the
  byte-GC. That ceiling is **512 KiB on the C61 but 2 MiB under
  `s3-8mb-psram`**, so on the S3 profile the fixture sat *below* the budget and
  the precondition failed:
  ```
  thread '...compress_now_bounds_byte_footprint' panicked at
  magent-core/src/agent_runner.rs:5161:9:
  assertion failed: crate::conversation::estimate_bytes(&runner.messages) >
      crate::MAX_DYNAMIC_CONTEXT_BYTES
  ```
  `test-workspace.sh` only `cargo check`ed the feature (the matrix step), and
  the standing sweep runs `--features std`, so the S3 profile's byte-GC bound
  was advertised (REQ-SCHED-001 / mem-3) but *never* verified. The
  implementation (`conversation::gc_to_budget`) was correct — the defect was
  that nothing exercised it under the S3 ceiling.
- Fix: the fixture is now sized *from the budget under test*
  (`MAX_DYNAMIC_CONTEXT_BYTES / 200 KiB + 2`), so the precondition holds and the
  GC is exercised under both ceilings. Added
  `s3_profile_keeps_a_history_the_c61_would_drop`, which pins the behaviour the
  raised ceiling actually buys: a ~1.2 MiB history (over the C61 budget, under
  the S3 one) is *kept* verbatim rather than GC'd.
- `test-workspace.sh` gained step **5b**, which runs
  `cargo test -p magent-core --features std,s3-8mb-psram --lib` — the S3
  profile's tests can no longer go unrun.

### Fixed (2026-09-27) — the S3's temperature read reported a value it never obtained
- **`read_sensor sensor=temperature` (firmware) and the Lua
  `hardware.sensor_read("temp")` path discarded
  `temperature_sensor_get_celsius`'s return code.** Both sites used the pattern
  ```rust
  if e == 0 { sys::temperature_sensor_get_celsius(handle, &mut celsius); }
  ...
  let ok = last == 0;
  ```
  so `e` (reported as `last`) never saw the *read's* failure: a failed read
  answered `success: true` with `celsius` still at its `0.0` initialiser — a
  plausible-looking `temperature=0.0 C (err=0)` the sensor never produced. The
  Lua path returned `Ok(0.0)` for the same reason.
- Fix: capture the `esp_err_t` from `get_celsius` (both
  `firmware/esp32-app/src/local_tools.rs` and `.../lua_task.rs`). On failure the
  agent tool answers `temperature read failed (err=…)` with `success: false`,
  and the Lua backend returns `Err` — no fabricated value.
- Gate: `cargo check` for the C61 build and
  `cargo +esp check --target xtensa-esp32s3-espidf --features board-s3,wifi,uart`
  (also with `,lua`).

### Fixed (2026-09-27) — `apply_profile` could panic despite documenting "never panics" (S3)
- `firmware/esp32-app/src/core_affinity.rs`'s `apply_profile` documentedly
  "never panics on its own; a failure to set the profile is downgraded to a
  `log::warn!`". But `ThreadSpawnConfiguration::set` (esp-idf-hal) **panics**
  when the priority is outside FreeRTOS's `[1, configMAX_PRIORITIES - 1]`, and
  the `unwrap_or_else(|e| log::warn!(..))` at the call site cannot catch a
  panic. The shipped constants (8/10/15/20 vs `configMAX_PRIORITIES = 25`) are
  in range today, so this was latent — a future constant at or above the ceiling
  would abort the spawn instead of failing open.
- Fix: clamp the priority into `[1, configMAX_PRIORITIES - 1]` (with a warning
  when clamping) before `set()`, so the documented fail-open actually holds.
- Gate: the S3 `cargo check` above — this file is `#[cfg(feature = "board-s3")]`.

### Fixed (2026-09-27) — the smartwatch step counter could never count more than one step
- **`host/nrf52-simulator`'s `StepCounter::tick` latched off after the first
  step.** The type advertises step tracking (the demo prints `Steps: …`, the
  `read_sensor sensor=steps` tool reads it, and the "Track my steps" scenario
  exists to exercise it), but:
  ```rust
  pub fn tick(&mut self, z: f32) {
      if z > 10.5 && self.steps.is_multiple_of(2) { self.steps += 1; }
  }
  ```
  incrementing takes `steps` to 1, after which `is_multiple_of(2)` is
  permanently false — no input sequence could ever reach 2. The counter a
  caller reads is a constant, not a count.
- Fix: `StepCounter` now tracks whether the previous reading was above the
  threshold and counts a step on the *rising* edge only. A footfall counts
  once (a sustained above-threshold reading is not re-counted every tick), and
  successive footfalls keep counting. `add_steps` now saturates instead of
  overflowing.
- **Companion: the accelerometer never produced the spike the counter
  thresholds on.** `Accelerometer::tick` wrote only resting noise
  (`9.8 ± 0.2`), so `SmartwatchSimulator::tick` — the documented way to advance
  the sensors — fed the counter a value that never exceeded 10.5. The
  accelerometer now models a walking wearer: once per stride it spikes the
  vertical axis above the threshold and settles back to ~1 g between steps.
- Regression tests, verified to fail against the old code
  (`host/nrf52-simulator/src/lib.rs`):
  - `step_counter_counts_each_footfall` — three footfalls must be three steps.
    Old code: `assertion left == right failed: left: 1, right: 3`.
  - `step_counter_does_not_double_count_a_sustained_reading` — a sustained
    spike is one step, a later footfall still counts. Old code:
    `left: 1, right: 2`.
  - `simulator_tick_advances_the_step_counter` — ticking the simulator 3 s must
    accumulate steps. Old code: `expected steps to accumulate over 3 s, got 0`.

### Fixed (2026-09-27) — the nRF52840 RTC rolled `seconds` over one tick late
- **`magent-hal`'s simulated `RtcTime::tick` compared `fetch_add`'s
  *pre*-increment value against the rollover boundary.**
  `magent-hal/src/nrf52/sim.rs`:
  ```rust
  let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);   // value *before* the add
  if ticks >= 32768 { … self.seconds.fetch_add(1, …); }    // so this fires on tick 32 769
  ```
  The doc comment promises "every 32 768 ticks roll `seconds` over by one";
  in practice every second of uptime was reported one tick late.
- Fix: compare the previous value against `32767` (the value the 32 768th tick
  sees), so `seconds` advances exactly on the 32 768th tick.
- Regression test, verified to fail against the old code
  (`magent-hal/src/nrf52/sim.rs`):
  `rtc_rolls_seconds_over_on_the_32768th_tick` — after 32 767 ticks `seconds`
  is still 0, on the 32 768th it is 1 and the sub-second counter resets. Old
  code: `assertion left == right failed: the 32 768th tick completes a second,
  left: 0, right: 1`.

### Fixed (2026-09-27) — `TransactionRequest::transaction_hash` returned an all-zero hash
- **A `pub` method documented as "Calculate the transaction hash" ignored both
  its arguments and returned `Hash::zero()` for every input.**
  `magent-core/src/web3/blockchain/transaction.rs` carried:
  ```rust
  pub fn transaction_hash(&self, _signature: &[u8; 65]) -> Hash {
      // In a real implementation, this would compute the keccak256 hash …
      Hash::zero()
  }
  ```
  A caller that displayed the result — the documented "display purposes" use —
  would print an all-zero hash that resolves to no transaction, i.e. a
  fake-success path: it looks like a computed value and is a constant. Round 38
  built every encoder the real computation needs (`encode_legacy_signed`,
  `encode_typed_signed`, `rlp_encode_u256_be`), so the method can be honest now.
  - Fix: `transaction_hash` reconstructs the signed encoding from the 65-byte
    `r ‖ s ‖ v` signature and returns `keccak256` of it — the same bytes
    `TransactionBuilder::sign` hashes. `v` is the canonical `recid + 27` in the
    signature, so for a legacy request the on-chain value is recomputed as
    `chain_id * 2 + 35 + y_parity` (overflow saturates rather than panicking),
    and a typed request gets the raw `y_parity` — mirroring `sign` exactly so
    the two cannot disagree.
  - `#[cfg(feature = "web3")]`, matching `sign` and the `sha3` import (the
    `blockchain` feature implies `web3`, so no build loses the method).
- Regression test, verified to fail against the old code
  (`transaction_hash_matches_the_signed_encoding`, `transaction.rs`): signs a
  chain-137 legacy transaction and an EIP-1559 one, checks the method's result
  is not `Hash::zero()` and equals `SignedTransaction.hash` for both. Old code:
  `assertion left != right failed: a real signature must not hash to zero`
  (`left`/`right` both `Hash([0; 32])`).
- Note: nothing in the workspace calls `transaction_hash` yet, so no
  user-visible path changes; the defect was latent but reachable by any first
  caller. The CLI's `web3 tx build` still prints an unsigned transaction.

### Fixed (2026-09-27) — signed transactions were not encodable for submission (EIP-155 `v` truncated, typed `v` wrong)
- **`TransactionBuilder::sign` produced a `raw_transaction` that is not a
  transaction, and an EIP-155 `v` that is wrong on most chains.**
  `magent-core/src/web3/blockchain/transaction.rs` built the wire bytes as
  `raw = encode_for_signing(); raw.extend_from_slice(sig.as_bytes())`. That is
  only remotely right for an EIP-2718 (typed) transaction, and even then it
  appends the wrong `v`:
  - **Legacy**: a legacy transaction is a *plain RLP list*
    `[nonce, gasPrice, gas, to, value, data, v, r, s]` — gluing 65 signature
    bytes onto the nine-item signing payload is not a valid encoding at all.
    The signing payload was also one item too long: it emitted ten items
    (`… chainId, 0, 0, 0`), whereas EIP-155 specifies nine
    (`… chainId, 0, 0`), so the hash being signed was not the EIP-155 hash.
  - **EIP-155 `v` truncation** (`secp256k1.rs:529`):
    `sig.bytes[64] = (chain_id * 2 + 35 + y_parity) as u8`. `EthereumSignature`
    stores `v` in a single byte, so for `chain_id > 110` the value wraps —
    Polygon's 137 gives 309, and `309 as u8 == 53`. The old test computed its
    expectation with the same `as u8`, so it passed while the encoder was
    wrong.
  - **Typed**: EIP-2930/1559 signatures carry the raw `y_parity` (0 or 1), but
    the appended bytes came from `EthereumSignature`, whose `v` byte is
    `recid + 27` (27/28) — a form no node accepts. `SignedTransaction.v` said
    `0`/`1` while the bytes on the wire said `27`/`28`.
  - `SignedTransaction.hash` was the hash of the *signing payload*, not of the
    signed transaction, despite being documented as "the raw transaction hash".
- Fixes:
  - `TransactionSigner::sign_legacy_eip155` now returns
    `(EthereumSignature, u64)` — the signature keeps the canonical `recid + 27`
    byte (so `verify`/`recover_from` still work), and the full-width EIP-155
    `v` comes back as a `u64` for the caller to encode. This is the only way to
    be correct: the value does not fit in the signature's byte.
    **Breaking change to a `pub` API** (no in-repo callers outside this module).
    Overflow of `chain_id * 2 + 35` is now a `BlockchainError`, not a debug-mode
    panic.
  - The legacy signing form is nine items, and a new
    `encode_legacy_signed(v, r, s)` emits the signed RLP list.
    `encode_typed_signed(y_parity, r, s)` emits the EIP-2718 envelope with the
    raw `y_parity`. Both share one field list per type
    (`typed_unsigned_items`), so the unsigned and signed forms cannot drift.
  - `r` and `s` are RLP-encoded as integers with leading zeros stripped
    (`rlp_encode_u256_be`); the fixed-width 32-byte form is not a valid RLP
    integer and changes the transaction hash.
  - `SignedTransaction.hash` is now `keccak256(raw_transaction)`.
- Regression tests, both verified to fail against the old code:
  - `sign_legacy_keeps_v_full_width_above_chain_id_110`
    (`transaction.rs`) — old code produced `v = 53` for chain 137
    (`expected EIP-155 v for chain 137, got 53`). It decodes the raw
    transaction's RLP list and asserts the 7th item is the full-width `v`,
    not its low byte.
  - `test_transaction_builder_sign_eip1559_y_parity_is_zero_or_one` (tightened
    and extended) — old code's raw had 9 signature-less items where the signed
    encoding needs 12 (`left: 9, right: 12`).
  - `test_transaction_builder_sign_legacy_eip155` now rebuilds a recoverable
    signature from the encoded `r`/`s`/`v` and recovers the signer, i.e. it
    checks the raw bytes actually verify against the signing payload.
  - A small RLP list reader (`rlp_list_items`) is included in the test module —
    the crate ships encoders only — so the assertions are on the decoded
    structure rather than on a re-derivation using the same code under test.
- Note: nothing in the workspace calls `TransactionBuilder::sign` yet (the CLI's
  `web3 tx build` prints an unsigned transaction), so no user-visible path
  changes. The companion `TransactionRequest::transaction_hash` stub (which also
  returned `Hash::zero()`) was implemented in the round-39 entry above.

### Fixed (2026-09-27) — `Erc721ApprovalForAll::from_event` panicked on short event data
- **An event parser indexed the data blob at a fixed offset behind a check that
  did not bound it.** `magent-core/src/web3/blockchain/events.rs:388` read the
  `approved` flag as
  ```rust
  let approved = if !event.data.is_empty() { event.data[31] != 0 } else { false };
  ```
  `!event.data.is_empty()` only rules out length 0, so any 1–31 byte `data`
  indexed out of bounds: `index out of bounds: the len is 1 but the index is 31`
  at `events.rs:402` (pre-fix line). The function is `pub`, returns
  `Result<Self, Web3ErrorKind>`, and is re-exported from `web3::blockchain` — a
  caller that matches on `Err` gets a panic instead. The crate already denies
  `clippy::panic_in_result_fn`, but raw indexing is a different lint
  (`indexing_slicing`), so nothing flagged it.
  - `Erc721ApprovalForAll` was also the only one of the four `from_event`
    parsers that touched raw bytes: `Erc20Transfer`, `Erc20Approval` and
    `Erc721Transfer` all go through the guarded `Event::topic_as_*` /
    `data_as_*` helpers, each of which returns `None` for a short input. The
    guard the others rely on simply wasn't applied here.
  - Fix: a new `Event::data_as_bool()` alongside the existing `data_as_address` /
    `data_as_u256` / `data_as_u64` — same 32-byte guard, `Some(data[31] != 0)` —
    and `from_event` now maps `None` to a `BlockchainError` instead of indexing.
    Empty data, which the old code silently coerced to `false`, is now reported
    as malformed like every other unparseable payload: guessing "not approved"
    from a truncated log is the same failure mode as the always-`Ok` validator
    fixed in round 36.
  - Blast radius: a node or contract returning a short/malformed log is
    attacker-influenced data from outside the process, so this was reachable with
    no bad input from the agent itself. Indexing was the only unguarded read of
    this class left in the module — the other sites (`topic_as_address`,
    `data_as_address`, `parse_u256_be`/`parse_u64_be`, `Address::from_hex`,
    `EventFilter::with_topic`) all bound-check, verified this round.
- Regression test `erc721_approval_for_all_rejects_short_data_instead_of_panicking`
  (`magent-core/src/web3/blockchain/events.rs`), verified to fail against the old
  code (panicked at `events.rs:402`). It covers every length 1..32, the
  previously-silent empty case, and both `true`/`false` decodes of a full word.

### Fixed (2026-09-27) — `hardware.i2c_write` accepted a payload that silently aliased its own registers
- **The simulated I2C register file is addressed by an 8-bit offset, and the
  write path never bounded the payload's length.** Both `SimHardware::i2c_write`
  and `SimHardware::i2c_read` compute each register as
  `reg.wrapping_add(off as u8)`, so an offset past 255 folds back onto earlier
  registers. A script writing more than 256 bytes therefore overwrote the bytes
  it had just written and was told the write succeeded — the caller's data was
  silently corrupted with no error to detect it.
  - The `mlua` engine (host default) already rejected oversized payloads in its
    own binding (`vm.rs`, `MAX_I2C_LEN`), but the **`piccolo` engine has no such
    check** — and piccolo is the engine the ESP32-S3 runs on-device, so the
    unprotected path was the one that matters. The two engines disagreeing about
    the same script is itself the bug.
  - The bound now lives on the shared backend
    (`host/lua-app/src/hardware.rs`, `MAX_I2C_WRITE_LEN`), so every engine and
    any future Rust caller is covered, and the `HardwareBackend` contract is
    honoured: a script fault is an `Err`, never silent corruption.
- Regression test `piccolo_vm_rejects_an_oversized_i2c_write`
  (`host/lua-app/tests/piccolo_tests.rs`), verified to fail against the old code
  (the 300-byte write returned `Ok`). It also pins that a payload exactly at the
  bound (256 bytes) still round-trips, so the check cannot degrade into
  "reject all large writes".

### Fixed (2026-09-27) — `CredentialSchema::validate` could never report a missing field
- **The credential-schema validator tested `required` for the wrong JSON shape,
  so it returned `Ok(())` for every credential.** JSON Schema defines `required`
  as an *array of property names*; the code matched
  `if let Some(Value::Object(map)) = self.schema.get("required")`, a branch no
  spec-compliant schema can ever take. The `for` loop inside was unreachable:
  a schema declaring `"required": ["name", "degree"]` validated a credential
  with neither claim as conformant.
  - This is the "checker that can never fail" class again: the function is a
    `pub` API whose doc comment promises "Validate a credential against this
    schema", and it is compiled into the CLI's blockchain build
    (`blockchain` → `verifiable_credentials`). An always-`Ok` validator is worse
    than none, because a caller trusts it.
  - `validate` now reads `required` as an array (`as_array`), resolving a name of
    `id` to the subject's DID (a dedicated field, not a claim) and every other
    name to the subject's claims. A `required` value that is present but not a
    well-formed array of strings is reported as an error rather than skipped —
    silently ignoring a schema this function cannot interpret is how the
    always-`Ok` behaviour arose in the first place. Every `"required"` fixture in
    the workspace is already an array, so no in-repo schema depends on the dead
    object form.
- Regression test `schema_validate_enforces_required_fields`
  (`magent-core/src/web3/verifiable_credentials.rs`), verified to fail against
  the old code. It pins the missing-claim rejection (by field name), the
  conformant case, the `id`→DID resolution, and the malformed-`required` error.
- Note: nothing in the workspace calls `CredentialSchema::validate` yet (the
  CLI's `web3 vc verify` uses `VerifiableCredential::validate`, which is a
  different function and was already correct). The defect is fixed at the
  library surface so the advertised capability works when it is wired up.

### Fixed (2026-09-27) — `magent web3 tx --data` panicked on a non-ASCII hex argument
- **The CLI's blockchain hex decoder sliced a `&str` by byte offsets.** Input is
  only checked for even length, then decoded with
  `u8::from_str_radix(&hex[i..i + 2], 16)`. A `&str` slice must land on a UTF-8
  character boundary, so an argument whose 2-byte windows straddle a multi-byte
  character panicked: `magent web3 tx build 0x11… --data '11€x'` hit
  `end byte index 4 is not a char boundary; it is inside '€'` at
  `cli/src/web3_blockchain.rs:820`. The module's `Result` signature and the
  CLI's own "this is a bug in magent, not in your command line" panic handler
  both treat that as an internal error — i.e. a user typo was reported as a
  program defect, and the process took the panic path.
  - `decode_hex` now rejects a non-ASCII argument up front with a normal usage
    error (`hex string "11€x" contains non-ASCII characters`, exit code 2). Hex
    is ASCII by definition, so no legitimate input is affected, and the message
    no longer claims "invalid hex digit" for a string that cannot be hex.
  - The other `decode_hex` in the CLI (`cli/src/web3.rs`, used for `--seed` /
    `--pk`) already worked on `as_bytes()` and was never exposed; this was the
    only `from_str_radix(&str_slice, …)` in the workspace.
- Regression test `decode_hex_rejects_non_ascii_without_panicking`
  (`cli/src/web3_blockchain.rs`), verified to fail against the old code
  (panicked at `cli/src/web3_blockchain.rs:820`). It pins the three alignment
  cases (multi-byte char mid-window, at the window start, alone) and the valid
  path is still covered by the existing hex tests plus an end-to-end
  `magent web3 tx build … --data 0xdeadbeef`.

### Fixed (2026-09-26) — a Lua pattern with an unterminated `(` panicked the VM
- **The piccolo `string.match` shim sliced a reversed byte range.** The shim is
  hand-written (piccolo's `Lua::full()` ships no `string.match`), and its
  documented contract is that *"Unsupported patterns return `None`."* While
  parsing a `(...)` capture it scanned forward for the closing paren and then
  built the group body as `&pat[group_start..i - 1]`. With the paren left
  unterminated at the end of the pattern, the scan never advanced past
  `group_start`, so `i - 1` landed *before* `group_start` and the slice panicked
  (`slice index starts at 1 but ends at 0`). Real Lua raises "unfinished
  capture" for the same input; neither a panic nor a VM abort is what a
  sandboxed interpreter should do with a user-supplied pattern.
  - This is not an exotic input: `string.match` is *the* way our enterprise Lua
    apps decode an agent reply (`string.match(action, "SET_COOLING_PULSE:(%d+)")`),
    so a typo'd or data-derived pattern takes down the whole app — and on the
    ESP32-S3 (`piccolo` is the on-device engine) that is a reboot loop, not a
    caught error.
  - The parser now returns `None` as soon as the group is left open
    (`if depth != 0 { return None; }`), before any slice is built, and the body
    slice is plain `i - 1` (safe once the group is known to be closed).
- Regression test `piccolo_string_match_tolerates_an_unterminated_capture`
  (`host/lua-app/tests/piccolo_tests.rs`), verified to fail against the old
  code (panicked at `host/lua-app/src/piccolo_vm.rs:851`). It also pins that a
  *closed* capture still works (`string.match('abc', '(b)') == 'b'`) and that an
  unbalanced nested group is refused, so the fix cannot degrade into
  "reject every capture".

### Fixed (2026-09-26) — a Lua `hardware.*` read length could abort the host
- **The Lua bindings passed a script-supplied read length straight to a
  `Vec` allocation, with no range check.** `hardware.flash_read(addr, len)` and
  `hardware.i2c_read(addr, reg, len)` are the one place an untrusted value
  crosses into a length-typed parameter, and the backend allocates a buffer of
  exactly that size (`vec![0u8; len]` in `SimHardware::flash_read`, the same in
  the S3 firmware's `i2c_read_device`, `Vec::with_capacity` in
  `SimHardware::i2c_read`). An out-of-range length therefore did not return the
  `Err` the trait advertises — it aborted the whole process, which on the
  device is a bricked app and on the host a dead test/CI run. The crate's own
  docs promise the opposite ("it never panics"; `#![deny(clippy::panic_in_result_fn)]`).
  - **piccolo engine** (`host/lua-app/src/piccolo_vm.rs`, shared with the
    ESP32-S3 firmware): the binding read the arg as `i64` and cast with
    `as usize`, so a negative Lua integer arrived as `usize::MAX` and
    `vec![0u8; usize::MAX]` hit a capacity overflow. The two read callbacks now
    go through a `pop_read_len` helper that rejects a negative or oversized
    length before the backend is called.
  - **mlua engine** (`host/lua-app/src/vm.rs`, the host default): `mlua`'s
    `usize` conversion already rejects negatives, but accepts any positive
    value, so `hardware.flash_read(0x100, 9223372036854775807)` reached the
    allocator and aborted with "memory allocation of 9223372036854775807 bytes
    failed". Both callbacks now share a `check_read_len` guard, so the two
    engines enforce the same bound (`hardware::MAX_FLASH_READ_LEN` = one flash
    sector, `MAX_I2C_READ_LEN` = the existing I2C write bound).
  - The S3 firmware inherits the fix through `magent_lua::piccolo_vm`; its
    NVS `flash_read` was already bounded (`len.min(required)`), so the
    unbounded `i2c_read_device` buffer was the remaining exposure.
- Regression tests, both verified to fail against the old code:
  `piccolo_vm_rejects_an_out_of_range_read_length`
  (`host/lua-app/tests/piccolo_tests.rs` — panicked with `capacity overflow` at
  `alloc/src/raw_vec/mod.rs:28`) and `out_of_range_read_length_is_an_error`
  (`host/lua-app/tests/lua_vm_tests.rs` — aborted the test process, `SIGABRT`).
  Both also assert that in-range reads (4096 bytes of flash, 256 bytes of I2C)
  still succeed, so the bound cannot silently reject legitimate use.

### Fixed (2026-09-26) — the nRF52 simulators' flash bounds check could be wrapped
- **Two more copies of `SimulatedFlash` panicked instead of returning the error
  their signature promises.** Both guarded a caller-supplied address with
  `addr + len > self.data.len()`. The sum wraps for an address near
  `usize::MAX`: the check compares a small wrapped number, passes, and the
  slice that follows panics — in debug as `attempt to add with overflow`, in
  release as a slice-index panic. This is the same bug the round-30 fix removed
  from `host/simulator`; these two crates are near-copies of that code and never
  inherited it.
  - `host/nrf52-simulator`: `flash_read`/`flash_write` parse `address` straight
    out of the tool argument (`parse::<usize>()`) and hand it to
    `SimulatedFlash::read`/`write`, so `{"address": "18446744073709551615"}`
    reached the panic. Both methods now use `checked_add` + a bound filter.
  - `magent-hal/src/nrf52/sim.rs`: the chip-faithful simulator's
    `read`/`write` had the same check, and `erase` had it twice more
    (`sector * sector_size`, then `start + sector_size`); all three are checked
    or saturating now, so an out-of-range sector is refused instead of wrapping
    into a valid-looking index.
  - Every library/host copy of the type now agrees: `magent-core/src/simulator.rs`,
    `host/simulator/src/main.rs`, `magent-hal/src/esp32.rs`, and the two fixed
    here. The remaining `address + len` checks live only in `magent-core/tests/`
    fixtures, which no shipped code path feeds an address into.
- Regression tests, both verified to fail against the old code:
  `nrf52-simulator`'s `flash_tool_rejects_an_overlarge_address_without_panicking`
  (panicked at `host/nrf52-simulator/src/lib.rs:362`, `attempt to add with
  overflow`) and `magent-hal`'s
  `flash_rejects_an_overlarge_address_without_panicking` (panicked at
  `magent-hal/src/nrf52/sim.rs:674`). Both also assert the last readable block
  still reads, so the fix cannot degrade into "reject everything".

### Fixed (2026-09-26) — a restricted cron month skipped to the wrong day
- **`CronSpec::next_after` carried the day-of-month across a month jump.** When
  the month field is not `*`, the matcher calls `advance_to_next_month` and
  retries. That helper only rolls the month over; it left `day`, `hour`,
  `minute` and `second` untouched. So the search resumed at *the same
  day-of-month and time-of-day* in whatever month it landed on:
  - `0 0 * 4 *` ("midnight every day in April") started from
    `2026-01-15T06:00:00Z` returned **April 16th 00:00**, not April 1st — the
    first half of April was silently skipped, and the `dow` it carried no
    longer matched the date it reported.
  - A carried day that does not exist in the target month was worse:
    `0 0 * 2 *` from `2026-01-31T12:00:00Z` produced `Feb 31`, which the day
    rollover rejected, so the search marched through the rest of the year and
    returned **February 1st of the following year** (observed `1801440000` =
    `2027-02-01T00:00:00Z`) — a daily task that came back one year late.
  - The month-skip branch now resets to the first moment of the target month
    (`day = 1`, time `00:00:00`) and re-derives `dow` from the new date, which
    is what "jump to the next listed month" has to mean.
- Regression test `scheduler::tests::cron_month_skip_lands_on_the_first_of_the_target_month`,
  verified to fail against the old code (`left: 1776297600` — April 16th —
  where the correct answer is `1775001600`, April 1st). It also pins the
  leap-adjacent `Feb 1`-of-the-same-year case so the day-carry cannot come
  back through the overflow path.

### Fixed (2026-09-26) — a model-chosen flash address panicked the host simulator
- **`FlashStorage::read`/`write` overflowed their own bounds check.** Both
  guarded with `address + len > self.data.len()`. The address is whatever the
  model put in the tool argument, read back with `as_u64()` — so `u64::MAX` is
  a value the executor accepts. The addition then wrapped: the check compared a
  small number, passed, and the slice that followed panicked — `attempt to add
  with overflow` in debug, a range panic in release. `flash_read` and
  `flash_write` are both reachable, and a `Result` that panics is not a
  failure the agent loop can report.
  - `erase_sector` had the same shape twice (`sector * sector_size`, then
    `start + sector_size`); both are checked now, so a sector that does not lie
    wholly inside the region is refused instead of wrapping into an index.
  - Fixed with `checked_add` / `checked_mul` + a bound filter, matching the
    hardening `magent-core`'s flash already carries (`simulator.rs`'s
    `erase_sector` documents this exact class of bug). `host/simulator` is a
    standalone crate with its own copy of the simulator stack, so it did not
    inherit that fix.
- Regression tests, both verified to fail against the old code:
  `tests::an_address_that_overflows_the_bounds_check_is_an_error_not_a_panic`
  panicked at `host/simulator/src/main.rs:227` with `attempt to add with
  overflow`; `addresses_at_and_past_the_end_are_handled_without_a_panic` covers
  the non-overflowing far edge (one past the end is refused, the last readable
  block still reads) so the fix cannot degrade into "reject everything".

### Fixed (2026-09-26) — Ctrl-C left the terminal in raw mode
- **The REPL's raw-mode guard could not run on the one signal it had to.**
  `lineedit`'s `RawMode` restores the terminal's `termios` from `Drop`, and the
  module deliberately leaves `ISIG` set so Ctrl-C still raises `SIGINT` (module
  doc: "nothing here has to invent an interrupt path"). But `SIGINT`'s default
  disposition **terminates the process without unwinding**, so `Drop` never ran:
  the REPL died and the shell it returned to had `ICANON` and `ECHO` cleared —
  no line editing, every subsequent keystroke invisible until the user ran
  `reset`. Only `stdin` being a TTY reaches this path, which is why the piped
  REPL tests never caught it and why this went unnoticed for so long.
- **The guard now restores the terminal before the signal kills the process.**
  `RawMode::enable` publishes the saved `termios` (boxed, so its address is
  stable across the move out of `enable`) in a `static AtomicPtr` and installs a
  `SIGINT` handler. The handler — async-signal-safe only: an atomic load,
  `tcsetattr` (on POSIX's safe list), `sigaction`, `raise` — restores the saved
  settings, resets `SIGINT` to `SIG_DFL`, and re-raises, so the process still
  dies from the signal with its usual status. `Drop` now also hands `SIGINT`
  back to whatever disposition it had before, so a caller that installed its own
  handler keeps it.
  - This covers a `SIGINT` from any source, not just the terminal's `^C` byte:
    `kill -INT` from another process restores the terminal too. Clearing `ISIG`
    and handling `0x03` in the key loop, the obvious alternative, would not.
  - A failure to install the handler is not fatal — `Drop` still covers the
    normal and panic paths, which is worth more than refusing line editing.
  - `libc` was already a unix-only dependency of the `magent` crate
    (`main.rs` restores `SIGPIPE`), so this adds no dependency.
- Regression test `cli/tests/sigint_restore.rs::sigint_restores_the_terminal_before_the_repl_dies`,
  verified to fail against the old code: it opens a pseudo-terminal (raw mode
  only engages on a TTY), runs `magent run --repl --mock` on it, waits for the
  prompt (which proves raw mode is engaged), sends `SIGINT`, and reads the
  *terminal device's* `termios` back. Against the old code it failed with
  `ICANON must be restored, or the shell is left with no line editing — left: 0
  right: 0`; the child still died from `SIGINT` in both versions, so the test
  isolates the restore.

### Fixed (2026-09-26) — the SSRF guard was bypassed by an empty port
- **`parse_http_authority` handed the classifier a host it cannot recognise.**
  When the port substring did not parse it returned the authority *verbatim*.
  For `http://127.0.0.1:/admin` — an empty port is legal and means "the scheme
  default" — that produced `("127.0.0.1:", 80)`. The guard then ran
  `"127.0.0.1:".parse::<Ipv4Addr>()` (fails), `parse::<Ipv6Addr>()` (fails), and
  the name checks `== "localhost"` / `.ends_with(".local")` (fails), so
  `public_fetch_target_refusal` returned `None` and the URL was **allowed**.
- **The request then really went out.** `validate_fetch_url` only inspects the
  host; the fetch dials the *raw* URL with `reqwest`, and the URL client parses
  `http://127.0.0.1:/admin` as loopback on the default port. So the
  model-generated `fetch_url` tool could read the device's own HTTP server or
  the cloud-metadata endpoint (`169.254.169.254`) through
  `http://169.254.169.254:/latest/meta-data/` — the exact scenario the guard
  exists to prevent (a fetched page's text re-enters the conversation as
  attacker-controlled instructions).
  - Affected forms, all dialable: `http://127.0.0.1:/`, `http://127.8.8.8:/`,
    `http://169.254.169.254:/`, `http://localhost:/`, `http://printer.local:/`,
    `http://10.0.0.1:/`, `http://[::1]:/`, `http://[fe80::1]:/`.
  - Not affected: a *non-numeric* port (`http://127.0.0.1:abc/`) — the URL
    client rejects it as `invalid port number`, so it is never dialled. Verified
    with the `url` crate, which is why the fix targets the empty case.
  - Fixed by returning `(host, default_port)` when the port is empty, so the
    classifier sees the bare host — the same reasoning the function already
    documents for the unbracketed-IPv6 case. A trailing slash-question-hash is
    irrelevant; `validate_fetch_url` also runs on the post-redirect `final_url`,
    so a redirect into this form is now caught too.
  - On the firmware, `local_tools` consumes the same function and re-glues
    `format!("{host}:{port}")` for `to_socket_addrs`; the dangling colon made
    that fail to resolve, but the guard now refuses first — a correct rejection
    instead of an opaque resolution error.
- Regression tests, verified to fail against the old parser:
  `at_validate::tests::parse_http_authority_drops_an_empty_port` failed
  `left: Some(("127.0.0.1:", 80))` / `right: Some(("127.0.0.1", 80))`;
  `empty_port_targets_survive_the_full_guard` and
  `web::tests::validate_fetch_url_rejects_an_empty_port_authority` failed their
  assertions (`validate_fetch_url("http://127.0.0.1:/admin")` was `Ok`).

### Fixed (2026-09-26) — `magent summary list` panicked on any multi-byte topic
- **`summary::truncate` sliced a `str` by bytes.** It read `s.len()` (bytes) and
  then did `&s[..max]`, which panics when `max` lands inside a multi-byte code
  point. Both call sites feed it operator- or model-supplied text:
  `list` truncates the topic column to 24 (`truncate(&r.topic, 24)`) and
  `summary show` truncates `llm_summary` to 80. `validate_topic` bounds a topic
  by *bytes* (128) and only forbids `/`, `\`, `..` and NUL, so a 26-byte CJK
  topic is perfectly legal and reaches the truncation.
  - Rewritten to count characters, matching the `truncate` helpers already in
    `config.rs` and `prompt.rs` (the odd one out was this one — and it was the
    one that panicked). Genuine truncation appends `…` within the `max` budget.
  - A sweep of the rest of the crate for the same byte-slicing pattern came back
    clean: `runner.rs` already has a boundary-safe `truncate_utf8` (added for
    exactly this class of bug on trace output), `tools.rs::clamp_utf8` and the
    `at.rs`/`web.rs` slices are boundary-safe or operate on ASCII/`&[u8]`.
  - Regression test `summary::tests::truncate_counts_characters_not_bytes`,
    proven to fail against the old code:
    `panicked at cli/src/summary.rs:657:26: end byte index 24 is not a char
    boundary; it is inside '日' (bytes 23..26 of string)`.

### Fixed (2026-09-26) — `web3 vc issue` wrote the credential nowhere in the default mode
- **The no-`--output` path of three commands output nothing.** `bind create`,
  `vc issue` and `vc present` build their artifact and write it through `emit`,
  whose doc promises "print it to stdout when no file is given". The no-file
  branch called `Output::write_json_str` — which is **a no-op in human mode** by
  design (so step output isn't interleaved with the envelope). So in the default
  `OutputKind::Human`, `magent web3 vc issue … > cred.json` wrote a **zero-byte**
  file; the credential was computed, signed, validated, and then discarded.
  - The same branch also broke `--json`: `emit` wrote the artifact object and
    *then* the caller's `render_vc`/`render_binding`/`render_presentation` wrote
    the display object, so stdout carried **two** JSON documents, contradicting
    `output.rs`'s promise that `--json` puts exactly one object on stdout
    (`web3 sign` and `scheduler run-once` were already fixed for this pattern).
  - `emit` now writes the no-file artifact with
    [`Output::write_stdout_line`] — which reaches stdout in **both** modes, the
    same fix `web3 sign` uses — and returns whether it did, so the callers skip
    their console rendering when the artifact itself is the output.
  - Verified end to end, not just in a unit test. Against the old branch, the
    real binary produced **0 bytes** of stdout in human mode and 1001 bytes
    (two objects) in `--json` mode, where `json.loads` failed with
    `Extra data: line 2 column 1 (char 739)`. With the fix, both modes emit a
    single 739-byte object that parses; `vc present` likewise emits a single
    1755-byte presentation.
  - Regression test
    `web3_blockchain::tests::emit_echoes_to_stdout_when_no_output_file_is_given`
    pins the contract (echo → `true`; `--output` → file written, `false`).

### Removed (2026-09-26) — a second "EVM RPC client" that could not make a single call
- **`web3::blockchain::client` exposed a `pub struct EvmRpcClient` that
  documented itself as an "HTTP-based EVM RPC client" and did no such thing.**
  Every one of its fourteen `ChainClient` methods routed through a private
  `raw_call` placeholder that unconditionally returned
  `Err(BlockchainError("RPC not configured: …"))`; `call()` returned
  `Err("… requires reqwest: …")`. It was never wired to anything: `grep`
  across the workspace found **zero** references outside its own file, and
  nothing re-exported it (`blockchain/mod.rs` re-exports only `BlockchainResult`,
  `ChainClient`, `ChainId`). The one live client is `HttpRpcClient`
  (`http_client.rs`), which `BlockchainBackend::Std` and
  `cli/src/web3_blockchain.rs` actually use. The type was `#[allow(dead_code)]`-
  hidden, so the compiler never asked why a public struct was unused — and its
  test suite contained the tell:
  `assert!(client.is_ok() || client.is_err());`, a test that cannot fail.
  - Removed: the `EvmRpcClient` struct and both `impl` blocks
    (`impl EvmRpcClient` and `impl ChainClient for EvmRpcClient`), the
    private `hex_encode`/`hex_decode` helpers, and the local
    `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError` copies. Those three
    duplicated the live `http_types` types and existed only to serve the
    placeholder; `HttpRpcClient` parses hex through its own
    `parse_hex_u64`/`parse_hex_wei`/`parse_hex_bytes` and has its own tests
    for them, so no parsing coverage was lost.
  - The module doc was corrected: it described "a concrete HTTP-based RPC
    client" that was in fact the placeholder. It now states what the module
    is — the `ChainClient` trait plus its re-exported receipt/log types — and
    points at `HttpRpcClient` for the concrete implementation.
  - Thirteen tests in the module went with it, including the vacuous
    `test_evm_client_creation` and `test_evm_client_call_returns_error_without_reqwest`
    (which asserted the placeholder *stayed* broken). The one test with real
    value, `test_event_filter_builder`, is retained.
- This is a removal, so it cannot be proven by a failing assertion the way a
  behaviour fix can. It is verified by construction instead: `grep -rn` finds
  no reference to `EvmRpcClient` anywhere in the tree (or in `docs/`), and the
  crate compiles without it across the whole feature matrix, including
  `--features blockchain` and `--features web3` on their own.

### Fixed (2026-09-26) — a `Retry-After` date was read as no hint at all
- **Half of RFC 7231 was unimplemented, and the doc said otherwise.**
  `parse_retry_after` claimed to "parse an RFC 7231 `Retry-After` value" but
  only handled the `delta-seconds` form (`Retry-After: 30`). The `HTTP-date`
  form — `Retry-After: Wed, 21 Oct 2015 07:28:00 GMT`, which plenty of
  gateways and CDNs emit — fell through to `None`, so a `429` that told us
  exactly how long to wait was retried after our own 250 ms guess instead,
  burned both retries inside a second, and reported a failure the server had
  already told us how to avoid. The old doc justified the gap as
  "untestable"; that is false — the only dependency is the wall clock, and
  passing it in makes the conversion exact.
  - `parse_retry_after(value, now)` now converts an `HTTP-date` to a delta
    against an injected `now`. The caller passes `SystemTime::now()`;
    `retry_after_hint` threads it through.
  - All three formats a recipient is required to accept are parsed:
    IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the obsolete RFC 850 form
    (`Sunday, 06-Nov-94 08:49:37 GMT`), and asctime
    (`Sun Nov  6 08:49:37 1994`). The RFC 850 two-digit year follows the
    RFC 7231 rule: a year more than 50 years ahead is read as the most recent
    past year with the same last two digits.
  - `days_from_civil` (Hinnant) is the new civil→Unix conversion; it is
    cross-checked against the core library's existing `unix_to_calendar`,
    which tests the direction that already had an implementation.
  - A date already in the past yields a zero delta, which `retry_delay`
    floors at the exponential backoff — so the header still only *extends* a
    wait, never shortens it below the backoff, and the 30 s cap still bounds
    a hostile value.
- Regression tests, verified to fail against the old code (with the date
  conversion stubbed to `None`, `retry_after_parses_the_http_date_form`
  failed `left: None` / `right: Some(30s)` and
  `retry_after_is_read_off_the_response_headers` failed `left: None` /
  `right: Some(9s)`):
  - `retry_after_parses_the_http_date_form` — IMF-fixdate, asctime and
    RFC 850 against a fixed `now`, plus four malformed values that still
    degrade to the backoff.
  - `an_rfc850_two_digit_year_is_resolved_against_now` — the 50-year window.
  - `http_date_conversion_matches_unix_to_calendar` — agrees with the
    library's own calendar conversion on four instants.
  - `a_429_with_a_date_retry_after_is_waited_out_for_the_delta` — end to end
    through the mock server, so the conversion is proven wired into the retry
    path and not merely unit-tested.

### Fixed (2026-09-26) — the ESP32 advertised ten sensors it cannot read, and hid the one it can
- **One tool, twelve sensor names, and a target that supports two.** Round 21
  stopped the ESP32 prompt from advertising whole tools the board has no
  hardware for, via `ToolHandler::serves()`. But `read_sensor` is a *single*
  tool whose argument names the capability: the registered description lists
  twelve sensors (`temperature, accelerometer, humidity, pressure, light,
  heart_rate, hrv, glucose, ecg, stress, battery, memory`), while
  `Esp32ToolHandler::read_sensor` runs exactly two (`temperature`, `memory`) and
  answers everything else with `unsupported sensor: <name>`. So the LLM system
  prompt still promised the model a battery, accelerometer, humidity, pressure,
  light, heart rate, HRV, glucose, ECG and stress reading on a board that cannot
  produce any of them — and, in the other direction, never mentioned `memory`,
  which the board *can* read.
  - `ToolHandler` gained `describe_tool(&self, name) -> Option<&'static str>`,
    the argument-level analogue of `serves()`: it lets a handler override *what*
    a served tool claims. The default returns `None`, so every existing handler
    and the handler-less simulated registry are byte-for-byte unchanged.
  - `ToolRegistry::describe_filtered` uses the override in the served view only
    (`describe_served`), mirroring how `serves()` is gated; plain `describe()`
    still reports the registry as registered.
  - `Esp32ToolHandler` now describes `read_sensor` as what it really is —
    `Read a real device value. args: sensor=temperature (chip die temperature) |
    sensor=memory (free heap)` — so ten phantom sensors leave the prompt and the
    one real status reading enters it.
- Regression tests, both verified to fail against the old code:
  `a_handler_can_narrow_a_served_tools_description` (tools.rs, pre-fix output was
  literally `- read_sensor: Read a sensor value (… battery)`) and
  `the_llm_prompt_does_not_advertise_sensors_the_handler_cannot_read` (agent.rs,
  exercising the real `build_llm_system_prompt` under the embedded features).
- **Boundary left as designed:** `MiniAgent::pick_tool` still emits
  `sensor=battery` for a battery question, because the heuristic planner is
  generic and `battery` is real on the nRF52 and on the host simulator. On the
  ESP32 that reaches `read_sensor`'s `else` arm and returns the honest
  `unsupported sensor: battery` (success=false) — the same refusal contract as
  the other unserved tools, and not a fabricated reading.

### Fixed (2026-09-26) — `magent web3 chain|bind|balance|tx|vc` were documented but unreachable
- **Nine advertised subcommands, all rejected as unknown.** `magent web3 --help`
  listed `chain list` / `chain info` / `balance` / `tx build` / `bind create` /
  `bind verify` / `vc issue` / `vc verify` / `vc present`, and the executor
  module (`cli/src/web3_blockchain.rs`) existed behind the `blockchain` feature
  with display structs and renderers — but no parser arm ever produced a
  `BlockchainAction`, and `Web3Action` had no blockchain variant. Every
  documented invocation died at `error: web3 chain (try …)`, the
  unknown-subcommand path: the surface was declared but never wired to the CLI.
  - `Web3Action` gained a `Blockchain(BlockchainAction)` variant (and dropped
    `PartialEq`/`Eq`, which the `serde_json::Value` it carries cannot satisfy;
    nothing compared `Web3Action` with `==`), and `cli.rs` gained the parser
    arms plus flag parsers for all nine commands.
  - All nine are now genuinely implemented on the real core APIs, not stubs:
    `balance` performs a live JSON-RPC `eth_getBalance`; `bind create` derives
    the address from the secp256k1 key, signs an EIP-191 `BindingClaim`, and
    **verifies the proof it just signed before emitting it**; `bind verify`
    checks the signature *and* cross-checks the proof's metadata against the
    signed message; `tx build` encodes a real unsigned `TransactionRequest`;
    `vc issue`/`vc present` sign real Ed25519 credentials and presentations;
    `vc verify` checks structure and expiry. `chain list` enumerates
    `KnownChain::ALL` (new).
  - `KnownChain` gained `ALL`, `from_name` (canonical name, display name, or
    decimal chain id, case-insensitive) and `canonical_name`, so the list and
    every chain-taking command resolve through one source.
- **A tampered binding proof verified as valid.** `BindingProof::verify` only
  recovers the signer of the proof's `message`; the proof's own `did` /
  `address` / `chain_id` are metadata nothing binds to the signed bytes. Editing
  the DID alone therefore left the signature valid and `bind verify` printed
  `✓ Binding is valid`. The verifier now also parses the `did:` / `address:` /
  `chainId:` lines out of the signed message and compares them to the proof's
  fields, and cross-checks any `binding` record in the envelope — a swapped DID
  or chain id now reports `✗ Binding is invalid or not found`. Regression test
  `message_binds_metadata_detects_a_tampered_did` asserts both halves (the
  signature still verifies; the metadata check does not).
- **`blockchain` did not pull in the credential module the `vc` commands use.**
  `magent-core`'s `blockchain` feature now includes `verifiable_credentials`
  (sharing its Ed25519/bs58 dependencies, so no new crates), and two latent
  lint/quality defects in that module and its integration test — surfaced by the
  new feature path — were fixed (a non-derivable `Default`, a missing doc, a
  needless `mut`, a `match`-for-`matches!`, two needless borrows).
- Regression test `web3_blockchain_subcommands_route` (cli.rs) drives
  `Args::parse` for every one of the nine forms and was verified to fail against
  the old parser (reverting the arms sends `chain list` back to "unknown
  subcommand"). Offline round-trip tests cover `wei_from_eth`, `decode_hex`,
  `new_id`, the bind issue→verify→tamper path, and the vc issue→verify path.

### Fixed (2026-09-26) — the advertised sleep tool could only ever fail in a CLI run
- **The prompt advertises `get_sleep_quality`; on the CLI's own tool backend it
  always errored.** `SimulatorExecutor` — the backend every `magent run` and
  `--repl` session uses — wraps `AgentSimulator`, and round 16 deliberately made
  a *fresh* simulator report no sleep (`AgentSimulator::new` starts empty: a
  device that has not been worn overnight has nothing to report, and the tool
  says so with `MissingField { field: "sleep" }` rather than inventing a score).
  That is right for a fresh device, but nothing in the tree ever called
  `simulate_night` outside tests, so the simulated device was *permanently*
  fresh: asking the CLI "how did I sleep last night" answered, every time,
  `The tool did not complete: ConfigurationError { field: "sleep", reason:
  MissingField }`. The one advertised sleep tool was unusable in the one place
  a user could exercise it.
  - `SimulatorExecutor::new()` now seeds the wrapped device with a week of
    nights (`simulate_night` over `[430, 452, 468, 471, 489, 495, 502]` min),
    the way a device the user has been wearing would carry history. The seeding
    lives at the *executor* layer, not in `AgentSimulator::new`, so round 16's
    decision stands untouched: a freshly-constructed simulator still honestly
    has no sleep, and `sleep_quality_without_a_recorded_night_is_an_error_not_a_score`
    still holds.
  - Seven nights is the window `get_sleep_quality` caps its average and debt at,
    so the seeded history exercises the full report (trend, average, debt)
    rather than a single night dressed up as one.
  - Regression test `the_cli_tool_backend_can_answer_the_advertised_sleep_tool`
    (real_tools.rs), verified to fail against the old code with exactly the
    `MissingField { field: "sleep" }` error a user saw. `magent run --mock 'how
    did I sleep last night'` now reports a computed score
    (`score 88/100 (Excellent) … last 7 nights avg 87 | sleep debt 0.9 h`).

### Fixed (2026-09-26) — the ESP32's prompt advertised seven tools the board cannot run
- **On a real target the registry is the wrong thing to advertise.** `MiniAgent`
  builds its LLM prompt from `self.tools.describe()` — the *registered* set —
  but the registry holds the whole built-in hardware pack (the health sensors,
  `voice_output`, `send_notification`, `ble_send`) while `Esp32ToolHandler`
  serves only eight names. So the prompt told the model it could read heart
  rate, glucose and ECG, speak, notify, and send over BLE on an ESP32 dev board
  that has none of that hardware (BLE was removed from `esp32-app` outright) —
  7 of the 15 advertised names, every one of which came back "not implemented
  on this target". The `ToolHandler` doc already required a handler to "cover
  every tool it advertises"; nothing enforced it.
  - `ToolHandler` gained `fn serves(&self, name: &str) -> bool`, defaulting to
    `true` so every existing handler and the simulated path are unchanged.
  - `ToolRegistry::describe_served()` lists only what the installed handler can
    serve (and logs how many it withheld); `describe()` is untouched. Both the
    embedded prompt (`build_llm_system_prompt`) and the skill/conversation
    injection (`inject_skills`, which makes the same promise) now use it.
  - `Esp32ToolHandler` declares its eight served names in one `SERVED` list; the
    `handle` fallback logs an error if a name is listed there with no arm, so
    the two cannot drift silently.
  - Two regression tests, both verified to fail against the old code:
    `the_prompt_lists_only_the_tools_the_handler_can_serve` (tools.rs, with a
    one-tool handler; the fixed prompt still contains `ble_send` pre-fix) and
    `the_llm_prompt_omits_tools_the_installed_handler_cannot_serve` (agent.rs,
    under the embedded features — note this test only compiles when a
    chip-family feature is on, which is exactly when the prompt exists).

### Fixed (2026-09-26) — the sensor vocabulary was a three-way contract, and only one list was right
- **`read_sensor`'s sensor names disagreed across three components, and the
  divergence broke the exact property the code claimed to preserve.** The
  host simulator's arm documents that its routing and `tools.rs`'s "must
  agree: a prompt tuned with `--mock` has to behave the same on the device it
  is being tuned for" — but they did not. `tools.rs::execute_read_sensor`
  resolves **12** names (`battery` and `memory` included, with a value table
  and a passing coverage test), `simulator::parse_sensor` resolved **10** (both
  missing), and `HEALTH_SYSTEM_PROMPT` advertised **9** (`stress` was reachable
  only through the separate `check_stress_level()` alias, and the model was
  never told `battery` or `memory` existed at all).
  - The CLI's executor is `SimulatorExecutor`, which delegates straight to
    `AgentSimulator::execute_tool` → `parse_sensor`. So a prompt that emitted
    `sensor=battery` or `sensor=memory` succeeded against the device registry
    and failed on the host with `InvalidValue` — the host tuning surface
    rejecting calls the device accepts.
  - `tools::SENSOR_NAMES` is now the single source of truth (12 names), and
    each of the three places has a guard test that iterates it. The simulator
    gained `SimSensorType::Battery` / `Memory` and the real readings behind
    them (`read_battery_mv` starts at 85%, matching the device's canned
    `Battery:85%`; `read_free_heap`); `parse_sensor` gained the `batt` / `mem` /
    `heap` / `free_heap` / `sram` aliases; the prompt's `read_sensor` line now
    lists every name the executor resolves.
  - The prompt guard is non-vacuous by construction: it asserts both parsed
    lists are ≥12 long before comparing, because a parser that matched nothing
    would make the sets trivially equal.
- **The offline planner could not reach the two newly-advertised device
  sensors.** `battery` and `memory` were now legal calls the executor could
  serve, but `--mock` fell through to the generic default and answered a
  battery question with "No tool call needed". Both now have routes. The
  `memory` route is guarded against the flash phrasings it shares a word with:
  `"read from flash memory"` still routes to `flash_read`, asserted by a test.
  - Three regression tests, all verified to fail against the old code:
    `every_canonical_sensor_resolves_on_the_host_executor` (fails with
    `battery` → `InvalidValue`), `the_prompt_advertises_exactly_the_canonical_sensors`
    (fails with the 9-name parse), and
    `device_status_sensors_route_without_hijacking_flash` (fails with no
    `read_sensor` call planned).

### Fixed (2026-09-25) — the prompt was a contract nothing checked, and `/tools` listed a different tool set
- **Nothing enforced that the CLI executor could serve the tools the system
  prompt advertises.** Rounds 14-18 each added one missing executor arm after
  discovering it by hand; the next one would have gone the same way. The
  prompt's tool list is now parsed at run time
  (`agent_runner::advertised_tools()`, reading `HEALTH_SYSTEM_PROMPT` itself —
  a hand-maintained second list is precisely what drifts) and the simulator has
  a guard test that walks every advertised name and fails on any that reaches
  the unknown-tool arm.
  - The guard checks the *error*, not `Ok` vs `Err`: most of these tools
    legitimately fail a bare `{}` call (`get_sleep_quality` with no recorded
    night, `start_exercise` with no type). What must never happen is
    `MissingField { field: "tool" }` — the executor not knowing the name.
  - Removing the three arms added in rounds 16 and 18 makes the guard name
    exactly those three, `["get_sleep_quality", "notify_emergency_contact",
    "find_nearest_hospital"]`. One test would have caught all three rounds.
  - The guard also guards itself: it asserts the parse produced at least 24
    names and contains five known ones, because a parser that silently matched
    nothing would make the loop pass while checking nothing. **That assertion
    immediately caught a bug in the parser itself** (the first version sliced
    the name up to `)`, so `read_sensor(sensor` failed the name check and all
    24 lines were skipped).
- **The REPL's `/tools` command listed a different tool set from the one the
  model is given.** It read `ollama::TOOL_DEFINITIONS` — the nRF52-era
  five-tool catalog — on the stated belief that it was "the same catalog the
  LLM is handed". It is not: the runner sends the system prompt, which
  advertises 24. `/tools` was telling the user that `start_exercise`,
  `get_sleep_quality`, `notify_emergency_contact` and the rest did not exist,
  while the model was calling them. It now renders the live prompt's tool
  lines, so the signatures and descriptions shown are the ones the model was
  actually given, and the listing cannot drift from them again.
  - Two tests: the simulator guard above (which also asserts the parsed
    signature and description, since `/tools` renders both), and a CLI REPL
    test requiring `start_exercise(type)`, `get_sleep_quality()` and
    `notify_emergency_contact(name, message)` in the output — which fails
    against the old five-tool listing.

### Fixed (2026-09-25) — the two emergency tools were advertised, implemented nowhere, and backed by a module with no caller
- **`notify_emergency_contact` and `find_nearest_hospital` existed only as lines
  in `HEALTH_SYSTEM_PROMPT` (`agent_runner.rs:745-746`).** No executor had an
  arm for either: both returned `ConfigurationError { field: "tool", reason:
  MissingField }` from the simulator's unknown-tool arm, and neither name
  appeared in `tools.rs`, `host/simulator`, or the ESP32's `local_tools.rs`.
  They are now implemented, backed by `early_warning::EarlyWarningSystem` — an
  1809-line chronic-disease engine (threshold evaluator, cooldown-limited
  emergency protocol that decides *which* contact and *what* message, and a
  distance-ranked hospital list) that had shipped with **no caller at all**.
  It was the last orphaned module in the tree.
  - `notify_emergency_contact` resolves the contact through the module's
    registry. **No name** means the primary contact — `add_emergency_contact`
    keeps the list sorted by priority, and that ordering exists for exactly
    this. A name that is *not* in the registry is a different mistake from an
    empty registry and is reported as such (`InvalidValue` vs `MissingField`),
    so the model can tell a typo from "nothing is configured".
  - The notification is queued through the module and **read back** to confirm
    it landed, rather than assuming it. It is reported as *queued, awaiting
    delivery*, never as sent: `magent-core` has no radio or SMS stack, and
    claiming delivery would be the fabricated-success bug in a new place.
  - `find_nearest_hospital` uses `nearest_er()` / `nearest_with_specialty()`
    over the same registry, so "nearest with cardiology" is decided by the
    module's distance ranking rather than re-sorted here. An unknown specialty
    is `InvalidValue`; a *known* specialty that no registered hospital has is a
    missing hospital — it is never answered with a different hospital, because
    sending someone to the wrong department is worse than saying nothing.
- **`EarlyWarningSystem::queue_notification`** is the module's new entry point
  for a caller-decided notification (a named contact and a message that did not
  come from a threshold crossing). It shares the alert protocol's
  awaiting-delivery slot but deliberately skips its 30-minute cooldown — that
  window exists to rate-limit *repeated* sensor alerts, and suppressing a
  user's explicit request would be the device silently ignoring them. Its
  `alert_id` is `0`, meaning "not raised by an alert"; reusing the alert
  counter would invent an alert that never happened.
- The device's emergency profile is simulated like every other reading (a
  provisioned device carries a contact and nearby hospitals; a device cannot
  discover either), and the registry is seeded non-empty for that reason — an
  empty one would make both tools unable to do anything but fail.
- Seven new tests (five in `simulator::tests`, two in
  `simulated_planner_tests`) — **all seven fail against the pre-fix code**,
  five with the unknown-tool error and two by planning a temperature read.

### Fixed (2026-09-25) — the offline planner answered every unrecognised task with a temperature reading
- **The `--mock` planner had no route for any of the health tools the system
  prompt advertises.** Every health task — "how did I sleep last night", "start
  a running session", "set do not disturb", "what is my heart rate" — fell
  through to the default arm, which answered with
  `{"tool": "read_sensor", "args": {"sensor": "temperature"}}` and then
  reported success. The offline planner, the one path that exists to
  *demonstrate* the tool set without a model, was demonstrating the wrong tool
  and never touching the rounds 14–16 work. Ten routes now cover the advertised
  health set (`get_sleep_quality`, `read_heart_rate`, `check_stress_level`,
  `read_glucose`, `read_ecg`, `send_meditation`, `set_dnd`, `send_health_alert`,
  and the exercise quartet), ordered ahead of the generic
  `["health", "status", "verify", "system"]` arm that would otherwise swallow
  "health alert".
  - Inside the exercise block, *end* and *adjust/get* are matched before
    *start*: "end my run" and "adjust my exercise goal" both contain
    "exercise"/"run", and the start route would otherwise win.
  - `start_exercise` is called with the activity the task named (`cycling`,
    `swimming`, `yoga`, `hiit`, `walking`, `strength`), falling back to
    `running` only when the task names no type.
- **The default arm ran a tool the task never asked for.** An unmatched task
  ("hello") produced a temperature reading and then "Task completed
  successfully" — an unrelated action against real state, reported as the
  answer. It now returns
  `{"result": "No tool call needed for this task (simulated planner)"}`. The
  same bug existed in `host/simulator`'s independent planner and is fixed
  there too.
- **A tool that failed was reported as done.** The canned completion text
  assumes the tool it just called worked; on a failure the planner still said
  "Sleep quality reviewed" (or "Health check complete - all systems
  operational" after the BLE notify step errored). The planner now reads the
  last tool observation and answers
  `"The tool did not complete: <error>"`, JSON-escaped so a
  `ConfigurationError { field: "sleep", .. }` payload survives `parse_result`
  instead of being silently dropped.
  - Two existing tests encoded the fabricated success and were corrected:
    `sim_environmental_monitoring_runs_full_chain` and
    `sim_temperature_with_fan_control` stubbed only the first tool of a
    multi-step chain, so a later step errored and the old code glossed over it.
    They now stub every step the chain calls, and a new integration test
    (`sim_a_failing_step_in_a_chain_is_reported_not_glossed_over`) pins the
    honest-failure behaviour. The REPL turn-keying test moved its first turn
    off "check the system health" (whose final `ble_send` cannot succeed with
    no peer) onto a route that completes.
- Five new tests in `agent_runner::simulated_planner_tests` (route coverage for
  14 task→tool pairs, route ordering, activity extraction, empty default,
  failure reporting) — **all five fail against the pre-fix planner**, four of
  them because the health task planned `read_sensor`.

### Fixed (2026-09-25) — the last advertised tool, and a sleep-debt number invented from missing nights
- **`get_sleep_quality` answered the unknown-tool arm.** It is documented to the
  model at `agent_runner.rs:736` but had no arm in the simulator's
  `execute_tool`. It is now backed by `sleep_manager::SleepManager` — a 928-line
  module with a sleep-quality scorer, a 30-night history and a circadian model
  that had shipped with **no caller at all** (the third and last orphaned
  wearable module to be wired, after `sports_coach` and `voice_notification`).
  - A device that has not recorded a night has nothing to report, and the tool
    says so (`MissingField { field: "sleep" }`) rather than reporting an overall
    score. A fabricated sleep metric is a fabricated health claim.
  - `AgentSimulator::simulate_night(duration_min)` generates a night from the
    same resting HR/HRV model the daytime reads use; the *score* is then
    computed by `SleepQuality::new`, not written by hand. The reply carries the
    module's score, its rating band, and the 7-night trend and debt.
- **`SleepManager::calculate_sleep_debt` turned missing nights into maximum
  debt.** With three recorded nights, `calculate_sleep_debt(7)` assumed four
  nights of *zero* sleep and returned `56 h − 18 h = 38 h`. Its sibling
  `average_sleep_score` already capped its window at the history length; the two
  methods disagreed about what "the last 7 nights" means over the same data. The
  window is now capped the same way, so an unworn device reports **0 h** of debt
  instead of a week's worth.
  - This also removed a fabricated recommendation:
    `get_sleep_recommendations` emits "本周睡眠债务约56.0小时，建议增加睡眠时间"
    whenever the debt exceeds 2 h, so a device with no sleep data was telling
    the user to sleep more.
  - The old behaviour was documented as intentional ("no data = worst case").
    It was a bug with a comment: the module had zero consumers, so nothing ever
    exercised the claim, and a device that has been worn three nights cannot
    have a week of debt.

### Fixed (2026-09-25) — the prompt's alert and notification tools, and the second module with no caller
- **`queue_voice`, `send_health_alert`, `send_meditation` and `set_dnd` answered
  the unknown-tool arm.** All four are documented to the model at
  `agent_runner.rs:731-734`, but the simulator's `execute_tool` had no arm for
  any of them, so every call returned
  `ConfigurationError { field: "tool", reason: MissingField }`. Six of the seven
  new behaviour tests fail against the old code with exactly that error.
  - They are now backed by `voice_notification::VoiceNotificationManager`, a
    991-line module with a priority queue, a multi-channel alert and a
    do-not-disturb window that had shipped with **no caller at all**. This is
    its first consumer — the second orphaned module wired in as many rounds
    (after `sports_coach`).
  - `queue_voice` reaches the real priority queue; `set_dnd` really moves the
    window, so a following low-priority cue is routed away while an urgent one
    (priority ≥ 8) still gets through; `send_health_alert` raises a notification
    and a voice cue; `send_meditation` queues a script.
  - The simulator's timestamps are **monotonic uptime**, not a wall clock — the
    device is not guaranteed to have one — so a freshly-created simulator is at
    hour 0, which is what a just-booted device sees.
- **A health alert is no longer reported as "delivered" when the pipeline
  dropped it.** The manager routes the voice cue and the notification
  independently and drops either under do-not-disturb, so an unconditional
  "delivered" would be the fabricated-success bug in a new place. The reply now
  reads the queue depth and notification history before and after and reports
  which channels actually fired (`voice queued / not queued`, `notification
  raised / not raised`).
- **Scales and clocks are validated, not clamped.** `severity` and `priority`
  are `1..=10` and a value outside it is `InvalidValue` (clamping `70` would
  store a number nobody asked for); a DND hour is `0..=23`, so `24` is rejected
  rather than wrapped to `0`, which would silently invert the window; absent
  arguments — no `text`, no `script`, no `enabled`, no window — are
  `MissingField`; and a present-but-unreadable bool (`{"enabled":"maybe"}`) is
  `InvalidValue`, distinct from absent. A rejected call changed nothing: no
  queue entry, no notification, no DND.
- Added `extract_bool_arg`, which reads the JSON spelling (`true`), the quoted
  one (`"true"`), `1`/`0`, and the legacy `key=value` form; and two small
  `missing_field` / `invalid_value` constructors, replacing the inline error
  literals the round-14 arms repeated.

### Fixed (2026-09-25) — the prompt's exercise tools were advertised, and the module implementing them had no caller
- **The four exercise tools in `HEALTH_SYSTEM_PROMPT` answered the unknown-tool
  arm.** `start_exercise`, `end_exercise`, `get_exercise_goal` and
  `adjust_exercise_goal` are documented to the model at
  `agent_runner.rs:726-729`, but the simulator's `execute_tool` had no arm for
  any of them, so every call fell through to
  `ConfigurationError { field: "tool", reason: MissingField }` — a model
  following the prompt got "missing field: tool" for all four.
  - They are now backed by `sports_coach::SportsCoach`, a 968-line state machine
    that had shipped with **no consumer at all**. This is its first caller.
  - `start_exercise` sets the goal, enters the session, and sets
    `health_state.is_exercising = true`, so the effect is visible to the rest of
    the simulator: the next `read_heart_rate` returns an exercising rate
    (~141 BPM) rather than a resting one (~70 BPM). The session is a fact, not a
    printed string.
  - `end_exercise` measures the elapsed time from a monotonic clock and drives
    the coach's state machine by that real delta, so a session that lasted no
    measurable time reports `0s` rather than an invented duration.
  - `adjust_exercise_goal(intensity, duration)` stores the adjusted goal, so a
    following `get_exercise_goal` reports it — the two are one round trip.
- **The absent-session and unknown-type cases are errors, not fabricated
  successes.** Ending a session that was never started, and reading a goal
  nobody configured, are `MissingField`; naming a type the build does not know
  is `InvalidValue`; and an intensity outside `0.0..=1.0` (a model writing `70`
  for 70 %) is rejected rather than clamped. A misspelled activity no longer
  becomes a real Running session, and a rejected `start_exercise` leaves
  `is_exercising` false.
- **Removed two dead no-op `impl core::fmt::Write` blocks** —
  `sports_coach.rs::ExerciseGoal` and `early_warning.rs::EarlyWarningSystem`.
  Each implemented `write_str` as `Ok(())`, discarding the input: a `write!`
  into one of these types would have silently produced an empty string. Neither
  was reachable (all writes target a heapless `String`), and both were removed
  rather than left as a trap.


- **`send_with_retry` ignored the server's `Retry-After`.** A rate limiter that
  answered `429 Too Many Requests` with `Retry-After: 30` was retried after the
  client's own `250 ms` backoff, and again after `500 ms` — the whole budget
  spent inside a second, followed by a reported failure for a request the
  server had already explained how to complete. The header is the one piece of
  information that beats our guess at how long a transient condition lasts.
  - The wait is now `retry_delay(attempt, hint)` = the server's hint when it
    asks for **longer**, never shorter than our own backoff (a `Retry-After: 0`
    from a limiter that is still shedding load must not become a busy loop),
    capped at 30 s because the value comes from a remote server.
  - Only the `delta-seconds` form was honoured here; the `HTTP-date` form was
    left unimplemented because it "needs the wall clock" (see the 2026-09-26
    entry above — that reasoning was wrong, and round 25 landed the date form).
  - The wait is read off *this* response, before the next attempt consumes it,
    and the retry log line now reports the wait it will actually take.
  - `send_with_retry` delegates to a `send_with_retry_sleeping` whose sleep is
    injected, and the mock's `Reply` gained response headers — so the header is
    proven to travel response → delay end to end rather than only through the
    pure functions. Pinned by
    `a_429_with_retry_after_is_waited_out_for_the_header_value` — verified to
    fail against the old arithmetic with `left: [250ms], right: [7s]` — plus
    its control `a_429_without_the_header_keeps_the_ordinary_backoff`, which
    rules out a client that simply always waits.

### Fixed (2026-09-25) — a response naming two tools ran one, and a REPL turn reused the last turn's call id
- **`parse_tool_call` returned a single `Option`, and its OpenAI-shape arm read
  `tool_calls[0]` and stopped.** A model that batches independent reads into one
  `tool_calls` array — the standard way to ask for temperature *and* humidity —
  had every call but the first discarded: no error, no trace, and a next turn
  that saw a conversation in which it had asked for only the first. The reading
  it explicitly requested silently never happened.
  - The ReAct loop now parses **all** calls (`parse_tool_calls`) and executes
    each as its own assistant-tool-call / tool-result pair, so every announced
    id is answered and the request stays well-formed if the budget runs out.
    `parse_tool_call` is retained and delegates, so no caller changed.
  - Pinned by `two_tool_calls_in_one_response_are_both_executed` (fails against
    the old code with `tool_call_count == 1` after two calls were asked for) and
    `every_tool_result_is_paired_with_its_assistant_call`.
- **Tool-call ids were allocated from the per-run budget counter, so each REPL
  turn re-issued `call_1`.** `run_continuing` keeps the history but resets
  `tool_call_count`, so turn two's first call carried the same id as turn one's
  still-present call — one conversation holding two different calls under one
  id, with pairing left to position rather than to the `tool_call_id` the API
  pairs on.
  - Ids now come from a session-long `call_id_seq` that is never reset between
    turns. `prune_dangling_tool_pairs`' position rule is retained and its
    comment corrected: it still guards a hand-built list, a history persisted by
    an older version, or a window in which compression dropped a call.
  - Pinned by `repl_turns_do_not_reissue_a_tool_call_id` — verified to fail
    against the old code with `["call_1", "call_1"]`.

### Fixed (2026-09-25) — the fourth executor still had the bugs the other three had already fixed
- **`host/simulator/src/main.rs` (`magent-simulator`) is a fourth implementation
  of the same tool set — independent of `magent-core`, with, until this round,
  **zero tests** — and it still carried all three bug classes that rounds 6, 7
  and 9 removed from the other copies.** The same prompt therefore behaved
  differently depending on which executor answered it.
  - **Unknown sensors fell through to temperature.** `read_sensor` matched five
    names and sent everything else to `_ => SensorType::Temperature`, so a call
    for `glucose`, `heart_rate`, `ecg`, `stress` or `battery` answered
    `23.5°C` with `success: true` and no sign the reading came from a different
    sensor. `magent-core`'s simulator and the firmware both refuse an unknown
    sensor; this copy now does too, via a shared `parse_sensor` that also
    accepts the alias spellings (`temp`, `humid`, `press`, `accel`, `lux`) the
    other executors accept. A missing `sensor` is now a missing argument rather
    than a silent request for a temperature.
  - **GPIO state semantics diverged from the firmware.** `state` defaulted to
    `"low"` and anything not exactly `"high"` became `Low`, so `state:"1"` drove
    the pin **low** where the firmware drives it high, an omitted state drove it
    low where the firmware defaults to high, and `state:"mid"` was accepted
    silently as low. All three now match `firmware/esp32-app`'s handler.
  - **A missing `pin` or flash `address` became `0`.** `unwrap_or(0)` in
    `write_gpio`, `flash_read` and `flash_write` turned a call that named no pin
    into a real write to pin 0. All three are now `ConfigurationError`s, in all
    four executors that shared the pattern (`magent-core/src/tools.rs`'s
    `required_arg_parse`, `magent-core/src/simulator.rs`'s `required_int_arg`,
    and this copy) so no inconsistency was introduced.
  - Pinned by six new tests — the crate's first — each verified to fail against
    the old code (e.g. `glucose must not report success, got "24.2°C"`;
    `state="1" must drive the pin high — left: Low, right: High`).

### Fixed (2026-09-25) — a freshly booted device could not raise its first alert
- **The `EarlyWarningSystem` cooldowns treated "never raised" as "raised at
  t=0".** `last_glucose_alert_ms`, `last_ecg_alert_ms` and
  `last_emergency_notification_ms` started at `0`, and each check read that as
  a timestamp — so at boot no glucose alert could fire for the first 5 minutes
  of uptime, no ECG alert for the first minute, and **no emergency for the
  first 30 minutes**. A health monitor that cannot report the emergency at the
  moment it happens has lost the feature it exists for; the readings most
  likely to be critical are the first ones after power-on.
  - Each field is now `Option<u32>` — `None` means *never raised* — and the
    three checks share a `cooldown_elapsed(last, now, cooldown_ms)` helper that
    returns elapsed immediately for `None`. The cooldown still rate-limits
    *repeats*: a second reading inside the window is suppressed, and the
    backwards-clock behaviour is unchanged (a `now` before `last` saturates to
    no elapsed time and suppresses a duplicate rather than firing one).
  - **Nothing caught it because every test used a timestamp past the
    cooldown** — one comment even recorded the workaround ("the 30-minute
    emergency cooldown, which is measured from boot, so a small timestamp is
    suppressed") instead of questioning it. Pinned by
    `the_first_glucose_alert_is_not_swallowed_by_the_cooldown`,
    `the_first_emergency_is_raised_immediately_after_boot` and
    `a_second_alert_inside_the_cooldown_is_still_suppressed` — the first two
    verified to fail against the old arithmetic, the third to prove the fix did
    not simply delete the cooldown.
  - Note: `early_warning` is a published module (`pub mod early_warning`) with
    a real API, but still has no in-tree consumer — see the backlog item
    carried from round 8.

### Fixed (2026-09-25) — the prompt's zero-argument health tools answered "Invalid sensor type" on every target
- **The default system prompt (`HEALTH_SYSTEM_PROMPT`, which *is* the CLI's
  default via `RunnerConfig::default()`) documents `read_heart_rate()`,
  `read_glucose()`, `read_ecg()` and `check_stress_level()` as taking **no
  arguments**, and the device registry registers the first three as
  `read_sensor` aliases. The alias never supplied the sensor it was an alias
  *for*.** `ToolRegistry::execute` handed `execute_read_sensor` the caller's
  raw argument string — empty for a zero-arg call — so the sensor resolved to
  `""` and every advertised call answered
  `data: "Unknown sensor", error: "Invalid sensor type"`. A model following
  the prompt got a failure for the four calls it was told to make.
  - Dispatch now splits `execute_read_sensor` into
    `execute_read_sensor_as(implied, args)`: an explicit `sensor=` value (or a
    bare token) still wins, and the alias's implied sensor is used only when
    the caller named none. `read_heart_rate` → `heart_rate`, `read_glucose` →
    `glucose`, `read_ecg` → `ecg`, and a new `ToolType::StressLevel` →
    `stress` for `check_stress_level`, which is now registered by
    `register_builtin_tools` alongside the other three.
  - **The host executor had no arm for any of the four at all.**
    `AgentSimulator::execute_tool` — the CLI's real executor, behind
    `SimulatorExecutor` — matched neither the aliases nor `check_stress_level`,
    so they fell to the unknown-tool `MissingField` error. This is the same
    third-executor divergence the earlier rounds kept finding: the render arm
    is now a shared `render_sensor` used by `read_sensor` and all four aliases.
  - Pinned by `sensor_aliases_supply_their_own_sensor` (device registry) and
    `read_sensor_aliases_resolve_on_the_host_executor` (host executor), both
    verified to fail against the old dispatch (`Unknown sensor` /
    `ConfigurationError { field: "tool", reason: MissingField }`).
  - Still unbacked and left `[ ]` in the backlog: the prompt's exercise,
    sleep-quality, meditation, DND and emergency-contact tools. They name no
    state this codebase keeps, and inventing one is the placeholder pattern
    these rounds are removing.

### Fixed (2026-09-25) — the emergency protocol was a no-op that reported nothing
- **`EarlyWarningSystem::trigger_emergency_protocol` checked a cooldown, stored
  the timestamp, and returned.** A `Critical` alert — a glucose crash, an
  abnormal rhythm — ran that path and nothing else happened: no message was
  built (`generate_emergency_message` existed but was unreachable from it),
  `HealthAlert::mark_emergency_notified` was dead code, and the alert reached
  the caller with `emergency_contacted: false`. Nothing distinguished a
  handled emergency from an ignored one.
  - The protocol now **decides**: it picks the highest-priority SMS-capable
    contact (the list is kept sorted by priority), builds the notification
    from the module's existing message builder — hospital and all — records it
    as a new `EmergencyNotification`, and marks the alert. Delivery belongs to
    the layer that owns a transport; `magent-core` under `no_std` has neither
    radio nor SMS, so pretending to send would be the same lie in a new place.
    The notification is handed over via
    `pending_emergency_notification()` / `take_emergency_notification()`, and
    is `Serialize` so a host can put it on the wire.
  - **The cooldown arithmetic could not survive a non-monotonic clock.**
    `current_time_ms - self.last_*_ms` was unchecked in three places (glucose,
    ECG, emergency). A timestamp that steps backwards — an NTP correction, a
    `u32` millisecond counter wrapping after ~49 days — was a **debug panic**
    and, in release, a wrapped value that read as "well past the cooldown",
    firing a duplicate alert. All three now use `saturating_sub`, which is
    identical whenever the clock advances and suppresses the reading when it
    does not.
  - **The recorded alert disagreed with the returned one.** The call sites
    pushed a clone into `alert_history` *before* raising the protocol, so the
    history entry stayed `emergency_contacted: false` while the returned alert
    said `true` — two views of the same alert. The protocol now runs first and
    the marked alert is what gets recorded.
  - Pinned by six tests, five of them verified to fail against the old no-op
    (`emergency_protocol_raises_a_notification_and_marks_the_alert`,
    `a_second_critical_alert_inside_the_cooldown_is_not_re_raised`,
    `taking_the_notification_drains_it`,
    `an_emergency_with_no_contacts_still_records_the_message`,
    `a_backwards_timestamp_neither_panics_nor_re_raises`), and the arithmetic
    separately proven by restoring the unchecked subtraction and observing
    `attempt to subtract with overflow`; the ordering fix by restoring
    clone-then-trigger and observing the `history must agree` failure.

### Fixed (2026-09-25) — the simulator's `read_sensor` did not understand sensor names the way the firmware does
- **`read_sensor` routed on `args.contains("…")` over the whole payload, and
  was case-sensitive with no aliases.** `tools.rs::execute_read_sensor` had
  already been fixed the other way — its comment records routing on the
  *parsed* `sensor` value precisely so a keyword elsewhere in the call cannot
  hijack the read — but `magent-core/src/simulator.rs` (the CLI's
  `SimulatorExecutor`) still carried the raw substring chain. Two consequences
  a prompt author would hit immediately:
  - **The value's neighbours decided the answer.** `{"sensor":"hrv","source":
    "heart_rate sensor"}` read the **heart rate** — the extra field's mention
    of `heart_rate` matched before `hrv` did — so the tool answered a question
    the model had not asked. Observed directly: `Heart Rate: 70 BPM, HRV: 56.0
    ms` for a call that named `hrv`.
  - **Capitalised and aliased names failed outright.** `{"sensor":
    "Temperature"}` — the capitalisation a model naturally emits — matched
    nothing and the call returned a hard `ConfigurationError`, while the
    firmware's `normalize_sensor` lowercases the name and accepts `temp`,
    `HR`, `accel`, `bg`, `ekg`, `lux`, `baro` and the rest. A prompt tuned
    against `--mock` therefore behaved differently on the device it was being
    tuned for, which is the whole point of the simulator.
  - The branch now resolves the sensor *argument* (the `sensor` key, or the
    legacy bare token) and maps it through a normaliser matching the
    firmware's alias table. Pinned by
    `read_sensor_accepts_the_names_and_case_the_firmware_accepts`,
    `read_sensor_routes_on_the_sensor_value_not_the_whole_payload` and
    `read_sensor_distinguishes_an_unknown_name_from_a_missing_one` — all three
    verified to fail against the substring chain. Also corrected while there:
    a name that is present but unrecognised now reports
    `ConfigError::InvalidValue`, not `MissingField`, matching the distinction
    the GPIO `state` fix draws. Confirmed end-to-end that the sensor path is
    unchanged for the canonical spellings the scaffold emits:
    `magent run --mock 'monitor the environment'` still reads temperature,
    humidity and pressure in turn.

### Fixed (2026-09-25) — the CLI's simulator disagreed with the firmware on three tool contracts
- **`magent-core/src/simulator.rs` was the odd executor out.** Three executors
  implement the same tool set — the embedded one in `magent-core/src/tools.rs`
  (what runs on the device), `host/simulator/src/main.rs`, and this one, which
  backs the CLI's `SimulatorExecutor`. The first two had already been fixed;
  this one still carried the pre-fix behaviour, so a prompt tuned against
  `magent run --mock` would behave differently on the hardware it was tuned
  for.
  - **`write_gpio` decided the level with `args.contains("high")`.** Any
    payload that omitted `state` — and any that used the firmware's numeric
    spelling `"state":"1"` — drove the pin **low**, the opposite of the
    embedded executor's `high` default, while a nonsense value like `"mid"`
    was accepted silently. It now uses the new `extract_str_arg` and matches
    `tools.rs::execute_write_gpio` exactly: `high|1` → High, `low|0` → Low,
    absent → High, anything else → `ConfigurationError { field: "state" }`.
  - **`flash_write` threw the model's `data` away** and stored the literal
    `format!("data at {address}")`, then reported *that* string's length. A
    write-then-read round trip handed the model back text it never wrote. It
    now persists the `data` value and reports its real length.
  - **`ble_send` forwarded the raw argument envelope** (`self.ble.send(args)`),
    so `{"data":"System health OK"}` went over the air as 27 bytes of JSON —
    braces, quotes and key name included — for a 16-character message, and the
    reported count described the envelope. It now sends the `data` value.
  - Pinned by `write_gpio_defaults_to_high_and_rejects_a_bogus_state`,
    `flash_write_stores_the_payload_the_model_sent`,
    `ble_send_transmits_the_message_not_the_args_envelope` and
    `extract_str_arg_accepts_json_and_key_value_payloads` — all three
    behavioural tests verified to fail against the legacy branches (reporting
    `Sent 27 bytes`, `Wrote 12 bytes … ` for the fabricated string, and a
    default `Low`). Confirmed end-to-end: `magent run --mock 'switch on the
    indicator led'` → `GPIO pin 13 set to high`; `… 'save this reading to
    flash storage'` → `Wrote 9 bytes to flash at 0x0400` (the string
    `data at 1024` would have been 12); `… 'send a BLE message'` →
    `Sent 17 bytes via BLE` for the 17-character payload.

### Fixed (2026-09-25) — every integer tool argument was silently discarded
- **`extract_int_arg` only understood `pin=13`, but the loop sends JSON.** The
  ReAct loop serialises `ToolCall::arguments` and hands the executor
  `{"pin":13,"state":"high"}` — which is also what any real model produces —
  while `extract_int_arg` scanned for the legacy `key=` shape. It therefore
  found nothing, every call site fell back to `unwrap_or(0)`, and the tool
  reported success for an argument the model never gave: `write_gpio` with
  `{"pin":13}` switched pin **0** and answered "GPIO pin 0 set to high", and
  `flash_read`/`flash_write` read `0x0000` for any address. A model reasoning
  from the tool result was told it had touched a pin that was not the one it
  asked for. The parser now accepts the JSON object first (a number, or a
  number a model wrote as a string) and keeps the `key=value` scan as a
  fallback for the scripted callers. Pinned by
  `write_gpio_reads_the_pin_from_a_json_payload`,
  `flash_tools_read_the_address_from_a_json_payload` and
  `extract_int_arg_accepts_json_and_key_value_payloads` — all verified to fail
  against the legacy-only parser. Confirmed end-to-end:
  `magent run "turn the led on" --mock` now logs `GPIO pin 13 set to high`
  where it previously logged `GPIO pin 0`.

### Fixed (2026-09-25) — the offline scaffold answered turn one's question for the rest of the session
- **`generate_simulated_response` keyed its script off the *first* user
  message.** That is correct for a single `run`, which starts from a cleared
  history, but `run_continuing` (the REPL) shares one history, so every turn
  after the first was answered with the first turn's script. Observed live:
  `magent run --repl --mock` told `turn the led on` that "Health check
  complete - all systems operational", because turn one had asked about
  system health. The anchor is now the **last** user message
  (`current_turn_task`), which is stable across a turn's iterations — the
  scripting needs that — while moving with the session. This is the same
  mistake as `get_result` scanning past the turn boundary, and the anchor now
  agrees with the one `slice_messages` already uses to decide which task
  compression must not drop. Pinned by
  `a_second_repl_turn_is_answered_by_the_second_task_not_the_first` (verified
  to fail against the first-message anchor, returning the health script) and
  `current_turn_task_returns_the_last_user_message`. Confirmed end-to-end:
  the same scripted REPL session now answers `LED turned on successfully`.

### Fixed (2026-09-25) — `get_result` re-judged accepted answers, and a `--repl` turn could inherit the previous turn's answer
- **A run that answered could still be reported as failed.** `think` accepts a
  well-formed `{"result": …}` envelope, stores the *extracted* text as the
  last assistant message, and sets `Finished`. `get_result` then fed that same
  text back through `parse_result`/`looks_like_chatter` — a weaker test than
  the one that accepted it — so an answer that merely started with a word the
  chatter heuristic claims (`first`, `next`, `ok`, `now `) or was shorter than
  `MIN_PROSE_RESULT_LEN` was rejected *on the way out*, failing a run that had
  answered. The loop's verdict is now authoritative: on the `Finished` path
  the stored result is returned as given. Pinned by
  `a_result_the_loop_accepted_is_not_re_judged_on_the_way_out`.
- **A `--repl` turn with no answer of its own returned the previous turn's.**
  REPL turns share one history and `get_result` scanned *all* of it backwards.
  When a turn produced no assistant text (every iteration a tool call, then the
  budget ran out), the scan walked past the start of the turn and found the
  earlier turn's answer — long and non-chatter-like, so it was handed back and
  the CLI printed a stale answer under `RESULT` for a turn that never answered.
  The scan is now bounded to the current turn, which begins at the last user
  message (`run_inner` pushes the task there). Pinned by
  `a_turn_with_no_answer_of_its_own_does_not_reuse_the_previous_turns`,
  confirmed to fail against the unbounded scan — it returned
  `"the temperature in the lab is 22.5 degrees"`, turn one's answer, as turn
  two's result.

### Fixed (2026-09-25) — the half-pair repair was itself defeated by a REPL session's recycled tool-call ids
- **`prune_dangling_tool_pairs` matched on id alone, and ids repeat every
  turn.** `reset_run_state` restarts `tool_call_count`, so a `--repl`
  session re-issues `call_1` at the top of each turn. A bare set-membership
  test then let a *later* turn's call vouch for an *earlier* turn's orphaned
  `tool` message — the exact half-pair the repair exists to remove, left in
  place, still a provider `400`. The repair is now **order-aware**: a `tool`
  is kept only by a call that *precedes* it, and a call only by a result
  that *follows* it. Position is what makes a pair a pair; uniqueness of the
  id was an assumption the REPL never honoured. Pinned by
  `prune_survives_a_session_that_re_uses_tool_call_ids`, verified to fail
  against the previous set-based implementation (it dropped nothing).
- **A completed run could still report itself mid-flight.** When the loop
  broke on a budget but `get_result` rescued a terse answer, nothing on that
  path assigned a terminal state, so `RunReport.state` read `Observing` for
  a run that returned `Ok` and printed a result. The `Ok` arm now sets
  `Finished`, extending the round-1 invariant — "returned `Ok`" and "state
  is `Finished`" are the same fact, just as `Err` and `Error` are.
- **`get_result`'s comment named two kinds of leftover text; there are
  three.** A bare JSON object the classifier rejects for lacking a `result`
  key (`{"temperature":22.5}`) is neither chatter nor a terse prose answer,
  and is deliberately passed through. The comment now says so, and
  `a_json_shaped_leftover_is_handed_over_rather_than_called_chatter` pins
  the choice as deliberate rather than an oversight of the chatter check.

### Fixed (2026-09-25) — the ReAct loop's context window could cut a tool call in half, and an unconverged run claimed success
- **Compression could orphan half a tool-call pair, and the provider
  rejects half a pair outright.** The conversation window
  (`--max-messages`, default 32) and the unconditional byte-GC both keep
  or drop whole messages *by index*, so a boundary can fall between the
  assistant message carrying `tool_calls: [{id: "call_2"}]` and the `tool`
  message answering it. OpenAI-compatible APIs — DeepSeek, the workspace's
  cloud backend — answer such a request with a `400`, so a long `--repl`
  session (or any session that crossed the 512 KiB context budget) would
  start failing every turn on a malformed history the user could neither
  see nor repair. Both budget functions now call a new
  `conversation::prune_dangling_tool_pairs` before returning, which drops
  only the orphaned half — a well-formed conversation is untouched. The
  repair is gated on the function having actually dropped something, so a
  history that already fit the budget is handed back byte-identical.
  Pinned by `a_slice_boundary_inside_a_pair_does_not_orphan_the_result`
  and `a_byte_gc_cut_does_not_orphan_the_result` (both verified to fail
  without the repair), plus `prune_leaves_a_well_formed_conversation_alone`
  and `prune_drops_both_halves_of_a_split_pair`.
- **`AgentState::Error` was never assigned.** The variant is documented as
  "an unrecoverable error was raised; the loop has been aborted" and the
  loop guard tests for it, but nothing ever set it: a run that died on an
  unreachable backend, on a mid-run LLM error, or on an exhausted budget
  reported `state: "Thinking"` in the `--json` envelope — a failed run
  described as still in progress. `RealAgentRunner::run` now leaves the
  state at `Error` on every `Err`, making "returned `Err`" and "state is
  `Error`" the same fact, while a run that converged still reports
  `Finished`. Pinned by `a_failed_run_leaves_the_runner_in_the_error_state`.
- **Mid-plan chatter was returned as the final answer.** When the loop
  broke on a budget, `get_result` fell back to the last assistant text
  *whatever it was*, so ten iterations of "ok" produced `Ok("ok")` — which
  the CLI printed under its `RESULT` banner and exited `0` on. The fallback
  is now split: text the loop's own classifier rejected as chatter
  (`looks_like_chatter`) errors with the text quoted, while a terse answer
  (`22.5` where the prompt asked for `{"result": 22.5}`) is still handed
  over, because `parse_result` had rejected that only for being shorter
  than `MIN_PROSE_RESULT_LEN`. Pinned by
  `run_exhausts_iteration_budget_on_chatter_and_reports_the_dead_end` and
  `a_terse_answer_below_the_prose_threshold_is_still_returned`. Verified
  end-to-end: `magent run … --mock -i 1` now exits `1` with
  `error: agent failed: run ended without a final answer: …`, and under
  `--json` emits a single `{"ok":false,"code":"run_error","exit":1}`
  document.

### Added (2026-09-25) — a transient LLM failure is retried instead of failing the run
- **The LLM backends sent one request and gave up.** `OllamaClient` and
  `DeepSeekClient` had a 60-second timeout but no retry, so a connection
  reset, a `503` while the model server restarted, or a `429` from a rate
  limiter failed the whole `magent run` — the agent's only real dependency
  was also its least resilient one. (The blockchain HTTP client already
  retried; the LLM path never did.) A shared `send_with_retry` now makes
  up to **3 attempts** (1 + 2 retries) with exponential backoff
  (`250ms → 500ms`, capped at `2s`) around the three call sites:
  Ollama `chat_with_messages`, Ollama `generate`, and DeepSeek
  `chat_with_messages`.
  The retry is deliberately narrow — only conditions a second attempt
  could actually clear: a transport error, or a `408` / `429` / a `5xx`
  response — **except** `501 Not Implemented` and `505 HTTP Version Not
  Supported`, which are `5xx` but permanent, so retrying them would only
  repeat the answer. A deterministic `4xx` (bad request, bad key, not
  found) is returned on the first attempt, so a wrong request is never
  delayed.
  The `Response` is handed back untouched when a retry is not warranted,
  so DeepSeek's `HTTP error: <status> — <body>` diagnostic is preserved.
  A failure that survives every attempt now reads `Request failed after 3
  attempts: …` instead of `Request failed: …`, and the runner still fails
  loudly rather than substituting a fabricated reply.
  **Retries are logged.** Without that, a slow-but-successful run is
  indistinguishable from a fast one: `send_with_retry` emits a
  `magent.llm` `warn` line per backoff naming the attempt, the cause and
  the delay (`--verbose` / `RUST_LOG=magent.llm=warn`), and also states
  the worst case explicitly — a hung backend is retried too, so a call
  can now take `(retries + 1) × timeout` rather than one timeout.
  **The backend probe retries too, and that is the point.**
  `check_connection()` is the run's *first* network call, and its `false`
  is load-bearing: the runner aborts with "LLM backend unreachable" and
  the CLI falls back to simulated answers. Un-retried, one blip at
  startup cost the whole session — and the chat-path retry could never
  help, because the run never got that far.
  Guarded by `llm_retry_tests`, which drives the retry through a real
  throwaway HTTP server (`http_mock`) rather than a faked `Response`:
  a `503` then a `200` recovers end-to-end through both
  `OllamaClient::chat_with_messages` and
  `DeepSeekClient::chat_with_messages`; a `400` and a `501` are each
  answered exactly once; an exhausted budget hands back the last `503`
  response rather than an error; both probes recover from a transient
  `503` and a permanently-down server still reports not-connected after
  a bounded number of attempts; and the DeepSeek case asserts the retry
  re-sent an **identical body and `Authorization` header**, which is the
  invariant the `body.clone()` in the retry closure exists to hold.

### Added (2026-09-24) — a crash now diagnoses itself, and `--help` documents the exit codes
- **A panic printed a bare Rust report and left stdout empty.** `main` now
  installs a hook that keeps the default report (message + source
  location), adds a line saying the failure is a *bug in magent* — not the
  user's command line — and where to report it, and, under `--json`,
  writes `{"ok":false,"error":…,"code":"internal_error","exit":101}` to
  stdout. That closes the last hole in the "stdout is exactly one JSON
  document" promise: a pipeline that pipes through `jq` now sees a
  structured failure instead of nothing when the process dies. The exit
  status is still 101, which is how a caller tells a crash from a handled
  failure (1) or rejected input (2).
  The panic is triggered through the `MAGENT_TEST_PANIC` test seam in
  `main` — inert unless set, and the only deterministic way to reach the
  hook. Guarded by `cli/tests/panics.rs`.
- **The exit codes were documented nowhere.** `magent --help` gained an
  `EXIT CODES:` section covering 0 / 1 / 2 / 101 / 130 / 141 — including
  that code 1 under `--json` still leaves an `ok:false` envelope on
  stdout, and that 141 is ordinary SIGPIPE from `magent … | head` rather
  than an error. Pinned by `help_text_documents_every_exit_code`, so a
  change to `main`'s dispatch has to update the contract it prints.

### Fixed (2026-09-24) — a failed HTTP-client build no longer disables the timeout
- **`OllamaClient` / `DeepSeekClient` fell back to `Client::new()` when
  `ClientBuilder::build()` failed.** That is not a fallback: `Client::new()`
  reruns the same TLS initialisation — and *panics* on the same failure
  instead of returning the error — while silently dropping the 60-second
  timeout, which is the one setting that keeps a stalled connection from
  hanging the agent. A build failure therefore traded a clean `Err` for
  "no timeout, maybe a panic". The client is now stored as a
  `Result<Client, String>` behind an `http_client()` accessor, so
  `chat_with_messages` / `generate` report
  `could not build the HTTP client: …` like any other transport failure,
  and the two best-effort probes (`check_connection`, `get_models`) still
  answer "not connected". Guarded by
  `ollama_client_build_failure_is_reported_not_swallowed` and
  `deepseek_client_build_failure_is_reported_not_swallowed`.

### Added (2026-09-24) — `--json` now fails in JSON
- **A failing `--json` command wrote nothing at all to stdout.** Every
  command still printed human text to stderr and exited non-zero, so a CI
  pipeline doing `magent --json … | jq` got a parse error instead of the
  failure it was trying to detect — the one place a machine-readable
  format is most useful was the one place it went missing. `Output::error`
  is replaced by `Output::fail(code, exit, msg)`, which in JSON mode
  writes a self-describing envelope to stdout alongside the stderr line:
  ```json
  { "ok": false, "error": "…", "code": "run_error", "exit": 1 }
  ```
  `code` is a stable per-dispatch tag (`run_error`, `set_prompt_error`,
  `summary_error`, `config_error`, `scheduler_error`,
  `scheduler_interrupted`, `scheduler_tasks_failed`, `web3_error`), and
  `exit` mirrors the process status. `ok` matches the field `doctor`
  already emits, so one predicate covers every command. Human mode is
  unchanged: `error: …` on stderr, empty stdout. Parse errors (bad flags)
  still print usage on stderr without an envelope — they happen before an
  `Output` exists, and the caller couldn't have parsed `--json` anyway.
  Guarded by `json_failure_emits_a_single_error_envelope` and
  `human_failure_still_writes_nothing_to_stdout`.
- **`scheduler run-once --json` put *two* JSON objects on stdout.** It
  writes its own result envelope and *then* returns `TasksFailed`, so the
  new error envelope landed after it — leaving `jq` with an unreadable
  stream. `Output` now latches once a document has reached stdout
  (`write_json`, `write_json_str`, `write_stdout_line`) and `fail` skips
  its envelope in that case. The run-once envelope is the one allowed to
  describe the outcome, so it is now self-describing: it gained
  `"ok": failed == 0` and, on failure, `error`/`code`/`exit`. Guarded by
  `json_scheduler_failure_is_one_self_describing_envelope`, which counts
  the JSON values on stdout rather than parsing the first one.

### Fixed (2026-09-24) — the scheduler recorded a config dump instead of the cause
- **`last_error` was ~380 characters of `[Config] applied provider=…,
  model=…` followed by the actual error.** Every `magent run` prints that
  preamble on stderr, and the state file keeps only `LAST_ERROR_MAX`
  characters — so the preamble filled the budget and the diagnosis was
  what got cut. `run_one_task` now builds the detail through
  `task_failure_detail`, which drops the `[Config]`/`[Backend]` status
  lines and labels what remains by stream. The transcript line
  (`one_line_failure`) strips that label back off, so it reads
  `"bad" FAILED: exit 1: error: could not load system prompt: …`.
  Guarded by `task_failure_detail_drops_the_config_preamble`,
  `task_failure_detail_labels_both_streams_and_skips_empty_ones` and the
  `last_error` assertions in the JSON-envelope test.

### Fixed (2026-09-24) — `scheduler run-once` no longer hides a failed task
- **A queue whose tasks all failed still exited 0 and printed
  `finished N task(s)`.** `run-once` is the unattended, CI-facing entry
  point — nobody reads its transcript — so a red queue looked identical to
  a green one from the shell's point of view. It now counts failures and
  returns `SchedulerError::TasksFailed { failed, total }`, which
  `main.rs` maps to exit **1** (a runtime failure, the same code `magent
  run` uses for a failed task; exit 2 stays reserved for rejected usage).
  The summary line becomes `finished N task(s), M FAILED` and the `--json`
  envelope gains a `failed` field. The daemon deliberately ignores the
  new variant and keeps ticking.
- **A failed task printed nothing at all.** The failure is now reported
  per task — `"bad" FAILED: exit 1: error: could not load system prompt`
  — with `one_line_failure` picking the child's own last non-empty line
  rather than the `[Config] applied …` preamble it arrives wrapped in.
- **A task failure was recorded as a *state file* problem.** `run_one_task`
  reused `SchedulerError::State { path: "<magent run> (exit N)" }` for a
  non-zero child, so the durable record blamed the state file — which was
  perfectly healthy — and never named the task. It now returns the new
  `TaskRun { name, exit, detail }`, and the state file records
  `scheduled task "bad" failed (exit 1): …`.
- **The `last_error` truncation dropped the cause.** It kept the *head* of
  a long error, which is the least informative part (a 300-char task
  message pushed the real failure off the end). It now keeps the *tail*,
  marked with a leading `…`, so the recorded error still ends in the
  reason the task failed.
- **A freshly written state file claimed `schema_version: 0`.** `Default`
  was derived, giving `u32::default()` — which contradicted the `1` serde
  writes for a missing field and that the daemon documents. It now has a
  hand-written `Default` carrying the current version.
  Guarded end-to-end by the new `cli/tests/scheduler.rs` (exit codes,
  summary text, state-file attribution, `--json` envelope), which induces
  failure deterministically offline via `--prompt-name <missing>`; plus
  unit tests for the tail truncation, the task-failure labelling and
  `one_line_failure`.

### Fixed (2026-09-24) — a long value no longer breaks the REPL's box frame
- **`magent run --repl --model <90-column name>` printed a banner row 103
  columns wide** in an otherwise 64-column box: the `Model:` row is built
  from user input, and `box_row` could only *pad* — a row already wider than
  the box got zero padding and pushed its right rail off the edge, so the
  frame visibly fell apart on the first screen of the session. The box
  renderer now clips to `REPL_BOX_WIDTH` display columns via the new
  `clip_display`, which is UTF-8- and double-width-aware (it never splits a
  character, and drops a wide char that would straddle the boundary) and
  marks a clip with `…`. This is the same class of bug the `display_width`
  table was added to fix, one input further out: the invariant that
  `repl_boxes_are_rectangular` asserts now holds for *any* value, not just
  the fixed labels. Only the display is clipped — the model the session
  actually uses is untouched.
  Guarded by `repl_boxes_survive_a_long_model_name`, plus
  `repl_banner_leaves_a_normal_model_name_alone` so the clip can't start
  eating ordinary names.
- **`stats` put its value column at three different offsets.** The rows were
  hand-spaced, which placed the value at column 23 (`Tasks run:`), 22
  (`Messages:`) and 24 (`Tool calls:`) — it read as a mistake rather than a
  table. All rows now go through `stats_row`, which pads the label to 17
  columns and right-aligns the value in 12, so every section shares one
  right edge.

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
