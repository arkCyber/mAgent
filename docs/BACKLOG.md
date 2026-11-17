# mAgent Backlog — Known Gaps (Post-Audit)

> Status as of the **2026-11-17 audit round 24**. This file tracks the
> feature/security gaps surfaced by the code audit that are **not yet resolved**,
> prioritised P0 → P2. Newly resolved entries are marked `[x]` with the date and
> keep their original finding for the record; see `CHANGELOG.md` for the round
> summary. Round 24 audited the *verifier* instead of the signing input and
> found a total break (a small-order Ed25519 key forged any credential or
> signed envelope), plus a seal-tag corruption that returned ciphertext as the
> credential; it also unified two divergent HRV formulas, closed the MQTT
> "reported delivery it never saw" gap, and fixed JSON-RPC error codes. The
> remaining round-24 findings change the signed wire format and are recorded
> as P1 owner decisions below. Round 14 found that round 13's option encoding was still malleable
> (absent vs empty `domain`) and that credentials carried the same unsigned-
> challenge hole round 13 fixed in presentations. Round 13 found that the
> presentation `challenge`/`domain` were
> outside the signed bytes — the anti-replay feature added in round 11 did
> nothing — and bound them, added `verify_with_challenge` and a CLI
> `--challenge`. Round 12 audited round 11's own code and closed a
> forged-credential hole in presentation verification, a CLI path that
> bypassed the same check, and a `finish()` that consumed its error — see
> below. Round 11 closed a
> verifiable-presentation gap (`vc present` signed presentations nothing could
> verify — `vc verify-presentation` and `VerifiablePresentation::verify` now
> exist), the emergency-hospital eviction
> policy, the boot-time stress-intervention cooldown, the sleep score's 90-point
> ceiling, `current_stress` ignoring heart rate, and several alert
> thresholds/rate limits — see below. Round 10 audited `magent-core`'s storage,
> wear-levelling, time and health modules: cooldowns that swallowed emergencies,
> a low-quality ECG raising a cardiac alert, compaction laundering corrupted
> values and deleting entries past a damaged header, static wear-levelling one
> sector behind its ledger, `i32::MIN` drift panics, and missing fsync in the
> CLI stores — see below. Round 9 audited rounds 4–8's own code and closed six
> defects those fixes introduced (SMTP retry duplicating a send, DND swallowing
> a cue and reporting delivery, claimed alert channels, after-the-fact line
> caps, lost request ids, `--repl -p -` no-op, `doctor` passing a broken config,
> corrupt mail reported as absent) — see below. Two 2026-11-10 rounds closed the
> CLI's *silent-success* error paths
> (a failed stdout write, unlistable prompt files, `--prompt -`, the `doctor`
> provider/model mismatch, padded config endpoints, daemon state-file
> failures) and the agent-side fabrications (an SSRF guard bypass, web tool
> failures recorded as successes, a leaked search snippet, a fenced tool call
> returned as the answer, rounded measurements, contradictory stress sensors,
> and impossible dates in `summary list`) — see below. The 2026-11-09 round
> closed the DBO1 identity-seed
> migration, the seal/open nonce-width invariant, the empty `search_emails`
> query, the email-MCP tool-error contract and `mark_read` verification, plus
> the CLI / agent-loop / Lua-sandbox / firmware findings recorded below.

## ✅ Done (2026-11-17) — round 24: the verifier itself, and readings that disagreed
- [x] **A small-order Ed25519 key forged any credential or envelope.** The
      lenient `VerifyingKey::verify` accepts the eight small-order points, so
      a `did:key` of the identity point plus `R = identity, S = 0` verified
      for an arbitrary issuer/subject/claims with no private key (reproduced:
      `verify()` → `Ok`). All three verifiers use `verify_strict` now.
- [x] **A corrupted `DBO` tag returned the ciphertext as the credential.** A
      one-bit flip in `D`, `O` or `B` defeated the round-23 shape test, so
      `XBO2:`/`DQO2:`/`DBC2:` blobs were handed back as the Wi-Fi password /
      API key. The test now discriminates on the long hex body.
- [x] **One moment had up to three different HRVs** (56.0 / 65.7 / 65.0 ms,
      measured) because two formulas both wrote `last_hrv`. Unified into
      `instant_hrv()`.
- [x] **Heart-rate alerts reported `threshold: 0.0`** for every band.
- [x] **A 126–180 mg/dL reading counted toward the "> 180 sustained"
      escalation**, so one genuinely high sample read as sustained
      hyperglycaemia.
- [x] **MQTT reported delivery it never observed** (10 KiB–1 MiB publishes
      dropped by the codec; PUBACK/SUBACK never awaited).
- [x] **The tool executor dropped buffered stdout between `rpc()` calls**,
      desynchronising the stream.
- [x] **JSON-RPC codes**: unknown tool → -32602, valid non-object JSON →
      -32600, rejected `initialize` → handshake failure.
- [x] **CLI web3**: `bind verify` ignored the signed expiry; `vc verify`
      skipped `validFrom`; `vc issue` accepted an unparseable `--valid-until`.

## 🟡 P1 — open, from round 24 (need an owner decision)

These are real defects that cannot be fixed mechanically: each changes the
signed wire format, which invalidates credentials and bindings already issued
by the committed build. They are ranked by severity.

- [ ] **`proof.type` is not covered by the signature.** `encode_proof_options`
      hashes `created`, `verificationMethod`, `purpose`, `challenge` and
      `domain` — but not the algorithm identifier. Flipping
      `Ed25519Signature2020` ↔ `2018` in the JSON still verifies, so one
      signature is valid for two documents that claim different algorithms.
      Note the existing `every_credential_proof_field_is_covered_by_the_signature`
      only catches the `DataIntegrityProof` mutation, and only because that
      type is unsupported — false assurance. Fixing it means adding `proofType`
      to the options encoding, which invalidates every credential issued so
      far; that is a migration decision (bump a scheme version and accept both
      on read, or accept revoking existing credentials).
- [ ] **`credentialSubject` accepts a duplicate claim key.** `RESERVED_CLAIM_KEYS`
      covers only `id`; inserting `"role":"user"` before the signed
      `"role":"admin"` parses last-wins, verifies, and re-serialises to the
      original signed text. A first-wins consumer reading the raw document sees
      a claim the issuer never signed. A `deny_unknown_fields`-style guard or a
      duplicate-key scan at parse time would close it; the latter needs
      `serde_json`'s raw-value path.
- [ ] **Unknown / W3C fields are silently dropped, and the W3C standard field
      names are not accepted.** No `#[serde(deny_unknown_fields)]`, and the
      structs have no `expirationDate`, `evidence`, `termsOfUse` or
      `proofValue`; a W3C-shaped document fails to deserialise
      (`missing field 'proof_value'`). Verification re-serialises the *parsed*
      struct, so an added `expirationDate` vanishes before hashing and a
      credential a W3C consumer calls expired is accepted here. This is a
      compatibility decision: interoperate with the W3C data model, or
      document explicitly that this is a private profile.
- [ ] **`credentialStatus` is parsed but never consulted**, so revocation is
      ignored and a revoked credential verifies forever (the module docs
      advertise revocation support). Enforcing it needs a status-list fetch,
      which is a transport/session question for the CLI.
- [ ] **`purpose` is not checked on the `options-v1` path.** It is inside the
      signed bytes, so it cannot be *changed* after signing — but a credential
      whose proof declares `Authentication` is accepted as an assertion.
      Adding the check would reject documents already signed this way
      (including any issued with the default `AssertionMethod`? no — those
      pass; only deliberately mis-purposed ones fail), so it is closer to
      safe, but it still needs a decision on which purposes are acceptable per
      operation.
- [ ] **Fractional-second truncation is asymmetric across the two bounds.**
      `parse_iso8601_instant` truncates fractions *down*, so `validUntil:
      …:04.999Z` expires up to 999 ms early (fail-closed, fine) while
      `validFrom: …:04.999Z` becomes valid up to 999 ms early (fail-open).
      Truncating the upper bound up instead is a two-line change but alters
      which credentials are accepted at the boundary.
- [ ] **`body-only` remains an accepted scheme for credentials.** Presentations
      refuse it outright; credentials accept it for legacy compatibility, which
      is a standing downgrade surface. Retiring it requires knowing whether any
      deployed build signed body-only credentials.

## ✅ Done (2026-11-16) — round 23: defects in round 22's own fixes
- [x] **A flipped separator byte bypassed the seal-tag guard** (`:`→`8`/`2`/`z`
      are one-bit alphanumeric changes; ~3 of 8 flips worked, handing a
      300-char blob back as the credential). The body shape is now required
      unconditionally. `a_flipped_separator_still_reads_as_a_sealed_blob`.
- [x] **The firmware's no-device-key fallback used `is_legacy()`**, so a
      `DBO1:` blob or a flipped-tag blob was sent as a bearer token (401 →
      misclassified as a timeout → 3 pointless retries). It uses
      `looks_like_seal_tag` now.
- [x] **Compaction's preflight disagreed with `write_entry_at`** about
      `WRITE_SIZE > 512`: it passed, erased the region, then failed every
      write. Same predicate both sides now.
      `compaction_refuses_an_oversized_program_unit_before_erasing`.
- [x] **`get_stats` reported partial counts as totals.** Added
      `scan_complete` to say when the figures are lower bounds.
      `stats_flag_a_partial_scan`.
- [x] **`AT+LLMCFG?` did an NVS migration write on the RT ingress thread.**
      Split into `load_llm_api_key_read_only` for real-time callers.

## ✅ Done (2026-11-16) — round 22: defects in round 21's own fixes
- [x] **Compaction erased the region before proving survivors fit**, so a
      padded survivor that did not fit lost every entry for an I/O error.
      Preflight capacity check added.
      `compaction_refuses_before_erasing_when_survivors_do_not_fit`.
- [x] **A torn slot read as end-of-store**, hiding every later entry and
      bricking `set`. `get` now reports `CorruptedData`; `get_stats` counts it
      in `corrupted_entries` instead of reporting a clean store.
      `a_torn_slot_is_reported_not_read_as_end_of_store`.
- [x] **Known single-byte charsets carrying UTF-8 were mojibaked**
      (`windows-1251`, `koi8-r`, `iso-8859-2/5/7`, `windows-1256`, `cp1251`).
      The single-byte allow-list now covers the family.
      `a_single_byte_charset_carrying_utf8_still_decodes`.
- [x] **`open_stored_wifi_pass` never re-sealed a DBO1 / plaintext entry**
      (unlike `open_dev_identity`), so a device that never ran
      `AT+WIFIPASSUPGRADE` kept an unauthenticated entry forever. It now
      migrates on the first successful open, best-effort.
- [x] **`glucose_slot`'s `_ => 4` wildcard** would have made every future
      `AlertType` share one slot and suppress the others. Now exhaustive.
- [x] **The LLM API key was stored in NVS in the clear** from all three
      provisioning paths (`AT+LLMCFG`, `MAGENT_LLM_API_KEY`, the agent install
      path). All reads/writes now go through `main::{load,save}_llm_api_key`,
      which seal with the device key via `wifi_pass_seal_v2::seal_secret` —
      the function written for this and previously uncalled. A legacy
      plaintext entry still loads and is re-sealed on first use. The firmware
      NVS read buffer went from 256 to 640 bytes because a sealed key is up to
      573 characters; a `magent-core` test pins the two together.
- [x] **Stale intra-doc links** (`breaks_cooldown` was deleted in round 21;
      private-item links in `security.rs` / `storage.rs`). CI gained a
      best-effort `cargo doc -D warnings` step — nothing had ever built the
      docs, which is why they rotted.

## ✅ Done (2026-11-15) — round 21c: defects in round 20's storage + seal fixes
- [x] **Compaction deleted a live tail entry.** `read_entry_header` returned
      `Erased` for "fewer than 35 bytes left", so a valid short entry at the
      region end was invisible to `get` and erased by `garbage_collect`.
      Reproduced: `get("y")` = `None` before GC, entry gone after. Test:
      `compaction_keeps_a_live_tail_entry`.
- [x] **The last slot of a padded store was unusable and reported a hardware
      fault.** Reproduced on a 160-byte / `WRITE_SIZE=16` region: 8 of 10
      entries fit, `free_space: 32`, `set` failed with
      `StorageReadFailed{ReadError}`. `set` now probes with the same decoder
      every read path uses. Test: `the_last_slot_of_a_padded_store_is_usable`.
- [x] **`WRITE_SIZE > 512` hung `set` forever** (unbounded pad loop on a
      bounded buffer). Reproduced; now a typed error.
      `a_program_unit_wider_than_the_buffer_is_refused_not_hung`.
- [x] **A header declaring more bytes than the region has left decoded as
      `Live`**, bricking the store with a permanent read fault. Now `Corrupt`.
      `an_entry_that_overruns_the_region_is_corruption_not_a_live_entry`.
- [x] **`--features web3` without `std` failed 6 tests** — they assert a
      successful encryption, which the round-20 nonce guard deliberately
      refuses without an entropy source. Gated on `std`, matching
      `BOOT_SALT_AVAILABLE`.
- [x] **`looks_like_seal_tag` was too strict and too loose**: it refused a
      legitimate password beginning `DBO2:` and let a corrupted *separator*
      through to the plaintext branch. Keyed on body shape now.
      **Owner decision 2026-11-15: an ambiguous value is treated as a
      password**, so `DBO2:mywifi` / `DBO2:zz` open while `DBO2:deadbeef`
      (hex, short) is truncated and `DBO2;<long hex>` is refused.
      `open_secret` had the same downgrade for API keys; fixed too.

## ✅ Done (2026-11-15) — round 21: defects in round 20's own fixes
Fourth round in a row where auditing the previous round's fixes was the most
productive step. Every entry was reproduced before being fixed.
- [x] **Round 20c's cooldown change caused an alert flood.** "A different
      condition breaks the cooldown" meant an alternating feed defeated the
      gate every sample: **60 alerts/minute**, history full in <50 s
      (measured). Replaced with one cooldown **per condition** (2 HR
      directions, 4 glucose conditions, 8 ECG kinds). Measured after:
      **2/minute**. Tests: `oscillating_conditions_do_not_alert_on_every_sample`,
      `oscillating_glucose_does_not_alert_on_every_sample`,
      `the_anti_flood_gate_is_what_suppresses_the_repeats` (sensitivity).
- [x] **Round 20's charset fix destroyed bodies with unknown labels.**
      `utf8mb4` / `unknown-8bit` (common from real MUAs) went through
      `mailparse::get_body()`'s lossy ASCII fallback and came back `������`.
      Decoding now dispatches on label class and rejects any result
      containing U+FFFD. Tests: `an_unknown_charset_label_falls_back_to_utf8`,
      `utf8_mislabelled_as_ascii_still_decodes`,
      `a_real_latin1_body_still_decodes_as_latin1`.
- [x] **`search_emails` / `mark_read` bypassed `map_imap_error`**, so a
      mid-call socket death never triggered the reconnect.
- [x] **`session_is_dead` treated a refused NOOP as death** → a full
      reconnect on every tool call. Only transport errors count now.
- [x] **`Output::finish` printed its diagnostic twice** (doctor + main).
- [x] **The executor killed a healthy child on a schema-level `ParseError`.**
      Added `McpError::StreamDesync` for the cases that really do lose line
      alignment; only that and `Io` restart the child.
- [x] **`ecg_alert_condition`'s doc comment was wrong** (`max_by_key` returns
      the *last* maximum, not the first). Corrected.

## ✅ Done (2026-11-15) — round 21b: the two remaining P2 health findings
- [x] **Heart-rate zone was always computed for a 30-year-old.**
      `HeartRateData::zone_for_profile()` / `HeartRateZone::from_hr_and_max()`
      added; `SportsCoach::set_profile()`. Without a profile the old
      behaviour (and its documented caveat) is unchanged.
      `the_danger_warning_uses_the_users_max_heart_rate`.
- [x] **Skin temperature was judged against core-body thresholds.**
      `TemperatureData` carries a `TemperatureSite` (`Core` default, `Skin`)
      with its own normal band; `has_fever()` is `false` for skin by
      construction, and `is_elevated()` is the signal the coach now uses.
      `temperature_sites_have_their_own_normal_bands`,
      `a_warm_skin_reading_warns_on_its_own_scale`.

## ✅ Done (2026-11-15) — round 20c: findings from a second audit of round 19's tree
Each entry below was reproduced against the pre-fix code before being fixed;
the regression test names are given so the check can be re-run.
- [x] **Undeclared-charset email bodies were Latin-1 decoded** (`温度` →
      `æ¸©åº¦`). `mailparse` defaults `ctype.charset` to `"us-ascii"`.
      Tests: `extract_text_body_decodes_utf8_when_no_charset_is_declared`,
      `..._in_a_multipart_alternative`, `undeclared_non_utf8_bytes_fall_back_to_latin1`.
- [x] **A corrupt `text/plain` part shadowed a decodable sibling.**
      `a_corrupt_part_does_not_shadow_a_healthy_sibling`.
- [x] **`mark_read` answered `marked_read` for a rejected STORE.** Flags are
      read back; a missing `\Seen` is an error.
- [x] **IMAP reconnected on every error, not just transport ones** — and
      leaked a socket per retry (`async-imap::Session` has no `Drop`).
      `only_transport_failures_are_retryable`; discarded sessions send
      `LOGOUT`.
- [x] **`email-mcp` answered -32000 for a missing `tools/call` name** (should
      be -32602). `a_tools_call_without_a_name_is_invalid_params`.
- [x] **`mqtt-mcp` answered a malformed notification.**
      `a_malformed_notification_still_gets_no_reply`.
- [x] **One non-UTF-8 request byte terminated either MCP server** with no
      reply. Now -32700 + keep serving.
- [x] **The executor forwarded malformed arguments as `null`.**
      `malformed_argument_json_is_rejected_before_the_server_sees_it`.
- [x] **A desynchronised executor stream kept the child** (every later call
      parsed fragments of the oversized line). Restarted now.
- [x] **A restarted exercise session suppressed the Danger warning for
      531 s.** `a_restarted_session_rearms_the_danger_warning`.
- [x] **A Critical alert in the opposite direction was swallowed by the
      cooldown** (205 → 35 BPM; 300 → 40 mg/dL; bradycardia → tachycardia).
      `a_critical_alert_in_the_opposite_direction_breaks_the_cooldown`,
      `a_bradycardia_crisis_breaks_a_tachycardia_cooldown`. NOTE: the
      *emergency contact* has a separate 30-minute anti-spam window; a second
      crisis inside it raises and records an alert but does not page again —
      see the P2 entry below.
- [x] **A fenced tool call behind a sentence became the final answer.**
      `parse_result_does_not_return_a_fenced_tool_call_after_prose`,
      `strip_code_fence_finds_a_block_after_prose`.
- [x] **`AlertOutcome::voice` counted the generic follow-up cue.**
      `a_dropped_alert_cue_is_not_reported_as_voiced_by_its_follow_up`;
      `follow_up` is now its own field.
- [x] **ECG alerts reported a threshold the reading never crossed.**
      `an_ecg_alert_reports_the_limit_it_actually_crossed`.
- [x] **A fresh simulator reported two HRVs for one instant** (55 vs 65 ms).
      `both_stress_readings_report_the_same_hrv`.
- [x] **A low-quality ECG rejection was unobservable.**
      `ecg_samples_rejected_low_quality()` counter.
- [x] **`flash_write` refused an explicitly empty `data=`.**
- [x] **A lost stdout payload could exit 0 when stderr was broken too.**
      `finish_reports_a_stdout_failure_even_when_stderr_is_broken`.
- [x] **A prompt named `*.signed` disappeared from `set-prompt list`.**
      `a_prompt_named_signed_is_not_mistaken_for_an_envelope`.
- [x] **`run` exited 2 with no config location** (no `$HOME`, no
      `$MAGENT_CONFIG_FILE`); `version` still worked. Missing location now
      resolves to defaults like a missing file.
- [x] **CI never compiled the opt-in modules** (`security`, `wifi_pass_seal*`,
      `verifiable_credentials`). The `host` job now lints + tests
      `std,web3,verifiable_credentials,boot-key` and runs a feature matrix.
- [x] **Round 21c follow-up: the header decoder is now single-source.**
      `set`'s free-slot probe and `read_entry_header` both go through
      `parse_kv_header_within(buf, avail)`, so "can I write here" and "can I
      read this" are the same computation. Before this they disagreed at the
      tail, which is how the data-loss defect above was reachable at all.
- [ ] **P2, open: a second crisis inside the 30-minute emergency window does
      not re-page a contact.** `trigger_emergency_protocol` is gated on
      `emergency_cooldown_ms` (30 min) so the contact is not spammed. A
      hypoglycaemia 60 s after a hyperglycaemia alert is now *raised and
      recorded* (round 20c) but does not notify. Needs an owner decision:
      re-page on a change of condition, or keep the hard window.

## ✅ Done (2026-11-15) — round 20: what the covered fields are actually used for
- [x] **A claim named `id` produced a credential that could not be read back.**
      `CredentialSubject` flattens `claims` over a struct with a real `id`
      field; serde emits a colliding key twice. `vc issue --claim id=…` wrote
      `{"id":"<subject>","id":"<claim>"}` — rejected by this crate's own
      `Deserialize` as `duplicate field 'id'`, and read last-wins as the
      *claim* by other parsers. `is_reserved_claim_key` / `try_with_claim`
      refuse it; `with_claim` drops it. Tests:
      `a_claim_named_id_cannot_shadow_the_subject_id`,
      `try_with_claim_reports_a_reserved_key` (verified to fail on the old
      code, whose wire form contains two `"id"` keys).
- [x] **`is_expired` compared strings, so a timezone offset could make an
      expired credential valid.** `parse_iso8601_instant` applies the offset
      and truncates fractional seconds; unparseable bounds fail closed.
- [x] **`validFrom` was never checked.** `is_not_yet_valid` is now part of
      `verify`.
- [x] **A body-only proof's `purpose` was unauthenticated.** Only
      `AssertionMethod` is accepted on that path.
- [x] **`encode_for_signing` / `signing_input` return `Result`** instead of
      serialising the body with `unwrap_or_default()`. A failed serialisation
      would otherwise have produced a signature over empty bytes that the
      verifier reproduces.
- [x] `encode_for_signing`'s doc no longer presents it as the signing API —
      on a proof-less document it returns proof-less bytes, and a proof built
      with `CredentialProof::new` defaults to `options-v1`, so signing those
      bytes can never verify. `signing_input` is the signing path.

## ✅ Done (2026-11-15) — round 19: exhaustive field-coverage check replaces attack-by-attack auditing
- [x] **Every mutable field of both proof types and both body types is
      enumerated and mutated**; verification must fail for each. Adding a
      field without covering it now fails a test instead of waiting for
      someone to think of the attack. Verified by removing the challenge from
      the encoding.
- [x] **Removing a covered domain** (the strip-a-binding direction) is tested
      separately, since the add-mutations cannot express it.
- [x] **`PresentationProof::signing_scheme` doc** no longer claims an absent
      value is accepted (round 18 made it mandatory).

## ✅ Done (2026-11-15) — round 18: round 16 only blocked an absent scheme
- [x] **An explicit `"signingScheme": "body-only"` still let a presentation be
      replayed** — round 16 checked `is_none()`, so setting the field took the
      body-only hash path and left `challenge` unsigned. `verify_proof` now
      requires `options-v1` for every presentation proof.

## ✅ Done (2026-11-15) — round 17: signing/verification scheme defaults diverged
- [x] **`signing_input` and verification resolved `signingScheme: None`
      differently** (`ProofOptions` vs `BodyOnly`), so a credential signed
      through the public helper with the field cleared could not verify.
      `signing_input` now shares the verification path, and a property-style
      test pins all three scheme values.
- [x] **`try_heapless_perfect_roundtrip_under_limit` asserted a false
      invariant** (128 characters ≠ ≤256 bytes); it was passing by luck.

## ✅ Done (2026-11-15) — round 16: round 15's fallback reopened the replay hole
- [x] **A presentation could be downgraded to `body-only`** — `signingScheme`
      is attacker-controlled JSON, and a body-only proof does not cover
      `challenge`, so a replayer could blank the field, swap the challenge and
      pass `verify_with_challenge`. Presentations no longer fall back; an
      absent scheme is `options-v1`.
- [x] **A body-only credential could carry an unsigned challenge/domain** and
      have it reported as bound. Rejected now.

## ✅ Done (2026-11-15) — round 15: round 14 broke every existing credential
- [x] **Credentials issued before round 14 stopped verifying** — the signing
      input changed from body-only to body+options, so every credential the
      committed build had produced failed with the same error as a forgery.
      `signingScheme` (`options-v1` / `body-only`, absent ⇒ body-only) makes
      verification apply the rule the signature was computed under.
- [x] **Two copies of the proof-option encoder** (credentials and
      presentations) could drift; merged into `encode_proof_options`.

## ✅ Done (2026-11-14) — round 14: malleability, and credentials got round 13's fix
- [x] **`domain: None` and `domain: Some("")` hashed identically** — an
      attacker could add or remove an empty domain without invalidating the
      signature. Now presence-tagged.
- [x] **`VerifiableCredential` had round 13's gap**: its `encode_for_signing`
      still dropped the whole proof, so `CredentialProof.challenge`/`.domain`
      were unsigned. Fixed with the same length- and presence-tagged options,
      plus `signing_input`.
- [x] Signing paths (CLI `vc issue`, tests) now use sign-then-attach, and the
      expired-credential test no longer depends on two timestamps landing in
      the same second.

## ✅ Done (2026-11-14) — round 13: the presentation challenge was never signed
- [x] **`challenge` / `domain` were outside the signed bytes.**
      `encode_for_signing` dropped the whole proof, so a rewritten challenge
      still verified — replay protection did nothing. Proof options are now
      length-prefixed into the signing input (W3C Data Integrity style).
- [x] **Signing was made symmetric** via `VerifiablePresentation::signing_input`
      (build proof → hash body+options → sign → attach); used by the CLI and
      tests.
- [x] **`vc verify-presentation --challenge <STR>`** and
      `VerifiablePresentation::verify_with_challenge` let a verifier require
      its own nonce instead of only checking internal consistency.

## ✅ Done (2026-11-14) — round 12: defects in round 11's own code
- [x] **`VerifiablePresentation::verify` accepted a forged inner credential.**
      It checked only the outer holder proof. Each contained credential now
      goes through `VerifiableCredential::verify` (structure, expiry, own
      signature).
- [x] **The CLI `vc verify-presentation` bypassed that check** by calling
      `validate()` + `verify_proof()` instead of `verify()`.
- [x] **`Output::finish` took its stored error**, so the second of its two
      calls (doctor then `main`) returned `Ok` and a lost payload could exit 0.
- [x] Bounded line reader: pinned that a multi-byte character split across a
      buffer refill survives intact.

## ✅ Done (2026-11-14) — round 11: presentations, alerts, sleep and HR scoring
- [x] **`vc present` signed presentations nothing could verify.** Added
      `VerifiablePresentation::{encode_for_signing, verify_proof, verify}` and
      `magent web3 vc verify-presentation <FILE>`. End-to-end: signed → 0,
      tampered holder → 2.
- [x] **Signing and verification used different encoders.** Both go through
      `encode_for_signing` now.
- [x] **Web3 output paths still used `unwrap_or_default()`** (empty document,
      exit 0). They report `Serialise`.
- [x] **A farther hospital evicted a nearer one** when the list was full,
      moving the emergency SMS target 92 km away.
- [x] **The first stress intervention after boot was suppressed** (plain `u32`
      cooldown read "offered at boot"). `Option<u32>` now.
- [x] **A meditation completion was attributed to the newest sample**, not the
      event that offered it.
- [x] **The sleep score's positive weights summed to 90**, so "N/100" could
      never reach 100.
- [x] **`current_stress` ignored HR** (190 BPM + HRV 85 read as "Relaxed").
- [x] **`duration_minutes` reported 24 h for a zero-length night** and
      underflowed on an out-of-range field.
- [x] **`simulate_heart_rate` overflowed `u32`** past ~50 simulated days.
- [x] **ECG alerts reported `threshold: 100.0` for every anomaly** (a 35 BPM
      bradycardia claimed 100).
- [x] **The "sustained high HR" streak never fired above Cardio**
      (`zone() == Cardio` matched only 151 BPM).
- [x] **The danger-zone warning had no rate limit** (filled the message buffer).


## ✅ Done (2026-11-13) — round 10: health safety, storage durability, wear accounting
- [x] **A Critical glucose/ECG reading was swallowed by a cooldown**, skipping
      the emergency protocol. Cooldowns collapse repeats only; an escalation
      (`escalates`) breaks through.
- [x] **A low-quality ECG raised a cardiac emergency** — `quality` was never
      consulted. Below 70 % no alert is raised.
- [x] **Heart-rate alerts had no cooldown** (60 alerts/minute). Same rule as
      the other channels.
- [x] **A full alert history dropped the newest alert** (unacknowledgeable).
      Evicts the oldest now.
- [x] **`Elevated` glucose counted toward the "sustained >180" escalation.**
- [x] **Compaction laundered corrupted values** (no CRC check before
      rewriting). Refuses with `CorruptedData`.
- [x] **A corrupt header mid-region read as end-of-store, and compaction
      deleted everything past it.** `decode_kv_header` distinguishes
      erased/corrupt; compaction refuses.
- [x] **Static wear-levelling was one sector behind its ledger** (`[11,10,10,9]`
      physical vs `[10,10,10,10]` recorded). Same formula both sides.
- [x] **`calculate_wear_level` used the reset rotation counter** (end-of-life
      read 0.25). Sums the per-sector ledger.
- [x] **`is_sector_worn` / `is_worn_out` disagreed at the limit** (`>=` vs `>`).
- [x] **`drift_ppm.abs()` panicked on `i32::MIN`** and accepted it in release.
- [x] **`unix_to_calendar`'s lower bound was 366 days early** (`0000-01-01`).
- [x] **`MonitoringManager` counters overflowed**; `add_health_check` appended
      instead of updating (stale Degraded forever).
- [x] **Prompt store and config file lacked fsync** around their renames.


## ✅ Done (2026-11-11) — round 9: defects in rounds 4–8's own fixes
- [x] **A failed SMTP send could be delivered twice.** The single retry was
      not safe for a non-idempotent operation: a drop after `DATA` leaves the
      message queued server-side while `send` errors, so the retry sent a
      duplicate. No retry; the transport is dropped for the next call.
- [x] **Do-not-disturb could swallow a cue and report it delivered.** Voice
      routing substitutes a notification for `priority < 8`, but the
      notification layer drops `priority < 7`. `DroppedInDnd` names the gap;
      `push_notification` reports whether an entry was really stored.
- [x] **`send_health_alert` guessed per channel from queue/history lengths.**
      An eviction leaves the length unchanged, so a suppression could read
      as delivered. The manager returns `AlertOutcome { voice, notification }`.
- [x] **MCP line caps were applied after the allocation.** All three servers
      and the executor now cap *during* the read (`fill_buf`/`consume`),
      drain an over-long line without storing it, bound the discard budget,
      and stay line-aligned so the next request is still served.
- [x] **A bad-field JSON-RPC request lost its id.** Valid JSON without
      `method` came back as `{"id":null,"error":{"code":-32700}}`; it is now
      `-32600` with the recovered id (round 8 fixed only `tools/call`).
- [x] **`--repl --prompt -` was a silent no-op** (both drain stdin); rejected
      at parse time with a message naming the conflict.
- [x] **`doctor` passed a config `run` rejects.** A corrupt config gave
      `ok: true` / exit 0 from built-in defaults while `run` exited 2. Doctor
      now fails fast with `config_error` / exit 2.
- [x] **A corrupt mail body read as "no text/plain part".**
      `TextBody::{Found, Undecodable, Absent}` distinguishes the three cases.
- [x] **`Output::flush` attributed a stderr failure to stdout**, which would
      have produced a false "the data was not delivered" exit.


## ✅ Done (2026-11-11) — tool errors are sentences, not enum dumps
- [x] **`SimulatorExecutor` rendered errors with `Debug`**, so the model was
      fed `ToolFailed { tool: …, message: … }`. Now `Display`. Became
      reachable when round 5 made web-tool failures real `Err`s.
- [x] Trailing-comma repair: nested/repeated cases pinned by tests; cut
      ranges verified disjoint.
- [x] `run_one_tick`: the absorbed `TasksFailed` arm no longer binds and
      discards a value.

## ✅ Done (2026-11-11) — defects in the previous rounds' own fixes
- [x] **`strip_trailing_commas` corrupted UTF-8 and merged array elements**
      (`北京` → mojibake; `["a","b"]` → `["a""b"]`). Byte-range rewrite.
- [x] **Anthropic invoke body had an off-by-one** — sliced from the tag's
      `>` instead of after it. Invisible until zero-argument calls were
      enabled; `<invoke>prose</invoke>` is also no longer a "call" now.
- [x] **`days_to_ymd` wrapped on absurd input** (`u64::MAX` → 1969); clamped,
      with the bound chosen to avoid overflow in the `+ 719_468` step.
- [x] **Two recovery mappings disagreed on `ToolFailed`** (`RetryBackoff`
      vs `Abort`). Both `Skip`; a test pins the pairing.
- [x] **`scheduler daemon --at` exited 0 on a failed task** while
      `run-once` exited 1. One-shot ticks now propagate; repeating ones
      still absorb.

## ✅ Done (2026-11-10) — MCP / mail / MQTT / Lua: failures the user never saw
- [x] **MCP `isError` was ignored** — a tool-level failure (rejected
      `send_email`) came back as a successful call. Now `McpError::ToolError`.
- [x] **`ble_send` without `data` ran `list_inbox`** — an unrequested side
      effect reported as the caller's action. Both the missing field and
      malformed JSON are errors now.
- [x] **MQTT publish topics are validated** (no wildcards, 65535-byte cap);
      `publish_bytes` had performed no checks at all.
- [x] **MQTT JSON-RPC keeps the request id** on a bad-parameter `tools/call`
      (`-32602`, original id) instead of `{"id":null,"error":{-32700}}`.
- [x] **A dead IMAP session reconnects** (one shared helper replaces five
      copies of the lock-and-call pattern).
- [x] **`get_email` uses `BODY.PEEK[]`** — it no longer marks mail as read.
- [x] **Mail bodies honour `charset=`** (`get_body()`, not
      `get_body_raw()`), and HTML-only mail says the text part is absent
      instead of returning `""`.
- [x] **Line reads are bounded** in all three MCP processes (1 MiB requests,
      4 MiB executor responses).
- [x] **`string.match` nesting is capped** (32 levels) — a deep pattern used
      to abort the Lua VM. The cap had been defeated by a shadowed counter;
      regression test proves both directions.

## ✅ Done (2026-11-10) — agent correctness: the model is not told things that are untrue
- [x] **SSRF guard bypass via numeric hosts.** `http://2130706433/` and
      `http://0x7f000001/` passed the lexical check (not an IP literal *as
      written*), `Url::parse` canonicalised them to `127.0.0.1`, and the
      guard's "already an IP literal" shortcut returned `Ok` — so they
      reached loopback, and `0xa9fea9fe` / `2852039166` reached the
      `169.254.169.254` metadata endpoint. The classifier runs on the
      normalised host now; regression test covers six spellings without
      opening a socket.
- [x] **Web tool failures were recorded as successes.** `Ok("error: …")`
      → real `Err`, carried by a new `AgentError::ToolFailed`.
- [x] **Search results carried their neighbour's snippet** (per-result
      segmentation instead of a cross-boundary `(?s).*?`).
- [x] **JSON tool arguments with a non-string member produced garbage URLs**
      (hand-rolled scanner replaced with `serde_json`; trailing commas
      tolerated).
- [x] **Bare `&` in HTML was rewritten to `&;`** (`AT&T` → `AT&T;`).
- [x] **A fenced tool call was returned as the final answer** (embedded
      `result` envelope extracted first; a fenced call yields no result).
- [x] **Fractional `result` values were rounded to 1 decimal** (0.25 → "0.2").
- [x] **Zero-argument Anthropic tool calls were unrecognised**, leaving the
      XML to be reported as the answer.
- [x] **Stress sensors contradicted each other** (`check_stress_level` vs
      `hrv`); both read the stored level.
- [x] **`flash_write` / `ble_send` accepted a missing payload** as a
      zero-byte success (host simulator + ESP32 handler).
- [x] **Voice/meditation cues claimed delivery that did not happen**
      (`QueueOutcome`; full queue and DND routing are reported truthfully).
- [x] **`summary list` / `show` printed impossible dates** for every
      leap-year February (`2028-01-32`); replaced with Hinnant's
      `civil_from_days`, verified for every day 1970–2100.

## ✅ Done (2026-11-10) — CLI: errors that used to look like success
- [x] **Failed stdout writes now exit non-zero.** `Output` records the first
      stdout error (`write_human` / `write_stdout_line` / `write_json_str` /
      `stdout_fmt_line` / `final_answer` / `write_json` / `fail`) and `main`
      calls `Output::finish()` for every dispatch arm. Before: `magent config
      export > /full/disk` exited 0 with the record never delivered.
      Regression test drives the binary under `RLIMIT_FSIZE=0`, which is the
      only portable way to make a real payload write fail deterministically.
- [x] **`set-prompt list` reports every file it skipped** (unreadable,
      unparseable, newer schema) on stderr and in a `skipped` array under
      `--json`. `*.signed.json` envelopes remain filtered silently.
- [x] **`--prompt -` reads stdin** for `run` and `set-prompt set`, through a
      1 MiB-bounded, UTF-8-checked reader.
- [x] **`doctor` resolves the model of the configured provider** (`OLLAMA_MODEL`
      no longer leaks into a `deepseek` check; `DEEPSEEK_MODEL` added) and its
      per-check `ok` now matches how the top-level verdict treats `SKIP`.
- [x] **Config endpoint URL/model are trimmed** consistently with `doctor`.
- [x] **Scheduler bookkeeping failures are fatal to the run** (exit 1,
      `scheduler_state_error`) instead of a swallowed `tick error`; task
      failures are still absorbed. `web3_blockchain` render helpers no longer
      `unwrap_or_default()` into an empty payload.
- [x] **Prompt-store temp file is created 0600** rather than narrowed after a
      0644 write.

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

## ✅ Done (2026-11-15) — round 20b: the three latent P1 traps are closed
- [x] **AES-GCM nonce reuse across reboots on a `web3` no-`std` build.**
      The nonce is `boot_salt || counter || len`, the key is a compile-time
      constant, and the counter restarts at zero each boot — so on a target
      with no entropy source the first message of every boot reused
      `(key, nonce)` for any two same-length messages. GCM keystream reuse
      leaks `P1 ⊕ P2` and allows tag forgery, and the old code returned `Ok`
      on that path, so the only symptom was a subtle ciphertext correlation.
      `BOOT_SALT_AVAILABLE` now reports whether a cross-boot salt exists
      (`cfg!(feature = "std")`) and `encrypt_aes_gcm` returns `CryptoError`
      when it does not, instead of producing a ciphertext whose nonce will
      repeat. **This does not make the `no_std` path usable** — a device path
      still needs a per-device key (eFuse/SoftDevice) and a persistent
      monotonic counter. It makes the trap loud rather than silent. Tests:
      `no_entropy_source_must_refuse_rather_than_reuse_a_nonce`,
      `boot_salt_availability_matches_the_salt_source`.
- [x] **`KvStore` required `WRITE_SIZE`-aligned entries but wrote arbitrary
      lengths.** `FlashStorage::write` rejects a length that is not a multiple
      of `F::WRITE_SIZE`, and a KV entry is `3 + key_len + value_len + 2`
      bytes, so **every** `set` failed on a part whose program unit is 4 (or
      the ESP-IDF encrypted view's 16) — the mock's `WRITE_SIZE = 1` made the
      whole test suite blind to it. Entries are now padded to whole program
      units (`slot_size`) on write, and every scan/compaction/stats path
      advances by the padded slot, so the pad is never read back as a header.
      The wire format is unchanged, so entries written by the old code still
      read correctly. Test: `kv_store_works_on_a_four_byte_program_unit`
      (write, second entry, delete-by-compaction, stats, all on `WRITE_SIZE=4`).
- [x] **`wifi_pass_seal_v2` downgraded on a corrupted prefix.** Only `DBO2:`
      ran the MAC check; every other prefix fell into DBO1's
      `LegacyPlaintext` branch and `open_stored_wifi_pass` returned that
      string verbatim as the Wi-Fi password. One flipped byte in NVS
      (`DBO2:` → `DBO7:`, or the `2` erased to `0xFF`) therefore turned an
      integrity-checked entry into an unauthenticated one. `looks_like_seal_tag`
      now refuses any `DBO<digits>:`-shaped value that is not a supported
      version, while a genuine legacy password (which cannot have that shape)
      still migrates. Tests: `an_unknown_version_tag_is_refused_not_treated_as_plaintext`,
      `seal_tag_shape_is_recognised_without_matching_plaintext`.

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

### 🟡 P2 — 2026-11-13 round 10 findings that need an owner decision

- [x] **Heart-rate zone is always computed for a 30-year-old.** *Fixed in
      round 21b:* `HeartRateData::zone_for_profile()` and
      `HeartRateZone::from_hr_and_max()` added, `SportsCoach::set_profile()`
      wired through. `zone()` keeps its old, now explicitly documented
      behaviour for callers with no profile.
- [x] **`sports_coach`'s "sustained high HR" check can never fire above
      Cardio.** *Fixed in round 11:* the guard is a lower bound
      (`zone() >= Cardio && hr > 150`), and `HeartRateZone` is `Ord`.
- [x] **Skin temperature is judged against core-body thresholds.** *Fixed in
      round 21b:* `TemperatureSite` (`Core` / `Skin`) with per-site normal
      bands; `has_fever()` is `false` for skin by construction and
      `is_elevated()` is the signal the coach warns on. A skin sensor still
      cannot establish a core temperature — doing that needs a core source,
      which is a hardware question, not a threshold one.
- [x] **Sleep score weights sum to 90, not 100.** *Fixed in round 11:*
      rebalanced to 30+30+30+10; a perfect night scores 100.
- [x] **`adjust_goal_for_conditions` is not idempotent.** *Fixed in round
      21d:* recomputes from a retained `intensity_baseline` and rebuilds the
      reason list each pass, so repeated calls with the same conditions are
      stable and a cleared condition restores the baseline.
      `adjusting_for_the_same_conditions_is_idempotent`,
      `a_cleared_condition_restores_the_baseline_intensity`.
- [ ] **`unix_to_calendar`'s upper bound is 9999-12-31 but the formatter
      prints a 4-digit year**, which is fine; the lower bound was off by 366
      days and is fixed (round 10). The remaining question is whether this
      API should also reject year 0000 for *arithmetic* reasons — currently
      handled by the corrected bound.

### 🟡 P2 — 2026-11-09 round 2 findings that need an owner decision

- [x] **C61's reserved-pin guard was empty.** *Fixed in round 23:* rather
      than encoding a board table this repo does not carry, the C61 path now
      asks ESP-IDF at runtime — `gpio_get_io_config`'s `fun_sel` is
      `PIN_FUNC_GPIO` (= 1, confirmed for `esp32c61` in
      `soc/.../io_mux_reg.h`) only for pins no peripheral owns, and
      `oe_ctrl_by_periph` catches the pins a peripheral drives. That answers
      exactly the question the guard is asking and stays correct if the pin
      map changes. A static fallback covers the window where a peripheral is
      between initialisation steps: the C61 MSPI pins (**14–17, 19–21**, from
      `components/soc/esp32c61/include/soc/spi_pins.h`) and UART0 (10, 11).
      Driving any of them detaches the flash and crashes the next fetch.
- [x] **Ingress framing fixed 2026-11-09 (round 3).** `UartAdapter::poll`
      now reassembles `\r`/`\n`-terminated lines and returns exactly one frame
      per call (bounded at 1 KiB), so long commands are not split and
      back-to-back commands are not merged.
- [ ] **`task_handle` / `reply_outbox` are single slots.** `main.rs` defines
      each as `Arc<Mutex<Option<String>>>`. The ingress thread overwrites the
      pending command slot; the agent polls it only once every ~5 s, so two
      AT/agent commands inside that window silently drop the first. The reply
      slot has the same shape: an agent `RESULT[…]` written while the ingress
      is blocked in `AT+HTTPGET` (up to 12 s) is overwritten by the AT reply.
      Fix is a bounded per-link queue (and `+CMDER:6` when full), which changes
      the ingress/agent handshake — an owner decision, not a mechanical fix.
- [ ] **The nRF52 "watchdog" is a software counter.** `firmware/nrf52-app/src/
      watchdog.rs` implements `feed`/`check_timeout` over struct fields; no
      hardware WDT is instantiated (no `embassy_nrf::wdt` call anywhere in the
      crate). `main.rs` creates it and logs "Watchdog: 60s timeout", so the
      logs claim protection that a hung executor would not get (`panic_reset`
      covers panics only). Wiring the hardware WDT needs a chosen timeout and
      reset semantics validated on the board; deleting the misleading log is
      the fallback if the scaffold stays.
- [ ] **OTA has no host allowlist.** `ota::validate_ota_url` still checks only
      the `https://` prefix (the redirect-downgrade bypass was closed
      2026-11-09 by `FollowNone`). A compromised operator URL can still fetch
      from any host. Production images should carry an explicit allowlist
      (`compile-time`/`cfg`), as the module docstring already says.

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
- [x] **A long value no longer breaks the box frame** (fixed 2026-09-24).
      `box_row` could only pad, so a row already wider than
      `REPL_BOX_WIDTH` got zero padding and pushed its right rail off the
      edge. The one row built from user input is the banner's `Model:`
      (`--model <name>`), and a 90-column name printed that row 103 columns
      wide — the frame fell apart on the session's first screen. Rows are
      now clipped by the new `clip_display`, which measures in display
      columns (CJK/emoji-safe, never splits a char, drops a wide char that
      would straddle the edge) and marks a clip with `…`; only the display
      is clipped, never the value actually used. Covered by
      `repl_boxes_survive_a_long_model_name` and
      `repl_banner_leaves_a_normal_model_name_alone`.
- [x] **`stats` value column aligned** (fixed 2026-09-24). Hand-spaced rows
      put the value at column 23 / 22 / 24 across the three sections. All
      rows now go through `stats_row` (label padded to 17, value
      right-aligned in 12), so every section shares one right edge. The
      `repl_boxes_are_rectangular` test continues to hold.
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

- [x] **Audited 2026-09-24, no change needed: `--max-iterations 0`.** It
      briefly looked as if it printed the generic top-level help instead of
      a usage error, but that was a `2>&1` merge artifact in the probe. The
      binary prints `error: invalid value '0' for flag 'max-iterations'
      (expected a positive integer (>= 1))` on **stderr**, prints nothing on
      stdout, and exits 2 — as `--max-tool-calls 0` does. `--max-messages 0`
      and `--tool-max-chars 0` keep the documented `0 = off` sentinel
      (`parse_usize_nonzero` vs `parse_usize`). Covered already by
      `rejects_zero_max_iterations` / `rejects_zero_max_tool_calls`, so no
      new test.

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
- [x] **`config_store` is wired into the ESP32 firmware (2026-11-08).**
      `firmware/esp32-app/src/config_nvs.rs` implements
      `config_store::StorageBackend` over ESP-IDF NVS (the `magent_cfg`
      namespace; keys are the store's names), and `run_agent_loop` now boots
      from `config_nvs::load_or_default()` instead of a hard-coded builder.
      Operator surface: `AT+AGENTCFG?` (query), `AT+AGENTCFG=<name>,<iters>,
      <mem>,<watchdog>,<ble>,<skills>,<max_skills>,<debug>` (validate + persist;
      applies next boot), `AT+AGENTCFG=RESET` (delete). A corrupt entry is
      reported by `AT+AGENTCFG?` and falls back to board defaults at boot, so
      it cannot brick the agent thread.
- [ ] **nRF52 still uses the compile-time `AgentConfig` builder.** The nRF
      firmware (`firmware/nrf52-app/src/main.rs`) has its own flash stack
      (`embedded-storage`), so it would need a second `StorageBackend`
      implementation. The ESP32 path is the reference; this is the follow-up.
- [ ] **The CLI config layer intentionally remains separate.** `magent config`
      persists `ConfigRecord` (provider/sampling/runner/compression/io) as JSON
      with its own atomic write; the core `AgentConfig` the CLI does not use
      (the host runner has `RunnerConfig`). Sharing `config_store` would mean a
      second schema in the CLI file, not a reuse win. Revisit only if a CLI
      command needs to read/write a device `AgentConfig`.
- [ ] **Email MCP on ESP32** — `--email-tools` is host-only; no SMTP client on
      firmware for device-originated alerts.
- [ ] **Zigbee / 802.15.4** — nRF52840 advertises the `thread` feature; Zigbee and
      a real 802.15.4 stack are still on the roadmap.
- [x] **Lua host is now compile-time S3-only (2026-11-08).** The module was
      gated on `lua` alone, so a C61 build that enabled it would have started
      the Lua host too. `main.rs` now emits `compile_error!` for
      `lua && !board-s3`; the C61 has neither the internal DRAM nor the
      dual-core scheduling the host assumes. No Lua App runtime on the C61, by
      construction rather than by comment.
- [ ] **S3 Lua VM crashes Core0 on hardware (2026-09-23).** The `lua` feature is
      OFF by default for a *runtime* reason, not a build one: the build is
      verified end-to-end (`cargo check/build --features board-s3,wifi,uart,lua`
      is clean — see `docs/LUA_SCRIPTING_S3.md`), but booting it crashes Core0
      even after the I2C pin fix (`firmware/esp32-app/build-s3.sh`). Until
      this is root-caused the on-device Lua app and `Esp32Hardware` drivers are
      unvalidated. The doc/build-hint drift that described this as a build
      failure was corrected 2026-09-23.
      **Diagnostics added 2026-11-08:** `lua_task::log_headroom` logs
      `internal_free` / `psram_free` / `stack_min_free` at thread start, before
      and after `PiccoloVm` construction, and after `main.lua` boot, so the
      next hardware boot log identifies the last completed phase and whether
      internal DRAM or the 32 KiB stack was the constraint.
      **Two suspect paths removed (2026-11-08):** (a) the Lua host no longer
      spawns a *second* 24 KiB Core-0 `llm-worker`/mbedTLS client — it shares
      the main agent's single worker via `llm::shared_sender`, halving Core-0
      TLS stack/heap pressure; (b) the default `main.lua` is now passive at
      boot and only defines `selftest()`, so no flash write or ADC/I2C/GPIO
      reconfiguration happens in the Wi-Fi/TLS startup window. Neither is
      proven to be the root cause without hardware; both are strictly less
      risky than the previous boot path. (c) `start_lua_task()` is also skipped
      in **safe mode**, so a Lua-induced boot fault no longer re-enters itself
      every boot and the AT/OTA recovery path stays reachable. (d) The Piccolo
      arena cap follows the PSRAM profile (512 KiB quad / 2 MiB N8R8) instead
      of a flat 8 MiB that exceeded the quad board's entire pool — an OOM abort
      is no longer reachable through ordinary scripts. (e) The Lua thread now
      prefers a 64 KiB stack (32 KiB fallback on ENOMEM) instead of sharing the
      agent's 32 KiB, since it nests the Piccolo VM and `agent.reason()`'s ReAct
      loop. The crash stays open pending a hardware boot log.
      **QEMU attempt (2026-11-08):** `qemu-s3.sh` builds and boots the S3 image
      under Espressif QEMU's `esp32s3` machine. QEMU has no PSRAM model
      (`quad_psram: not connected` aborts in `cpu_start`); with `CONFIG_SPIRAM=n`
      the image reaches IDF `app_init`/`heap_init`/`spi_flash` but stops before
      the firmware's `[magent]` logs — QEMU's clock/entropy/peripheral
      emulation is not sufficient to reach `main`. The emulator therefore does
      **not** reproduce the on-device Lua fault; a real S3 boot log is still
      required.
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
- [x] **Fixed 2026-11-09 (option: migrate both write paths to DBO2).**
      `device_key::seal_dev_identity` and
      `main::seal_and_store_wifi_pass` now call
      `wifi_pass_seal_v2::seal_str`. `open_dev_identity` opens DBO2 first and
      still accepts a DBO1 payload, re-sealing it forward in place; the BTDK1
      bound grew to `6 + wifi_pass_seal_v2::MAX_ENCODED_LEN = 195` bytes.
      `magent-core/tests/at_tests.rs` now exercises DBO2 (and the DBO1
      fallback when the `web3` feature — which supplies HKDF/HMAC — is off).
      Verified by `cargo check` for S3 and C61. The bullets below are the
      original finding, kept for the record.
- [x] **The two live write paths were switched to DBO2.** *Verified 2026-11-16:*
      both are already `wifi_pass_seal_v2::seal_str` —
      `device_key.rs::seal_dev_identity` (the 32-byte identity seed) and
      `main.rs::seal_and_store_wifi_pass` (the env-var AP default). Nothing in
      `firmware/` references `wifi_pass_seal::seal_str` any more, so no write
      path produces DBO1. The DBO1 *reader* stays, and both migration paths
      are in place: `open_dev_identity` re-seals on open when it sees
      `Dbo1Decoded`, and `open_stored_wifi_pass` accepts DBO1 through
      `open_sealed_v2`'s fallback. `AT+CWJAP?` still reports
      `DBO1_LEGACY`/`PLAINTEXT_LEGACY` so an operator can see leftovers.
      Tests that still assert `starts_with("DBO1:")` (`wifi_pass_seal`'s own
      unit test, `at_tests::…` behind `cfg(not(feature = "web3"))`) exercise
      the legacy *format*, which is correct and must keep working.
- [x] **Why the old format mattered.** DBO1 was a hand-rolled repeating XOR
      with **no MAC**, so a flipped bit in `magent:dev_identity` silently
      produced a different seed — and therefore a different device DID and
      wallet — with no error anywhere. The identity seed is now DBO2
      (HKDF-stretched per-entry key + truncated HMAC-SHA256), which rejects
      the same tampering with `BadMac`.

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
- [x] **Fixed 2026-11-09.** `wifi_pass_seal::seal_str`,
      `wifi_pass_seal_v2::seal_str`, and `wifi_pass_seal_v2::seal_secret` now
      return `SealError::BadNonceLength` unless `nonce.len() == NONCE_LEN`.
      Regression tests: `non_full_width_nonce_rejected_on_seal`,
      `seal_rejects_non_full_width_nonce`,
      `secret_rejects_non_full_width_nonce`. The original finding follows.
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
- [x] **Separate update-visibility defect fixed 2026-11-09 (round 2).**
      `set` appends and `get` used to return the first matching entry, so an
      overwrite was stored but never readable (and duplicates accumulated).
      `get` now returns the newest match (`setting_an_existing_key_makes_the_new_value_visible`).
      The compaction durability item below is unchanged.
- [x] **Mitigated 2026-11-08 (option (a)).** `compact` now retries the whole
      rewrite pass once after a failed `write_entry_at` (the live set is still
      in RAM); if the retry also fails it logs how many entries were rewritten
      and that the store may be incomplete, then returns the error. This
      recovers a transient flash fault but is **not** power-loss atomic — option
      (b) (staging region) remains the durability end-state if the owner wants
      crash-safety, and is still a store-format change.

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
- [x] **The silent pin default is closed (2026-11-08).** A pin that does not
      parse as an integer — `pin=abc`, or `pin` omitted entirely — used to fall
      back to the `"13"` default at `local_tools.rs:70`, so a malformed
      argument drove GPIO13 rather than failing. `write_gpio` now returns a
      failed tool result for a missing/non-numeric pin, and rejects a
      `state=` token outside `high|low|1|0` (`on`/`off` accepted as aliases)
      instead of driving every unrecognised value low. `flash_write` /
      `flash_read` were the same shape with `address=0` and now require the
      argument. Note that `magent-core`'s *simulated* `execute_write_gpio`
      deliberately keeps its documented default
      (`execute_write_gpio_missing_pin_defaults_to_zero`, `tools.rs:1252`), so
      the two paths stay intentionally different there; the firmware path is
      the one that touches hardware. Verified by `cargo check` only, as for all
      firmware changes.
- [ ] **What is left.** ESP-IDF's mask is `GPIO 0..=29` on the C61 — it does
      **not** exclude the SPI-flash or strapping pins, so the HAL check says
      nothing about whether a given pin is *safe to drive* on this board. An
      allow-list of truly-safe output pins needs the board's flash/JTAG/USB
      assignment, which is not recorded anywhere in this repo
      (`firmware/esp32-app/BOARDS.md` documents only the S3 console pins and
      UART0). Not applied because it is a hardware-behaviour change the
      `cargo check`-only verification cannot validate, and the safe-pin set is
      a board fact, not something to guess.

### 🟢 P2 — the CLI's `magent web3 chain|bind|balance|tx|vc` commands are documented but not implemented (2026-09-23) — **fixed in round 23**
- [x] **Resolved 2026-09-26 (audit round 23): implemented, not deleted.** The
      owner chose full implementation over deletion. All nine commands are now
      wired end to end (parser arms + executor on the real core APIs), plus a
      `bind verify` metadata-binding fix for a real tampering hole. See
      "audit round 23" below for the details. The bullets that follow are the
      original finding, kept for the record.
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
- [x] **`search_emails` now rejects an empty query (option (a)), 2026-11-09.**
      The check runs against the *escaped* form, so whitespace-only and
      quote/backslash-only queries — which `escape_imap_string` reduces to
      `""` — are rejected too; the schema gained `minLength: 1`. The
      `shutdown()` API-shape question below remains an owner decision.
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
- [x] **Partially resolved 2026-11-09 (round 3): `shutdown_async()` added.**
      Async callers no longer need the blocking form; the synchronous
      `shutdown()` also works inside a runtime now (it drives the dedicated
      runtime from a scoped helper thread instead of `Handle::block_on` on the
      current one). The remainder of the original note follows.
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
- [x] **Both fixed 2026-11-09.** `handle_tools_call` now keeps an unknown
      method/tool as a JSON-RPC error but returns a normal result with
      `"isError": true` for a handler failure (matching `mqtt-mcp`).
      `mark_read` probes with `UID FETCH <uid> (UID)` first and returns an
      error when no message exists, instead of reporting `marked_read` for a
      UID that was never touched. Tests: `unknown_tool_is_a_protocol_error`,
      `a_handler_failure_is_an_mcp_tool_error_not_a_protocol_error`. The
      original finding follows.
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
- [x] **Option (c) implemented 2026-11-08.** After a successful
      `config set`, human mode now prints
      `warning: this value leaves N issue(s) for config validate` when the
      mutated record fails `validate_record`. The write still succeeds (the
      type/semantic split is unchanged), and JSON mode's envelope is untouched.
      Options (a) refuse-on-write and (b) document-only remain owner decisions.

### 🟢 P2 — network / backend resilience: timeout, retry, backoff (2026-09-25)
Audited every outbound HTTP path before changing anything.
- [x] **Already covered, no change needed:**
  - `magent-core/src/web3/blockchain/http_client.rs` — full retry
    (`max_retries: 3`, `base_backoff_ms: 100`, `max_backoff_ms: 2_000`) with
    a 30s timeout, retrying only transport-level failures via
    `http_post_with_retry`. This was the model the LLM path lacked.
  - `magent-core/src/web.rs` — `HTTP_TIMEOUT = 15s` on every fetch.
  - Both LLM clients — a 60s per-call timeout, so a stalled connection
    cannot hang the agent.
- [x] **The gap: the LLM backends had no retry at all.** `OllamaClient` and
      `DeepSeekClient` sent once and surfaced the failure, so one transient
      `503` (model server restarting), one `429`, or one connection reset
      failed the whole `magent run` — the agent's *only* hard dependency was
      its least resilient one. Added a module-level `send_with_retry`
      (`LLM_MAX_RETRIES = 2`, so 3 attempts; `250ms → 500ms`, capped at
      `2s`) wired into the three call sites: Ollama `chat_with_messages`,
      Ollama `generate`, DeepSeek `chat_with_messages`.
      **Retry scope is deliberately narrow** — a transport error, or
      `408` / `429` / any `5xx`. Any other `4xx` is deterministic, so
      retrying would only delay an identical error and burn quota;
      `is_transient_status` encodes exactly that line and is unit-tested.
      **The `Response` is returned untouched** when no retry is warranted,
      which keeps DeepSeek's `HTTP error: <status> — <body snippet>`
      diagnostic working.
      **Why the runner still fails loudly:** this does not reintroduce the
      old silent fallback — `think()` already returns
      `Err("LLM backend error: …")` rather than synthesising a simulated
      reply, and the retry only adds attempts *before* that error.

#### Audit of the retry itself (2026-09-25, same day)
- [x] **Retries were invisible.** A retried call just took longer and then
      succeeded, so "why is this run slow?" had no answer from outside the
      process. `warn_retry` now logs attempt *n*/*total*, the cause and the
      delay on the `magent.llm` target (`--verbose`, or
      `RUST_LOG=magent.llm=warn`). **Why `warn` and not `info`:** a retry
      means the run degraded and recovered — worth seeing by default under
      `--verbose` without turning on trace for everything.
- [x] **`501` / `505` were classified transient.** Both are `5xx`, so
      `is_server_error()` swallowed them, but "Not Implemented" and "HTTP
      Version Not Supported" are permanent: the same request gets the same
      answer, so the retry only burned 750ms before reporting the identical
      error. Now excluded and asserted in the classification test.
- [x] **The status-retry path had no test at all.** `is_transient_status`
      was unit-tested but nothing proved a `503` actually produced a second
      request — the loop could have been dead and the tests would still
      pass. Added `http_mock`, a throwaway HTTP/1.1 server (one request per
      connection, no keep-alive, matching `pool_max_idle_per_host(0)`), and
      drove the retry through it end-to-end. **Why a real socket:** the
      behaviour under test is "a second request happens, on a new
      connection, with the same body" — a faked `Response` cannot express
      that, which is why the earlier tests only covered the transport path.
- [x] **The `body.clone()` invariant is now pinned.** DeepSeek re-attaches
      an `Authorization` header and a cloned body on every attempt; if a
      retry dropped either, the failure would be silent (a `401`, then a
      `400`). The DeepSeek test captures both requests and asserts the body
      and header are byte-identical across the attempt and the retry.
- [x] **Simplification: the `E: Display` generic was test-only.** The error
      type was generic so an early test could retry on a `String` instead
      of a `reqwest::Error` it had no way to construct — a seam that
      existed purely for a test that faked the thing under test. With
      `http_mock` (and a closed port for the transport case), the concrete
      `reqwest::Error` is used and the production signature is simpler.
- [x] **Documented worst case, deliberately accepted.** A hung backend is
      retried too, so a call's worst case is `(retries + 1) × timeout` —
      3 × 60s. Retrying a timeout is what the OpenAI SDK does, and the
      alternative (excluding timeouts) would leave the most common slow
      failure — an overloaded model server — with no retry at all. Stated
      in the `send_with_retry` doc comment so the next reader sees it.
#### Second audit — of the retry's own test harness (2026-09-25)
- [x] **The retry never covered the run's *first* call.** The retry was
      wired into the three chat sites but not into `check_connection()`,
      whose `false` is load-bearing: `RealAgentRunner::run_inner` returns
      `Err("LLM backend unreachable (…); refusing to fall back to
      simulated responses")` and the CLI's `runner.rs` either warns and
      falls back to *simulated answers* or refuses outright under
      `--probe-ollama`. So one transient blip at startup — a connection
      reset, or a `503` while the model server was still loading — cost
      the whole session, and the chat-path retry could never help because
      the run aborted before reaching it. **This was the worst possible
      place to be un-resilient**, since the probe is the first thing that
      touches the network. Both probes now retry. Pinned by
      `the_ollama_probe_survives_a_transient_503`,
      `the_deepseek_probe_survives_a_transient_503`, and
      `a_probe_that_never_recovers_is_still_reported_not_connected` (which
      also proves the probe spends a bounded budget and stops, rather than
      retrying forever). **`get_models` deliberately still does not
      retry:** only `doctor` uses it, for a listing, and an empty list is
      not load-bearing — the principle is "retry where the answer gates
      behaviour, not where it is informational".
- [x] **The visibility fix was itself untested** — the same class of gap
      as the untested status path. Nothing proved the warning said which
      attempt, what failed, or how long the wait was; deleting the whole
      `warn_retry` call would have left every test green. The message is
      now built by a pure `retry_warning()` and pinned by
      `a_retry_warning_names_the_attempt_the_cause_and_the_wait`. **Why
      not assert the log directly:** `log`'s logger is process-global and
      installable only once, and the test harness runs tests in parallel,
      so capturing log output reliably is not possible here — testing the
      string the logger is handed is the honest version.
- [x] **A swallowed `Result` in the mock could make an assertion pass for
      the wrong reason.** `let _ = reader.read_exact(&mut bytes)` on a
      short read recorded an *empty* body, and
      `assert_eq!(seen[0].body, seen[1].body)` would then compare `""` to
      `""` and pass. Now a hard `expect`: a truncated request means the
      harness is broken, not the code under test.
- [x] **The mock's contract was undocumented**, which invited wrong use:
      it answers only *after* recording (so a test that saw the reply has
      seen the record — the mutex's release/acquire edge), it reads
      `Content-Length` bodies only (`chunked` is not implemented, and a
      chunked request fails its assertion loudly rather than passing
      quietly), it serves one request per connection, and its thread is
      detached and outlives the test. All four are now stated on the
      module.
- [ ] **Not done — the retry warning does not quote the server's body.**
      A retried `503` logs `HTTP 503 Service Unavailable` but not
      Ollama's `{"error":"model is loading"}`. Reading the body would mean
      consuming the response on the *recovery* path, where a slow or
      never-terminating error body would add latency right when we are
      trying to get past a failure — the wrong place for it. The body is
      still surfaced when the retry ultimately fails, via the caller's
      existing `HTTP error: <status> — <body>` message.
- [ ] **Not done — `http_mock` lives inside `agent_runner.rs`** (a file
      already over 4 000 lines) instead of a shared `#[cfg(test)]`
      support module. It is generic enough for `web.rs`'s network tests
      to reuse, but nothing there needs it today, and moving it would not
      shrink the crate. Worth doing the next time a second caller
      appears.
- [ ] **Owner's call — `Retry-After` is ignored.** A `429` carrying
      `Retry-After: 30` still gets the fixed `250ms` backoff, so a rate
      limiter gets three quick requests and then the error. Honouring it
      properly is not a one-liner: a large value must *not* be slept
      through (the CLI would block for minutes with no output), so the
      server's request has to be reconciled with a cap, and the
      HTTP-date form is a second parser. Left to the owner; the current
      behaviour is at least bounded and logged.
- [ ] **Owner's call — the two clients duplicate the retry plumbing.** They
      are separate types with separate bodies (different auth headers,
      different response shapes), and the helpers are already shared, so
      the remaining duplication is three `send_with_retry(…)` calls rather
      than logic. Extracting a common transport layer would be a larger
      refactor than the problem justifies; noted rather than done.
- [ ] **Not done — no jitter.** The backoff is deterministic, so two agents
      that fail at the same moment retry at the same moment. Correct for a
      single-process CLI hitting a local Ollama; a fleet hammering a shared
      endpoint would want jitter. Out of scope until something is actually
      multiplexed.
- [x] **~~Deliberate, no change — the probes do not retry.~~
      SUPERSEDED by the second audit below**, which found the probe to be
      the single worst place to be un-resilient: its `false` aborts the run
      outright, so the chat-path retry could never be reached. Both
      `check_connection` probes now retry; only `get_models` was left
      alone, and the reason is different — it is informational, not
      load-bearing. The original reasoning ("`magent doctor` wants one
      round trip") applies to `doctor`, which is why `get_models`, its one
      caller, kept the old behaviour.
- [x] **Deliberate, no change — the web tools do not retry.**
      `web.rs`'s `fetch_url` / `web_search` return `Err(String)` *to the
      agent*, which sees the failure as a tool result and can re-plan —
      including calling the tool again. That is a retry at the right
      level: the agent knows whether the page matters, and a fixed
      client-side retry would just multiply the wait on a URL that is
      genuinely gone. (`web.rs` already says as much in its module doc.)

### 🟢 P2 — agent-loop audit: context pairing, error state, chatter-as-answer (2026-09-25)
Scope: the ReAct loop itself (`RealAgentRunner`, `conversation.rs`) rather than
the transport under it — i.e. "is the agent's own machinery complete?", asked
after the network/resilience pass above. Found by driving the loop in tests and
by running the CLI end-to-end (`--mock`, `--json`, `-i 1`).
- [x] **The context window could orphan half a tool-call pair, and the
      provider rejects half a pair.** `slice_messages` and `gc_to_budget`
      choose what to keep by *index*, so the boundary can land between an
      assistant `tool_calls: [{id: N}]` message and the `tool` message with
      `tool_call_id: N`. OpenAI-compatible APIs answer that with a `400`
      ("tool_call_ids did not have response messages", and the mirror
      complaint for an unanswered call), so once a session's history crossed
      the window it would fail *every* subsequent turn on a history the user
      could not see, let alone fix. The loop itself always pushes the pair
      adjacently — only compression can split one. Both functions now call
      the new `prune_dangling_tool_pairs` before returning, which removes
      only the half that lost its partner; a well-formed conversation is
      returned byte-identical, and the repair only runs when the function
      actually dropped something (so a history already inside the budget is
      untouched, even if a caller handed us a lone half).
      **A trap this exposed:** several test fixtures built "conversations"
      out of bare `tool(id, …)` messages with no owning assistant call —
      malformed histories that the new repair correctly deletes, which is
      how the fixtures were found. They now build real pairs via a `call(id)`
      helper. A fixture that cannot exist on the wire is not a fixture.
      Pinned by `a_slice_boundary_inside_a_pair_does_not_orphan_the_result`,
      `a_byte_gc_cut_does_not_orphan_the_result` (both confirmed to fail with
      the repair removed — the failure message names the orphan), plus
      direct unit tests for the repair.
- [x] **`AgentState::Error` was declared, guarded on, displayed — and never
      assigned.** Every failure path returned `Err` through `?` while the
      state stayed at `Thinking`, so `RunReport.state` — the field the
      `--json` envelope uses to describe the outcome — reported a run that
      had just died as still in progress. Worse, the failure branch was
      *three* places (probe failure, mid-run backend error, budget
      exhaustion), so a partial fix would have left the contract
      half-true. All three now set `Error`, giving the invariant
      `run() == Err(..)` ⇔ `state() == Error`; a converged run still ends
      `Finished`. Pinned by `a_failed_run_leaves_the_runner_in_the_error_state`.
      **Not done — a `BudgetExhausted` state of its own.** Exhausting the
      budget is a different failure from a backend error, and collapsing
      both into `Error` loses that. It would mean a new variant on a
      `#[repr(u8)]` public enum, so it wants a deliberate API decision, not
      a drive-by edit.
- [x] **Mid-plan chatter was returned as the CLI's `RESULT`.** `get_result`
      fell back to the last assistant text with no classification, and its
      comment justified that with a case `parse_result` already handles (a
      fenced answer is classified as a result earlier in the same call), so
      the fallback only ever fired for text the loop had *rejected*: too
      short, or chatter-like. Ten iterations of "ok" therefore ended the run
      `Ok("ok")`, printed under the `RESULT` banner, exit `0` — the same
      fabricated success `run_tool_only_exhaustion…` was written to stop.
      The fallback is now split by `looks_like_chatter`: chatter errors with
      the text quoted (so nothing the model said is lost, only relabelled),
      while a terse but non-chatter answer is still handed over. **Why not
      simply error on everything:** `MIN_PROSE_RESULT_LEN` rejects a
      legitimate `22.5` for being short, and turning a correct terse answer
      into a hard failure is a worse bug than the one being fixed — hence
      the split rather than a deletion.
      Pinned by `run_exhausts_iteration_budget_on_chatter_and_reports_the_dead_end`
      and `a_terse_answer_below_the_prose_threshold_is_still_returned`.
- [ ] **The loop executes one tool call per iteration; a multi-call response
      silently loses the rest.** `parse_tool_call_from_json` reads
      `tool_calls[0]` and returns it, so a DeepSeek/OpenAI-style response
      asking for three actions runs only the first. It is *not* a wire
      problem — the assistant message we append carries only the executed
      call, so the next request is well-formed and the model can re-request
      — but it is a correctness problem: the model is never told the other
      calls were skipped, so a model that assumes they ran reasons from a
      state that never happened. The honest fix is to execute every call in
      the turn, which means `Message::tool_call` becoming a `Vec` (it is
      `Option<ToolCall>` today) and updating both wire writers
      (`write_message_into`, the Ollama body), `estimate_bytes`,
      `approx_tokens` and the summary DTO — a real refactor with real blast
      radius, so it wants its own round rather than a warning bolted onto
      this one.
- [ ] **Not done — the `GET /api/tags` probe can report "connected" for a
      backend whose completion endpoint refuses.** Observed locally while
      testing: a gateway in front of Ollama answered `200` on
      `/api/tags` — so the CLI printed `[Ollama] connected at …` and
      `check_connection()` returned `true` — while `/api/chat` answered
      `401` with an empty body, so the first real LLM call failed with
      `LLM backend error: HTTP error: 401 Unauthorized`. The probe is
      unauthenticated and the `OllamaClient` has no API-key plumbing at
      all, so that deployment can never work; the message is what is wrong.
      Options: probe an endpoint that exercises the completion path, or
      special-case `401`/`403` on the chat path into a diagnosis that says
      "the backend answered but refused this client". Not fixed here
      because both change behaviour for backends that are fine today, and
      the failure *is* reported clearly — just later and less precisely than
      it should be.
- [x] **A run can be bounded in wall-clock time (2026-11-08).**
      `max_iterations` / `max_tool_calls` bound the *count*, not the time, so
      the worst case used to be `iterations × (1 + retries) × client timeout`
      (30 minutes with the defaults). `RunnerConfig::max_run_duration` is now
      checked at the head of every ReAct iteration; the CLI flag is
      `--timeout <SECS>` (one-shot and `--repl`, fresh budget per turn). The
      default is `None`, so existing callers keep today's behaviour. A firing
      budget emits `BudgetExhausted { kind: "wall_clock_ms" }` and the
      no-answer error names it. **Still true by design:** an in-flight HTTP
      request is bounded only by that client's own timeout — the loop check
      cannot interrupt it.

### 🟢 P2 — agent-loop audit, round 2: the repair's own blind spot (2026-09-25)
Scope: the code the round above *added*, re-audited the same way — drive it in
tests, then reason about what it assumes. The first item is the same class of
bug as the one it was written to fix, one level down.
- [x] **The half-pair repair matched on id alone, and a REPL session recycles
      ids.** `reset_run_state` clears `tool_call_count` at the head of every
      `run_continuing`, so each turn re-issues `call_1`. A set-membership test
      therefore let a *later* turn's call vouch for an *earlier* turn's
      orphaned `tool` message: the pair looked closed to the repair, stayed in
      the history, and the provider still saw a `tool_call_id` with no matching
      call — the exact `400` the round-1 work existed to prevent, reachable
      again the moment the session ran past its context window. The repair is
      now **order-aware**: a `tool` survives only if a call *precedes* it, a
      call only if a result *follows* it. Position, not id uniqueness, is what
      makes a pair — id uniqueness was an assumption the REPL never honoured.
      Pinned by `prune_survives_a_session_that_re_uses_tool_call_ids`,
      confirmed to fail against the set-based body (`left: 0, right: 1` — it
      dropped nothing).
- [x] **A rescued answer left `RunReport.state` mid-flight.** When the loop
      broke on a budget but `get_result` handed back a terse answer, no
      terminal state was assigned, so a run that returned `Ok` and printed a
      result reported `state: "Observing"` in the `--json` envelope. The `Ok`
      arm now sets `Finished`, extending round 1's invariant to
      `run() == Ok` ⇔ `state() == Finished`, symmetric with the `Err`/`Error`
      pair. Pinned inside `a_terse_answer_below_the_prose_threshold_is_still_returned`.
      **Deliberately still not a `BudgetExhausted` variant:** this round only
      had to stop a *completed* run from looking unfinished, which `Finished`
      does without public-API surface. Distinguishing "converged" from
      "budget ran out but the rescue fired" still wants a new `#[repr(u8)]`
      variant and stays the owner's call (see round 1).
- [x] **`get_result`'s comment named two kinds of leftover text; there are
      three.** A bare JSON object the classifier rejects for lacking a
      `result` key (`{"temperature":22.5}`) is neither chatter nor a terse
      prose answer, yet it is passed through — a real case the comment did not
      mention, which is how a deliberate behaviour starts looking like the
      oversight the chatter split was meant to end. The comment now names it,
      and `a_json_shaped_leftover_is_handed_over_rather_than_called_chatter`
      pins the choice. (The fixture uses a non-object value on purpose:
      `parse_tool_call_from_json`'s format 3 would read
      `{"temperature":{"value":…}}` as a tool named `temperature`.)
- [ ] **Duplicate `call_N` ids across turns are a latent wire ambiguity.**
      Same root cause as the first item, one level up: because the ids are
      recycled, a single request can carry two calls both named `call_1` —
      possible only if two *unanswered* calls from different turns ever
      coexist, which compression cannot produce today (it drops history from
      the oldest end, and each turn's pair is adjacent). Nothing observed
      rejects it, so the id format is left alone rather than renamed to a
      session-monotonic counter — that touches the wire path and its tests for
      a failure that has not been reproduced. Recorded so the next person
      sees it is known, not missed.

### 🟢 P2 — agent-loop audit, round 3: `get_result` went looking outside its turn (2026-09-25)
Scope: the round-2 rewrite of `get_result`, driven this time by the REPL rather
than by a single `run` — the multi-turn shape is what exposed both items. One
of them is a regression *introduced* by round 1, found only because round 2's
fix made the boundary condition explicit.
- [x] **A result the loop had accepted was re-judged on the way out.** Round
      1 split the leftover-text fallback by `looks_like_chatter`, but the
      split ran on the *path where the loop had already succeeded* too:
      `think` accepts `{"result": …}`, pushes the extracted text and sets
      `Finished`, then `get_result` fed that text back through the same
      classifier. `looks_like_chatter` lowercases and matches bare prefixes,
      so `first place is Alice`, `ok, done`, `next step is done` — all
      legitimate envelope answers — were rejected after acceptance, and the
      run returned `Err` despite the model having answered. The fix is to
      treat the loop's own verdict as authoritative: on `Finished`, return the
      stored result verbatim. **The trap that hid this:** a fixture answer of
      `FIRST_TURN_ANSWER` is *itself* chatter-like (`first` is a claimed
      prefix), so the first version of the regression test passed for the
      wrong reason — the error even contained the expected
      "without a final answer" wording. An assertion that only checks a
      substring of an error message can be satisfied by the wrong error; the
      test now asserts the specific diagnosis and the returned value. Pinned
      by `a_result_the_loop_accepted_is_not_re_judged_on_the_way_out`.
- [x] **A REPL turn could return the previous turn's answer as its own.**
      `get_result` scanned `messages` end-to-back with no lower bound. With
      `run_continuing` the history spans turns, so a turn that produced no
      assistant text of its own — every iteration a tool call, then the tool
      budget ran out — walked back into the *previous* turn, found its
      answer, and (being long and not chatter-like) returned it. The CLI
      printed another turn's answer under `RESULT`: a stale success of the
      same family as the fabricated one round 1 removed, differing only in
      which turn the text came from. The scan is now bounded to the current
      turn, taken to start after the last user message (the task `run_inner`
      pushes, and which `slice_messages` promises to keep). Pinned by
      `a_turn_with_no_answer_of_its_own_does_not_reuse_the_previous_turns`,
      confirmed to fail against the unbounded scan (it returned
      `"the temperature in the lab is 22.5 degrees"` as turn two's result).
- [ ] **The same shape in the embedded scaffold** — `agent::SmartwatchAgent`'s
      `get_final_result` returns the last assistant message with no verdict
      check and no turn bound. Not fixed: its `run` clears the conversation
      per task, so the cross-turn leak cannot occur, and the loop there is a
      deterministic offline heuristic rather than a model dialogue. Noted so
      the difference is visible if that agent ever grows a continuing mode.

### 🟢 P2 — agent-loop audit, round 4: the scaffold answered the wrong turn (2026-09-25)
Scope: the offline/simulated responder (`generate_simulated_response`, the
path a device takes with no model reachable) exercised through the real CLI
with `--repl --mock` rather than through unit tests alone — which is how a
turn-two defect showed up as "the answer was already familiar".
- [x] **The scaffold's script was keyed off the first user message.** Found by
      running `magent run --repl --mock` with two scripted turns: the second
      task (`turn the led on`) was answered with the first task's script
      (`Health check complete - all systems operational`). `first_user_message`
      is right for a single `run` — that path clears the history, so first and
      current coincide — but the REPL keeps one history, so the scaffold spent
      the rest of the session answering turn one. Renamed to
      `current_turn_task`, anchored on the **last** user message: stable within
      a turn's iterations (which the keyword scripting requires) and current
      across turns. Pinned by
      `a_second_repl_turn_is_answered_by_the_second_task_not_the_first`,
      confirmed to fail against the old anchor by returning the health script.
      **The pattern worth naming:** three separate defects in this pass —
      `get_result`'s unbounded scan, and now the script anchor — are the same
      bug wearing different clothes: something that means "this turn" was
      written as "somewhere in the history". The one place that got it right
      from the start is `slice_messages`, whose task anchor `get_result` now
      matches; the two are worth reading together.
- [ ] **The scaffold has no memory of what the previous turn concluded.** It
      keys off the current task's keywords and the tool count *within* the run,
      so a follow-up like "and now the humidity?" is not understood as a
      continuation — it either matches a fresh keyword branch or falls to the
      default. Not fixed: this is the deliberate limit of a scripted fallback,
      and the branch table would have to grow state to fake continuity, which
      is worse than the honest scripting it does now. Noted so a future
      "the mock felt dumb" report has the explanation ready.

### 🟢 P2 — agent-loop audit, round 5: the tool layer dropped every integer argument (2026-09-25)
Scope: the boundary below the loop — `SimulatorExecutor` /
`AgentSimulator::execute_tool` — reached by reading a real transcript rather
than by a failing test. Found in the round-4 CLI run: the model asked for GPIO
13 and the tool answered "GPIO pin 0 set to high".
- [x] **`extract_int_arg` understood only the legacy `key=value` shape, but the
      loop sends JSON.** `ToolCall::arguments` is serialised by
      `agent_runner::execute_tool` into `{"pin":13,"state":"high"}` — the same
      shape a real model emits — so the `pin=` scan found nothing, each call
      site fell back to `unwrap_or(0)`, and the tool reported success for an
      argument that was never read: `write_gpio` switched pin **0** for
      `{"pin":13}`, and `flash_read`/`flash_write` used `0x0000` for any
      address. The string arguments were unaffected (`read_sensor`, the gpio
      `state`, `ble_send` all use substring matching, which survives both
      shapes) — only the three integer call sites were wrong, which is why it
      stayed invisible: the reply *looked* like a normal tool result. A model
      reading it was told the device was in a state it had not asked for.
      JSON is now tried first, with the `key=value` scan kept for the scripted
      callers. Pinned by `write_gpio_reads_the_pin_from_a_json_payload`,
      `flash_tools_read_the_address_from_a_json_payload` and
      `extract_int_arg_accepts_json_and_key_value_payloads`, all confirmed to
      fail against the legacy-only parser; end-to-end the CLI now logs
      `GPIO pin 13 set to high`.
      **The lesson:** the fake backend is part of the agent's contract. Its
      arguments are produced by the same code path a real model feeds, so a
      mismatch there is a model-facing defect, not a test-fixture detail —
      and it hid because every failure mode of the parser was a *default*,
      not an error.
- [ ] **`unwrap_or(0)` still hides a genuinely absent argument.** With the
      parser fixed, `0` is only reached when the model omits `pin`/`address`
      altogether, and the tool then acts on pin 0 and reports success. An
      error ("write_gpio requires a pin") would be more honest, but it changes
      behaviour for callers that rely on the default and the tool description
      does document pin 0 as valid. Left as a judgement call for the owner.

### 🟢 P2 — agent-loop audit, round 6: the third executor was never fixed (2026-09-25)
Scope: cross-executor parity. Three implementations of the same tool set exist —
the embedded one in `magent-core/src/tools.rs` (the firmware), the host one in
`host/simulator/src/main.rs`, and `magent-core/src/simulator.rs` (what the CLI's
`SimulatorExecutor` calls). The first two carry comments recording that the
`state`-by-substring and discarded-`data` defects were fixed; the third still
had the pre-fix behaviour. Found by reading the three side by side, not by a
failing test — the round-5 fix brought the *integer* arguments into line but
left the string ones.
- [x] **`write_gpio` chose the level with `args.contains("high")`.** An omitted
      `state` and the firmware's own numeric spelling `"state":"1"` both drove
      the pin **low** — the opposite of `tools.rs`'s `high` default — and a
      nonsense value like `"mid"` was accepted silently. Now parsed through the
      new `extract_str_arg` with parity semantics (`high|1` / `low|0` / absent →
      High / else `ConfigurationError { field: "state" }`). Pinned by
      `write_gpio_defaults_to_high_and_rejects_a_bogus_state`.
- [x] **`flash_write` discarded the model's `data`** and wrote the literal
      `format!("data at {address}")`, then reported that fabricated string's
      length — so a write-then-read handed the model back text it never wrote,
      and the byte count described the fabrication (12 bytes for `data at 1024`).
      Now persists the `data` value. Pinned by
      `flash_write_stores_the_payload_the_model_sent` (9 bytes for `"Log entry"`,
      hex `6C6F67` for `"log"` on read-back).
- [x] **`ble_send` forwarded the raw argument envelope.** `self.ble.send(args)`
      put `{"data":"System health OK"}` on the air — 27 bytes of braces, quotes
      and key name — for a 16-character message, and reported 27. Now sends the
      `data` value. Pinned by
      `ble_send_transmits_the_message_not_the_args_envelope`.
- [x] **All three tests verified to fail against the legacy branches** before the
      fix was restored: `Sent 27 bytes via BLE`, `Wrote 12 bytes to flash at
      0x0400`, and a default `GpioPinState::Low`. Confirmed end-to-end on the
      real CLI, where the mock scaffold drives each tool:
      `magent run --mock 'switch on the indicator led'` → `GPIO pin 13 set to
      high`; `… 'save this reading to flash storage'` → `Wrote 9 bytes to flash
      at 0x0400`; `… 'send a BLE message'` → `Sent 17 bytes via BLE`.
      **The lesson:** "the other executor has this fix" is not evidence that
      *this* one does. A duplicated implementation is a place where a fix has to
      be applied N times, and the copy that the test suite exercises least —
      here the CLI's simulator, which nothing else asserts against — is the one
      that gets missed. The three-way comparison, not the green suite, is what
      surfaced it.
- [ ] **`extract_str_arg`'s `key=value` fallback splits naively.** In the
      non-JSON branch it terminates the value at the first `,`, `;` or
      whitespace, so a scripted `data=hello world` yields `hello`. The JSON
      branch (what the loop actually sends) has no such limit, and no caller in
      the tree passes a spaced value in the legacy shape, so this is latent
      rather than live. Fixing it means deciding on a quoting/escaping scheme
      the scripted callers would have to adopt.
- [ ] **`unwrap_or(0)` still hides a genuinely absent `pin`/`address`** (carried
      over from round 5). Left to the owner: the tool description documents pin
      0 as valid, and erroring would change behaviour for callers that rely on
      the default.

### 🟢 P2 — agent-loop audit, round 7: the third executor, part two (2026-09-25)
Scope: the rest of `AgentSimulator::execute_tool`, after round 6 aligned
`write_gpio`/`flash_*`/`ble_send`. Found by re-reading every remaining branch
against `tools.rs`, which is where the same defects are already fixed and
commented.
- [x] **`read_sensor` matched sensor names as substrings of the whole payload,
      case-sensitively and with no aliases.** `tools.rs::execute_read_sensor`
      routes on the parsed `sensor` value — its comment says so explicitly, and
      `execute_read_sensor_with_structured_args` pins it — but the simulator
      still ran the old `args.contains("…")` chain. Two live consequences:
      `{"sensor":"hrv","source":"heart_rate sensor"}` read the **heart rate**
      (the neighbouring field won the race), and `{"sensor":"Temperature"}`
      failed with a hard error while the firmware's `normalize_sensor`
      lowercased it and read it fine. Now resolved as the sensor argument and
      mapped through a normaliser with the firmware's alias table. Pinned by
      three tests, all confirmed to fail against the chain.
      **The lesson:** round 6's "the third executor was never fixed" was not a
      one-off — it is a *property of a triplicated implementation*. The right
      move after finding one divergence is to walk every remaining branch of
      the copy, not to stop at the one that showed up.
- [x] **A present-but-unrecognised sensor name reported `MissingField`.** A
      `name` that is supplied but not one of the known sensors is an invalid
      value; only an absent name is missing. Same distinction the GPIO `state`
      fix draws. Now `InvalidValue` for the former, `MissingField` for the
      latter.
- [x] **Checked and found *not* divergent** (recorded so the next pass does not
      re-open them): `flash_read`'s hard-coded 64-byte window matches
      `host/simulator/src/main.rs` exactly, and `tools.rs`'s `length` argument
      belongs to a firmware stub whose "read" returns fabricated text rather
      than flash contents — so honouring `length` in the simulator would be
      inventing behaviour, not matching any. `web.rs`'s `extract_query` already
      parses JSON with a bare-token fallback and is covered by tests. An
      executor `Err` is turned into an `"Error: …"` tool message with
      `success: false` and the loop keeps iterating, so the simulator's hard
      error on a bad sensor is survivable — the model can retry — even though
      the firmware reports it as a `success: false` tool result instead.
- [ ] **The simulator cannot read `battery` or `memory`.** The firmware's
      `normalize_sensor` knows both and `execute_read_sensor` returns canned
      readings for them (`Battery:85%, Voltage:3700mV`;
      `free_heap=128000 B …`). `SimSensorManager` has no such reading, and the
      other simulated vitals are *derived* from `HealthSimState` rather than
      invented, so there is nothing to derive a battery level from. Left to the
      owner: adding it means either inventing a value with no simulated state
      behind it — the placeholder pattern this project has been removing — or
      adding a backing field to the simulator's model.
- [ ] **`MiniAgent::pick_tool` keeps an order-coupled keyword chain** (from
      round 7's sweep, not fixed). `heart` is tested before `hrv`, so a task
      naming both — "what is my heart rate variability (HRV)?" — routes to
      `read_sensor sensor=heart_rate`. Unlike the executor cases above this is
      a *task*-keyword heuristic with no single right answer (a question
      mentioning "heart" may well want the heart rate), so it needs a product
      decision rather than a mechanical fix.

### 🟢 P2 — audit round 8: real implementations, starting with the emergency protocol (2026-09-25)
Scope: not parity this time but *substance* — paths that report success, or
report nothing, without doing the work. Started from a scan for placeholder
markers; the firmware's `local_tools.rs` (real GPIO via `gpio_set_level`, real
`temperature_sensor_get_celsius`, real NVS flash, real HTTP) came back clean,
the real-hardware `ToolHandler` dispatch is already correct ("not implemented
on this target" rather than an invented value), and the caught-out case was
`early_warning.rs`.
- [x] **The emergency protocol was a no-op.** `trigger_emergency_protocol` ran a
      cooldown check, stored the timestamp, and returned. `generate_emergency_message`
      and `HealthAlert::mark_emergency_notified` were both unreachable from
      production code. A Critical alert therefore reached nobody and arrived at
      the caller with `emergency_contacted: false`. Now it selects the
      highest-priority SMS-capable contact, builds the message (hospital
      included), records an `EmergencyNotification`, and marks the alert —
      exposed through `pending_emergency_notification` /
      `take_emergency_notification`. Delivery is left to the layer with a
      transport, which is the honest split: `magent-core` under `no_std` has
      neither radio nor SMS, so "sending" here would be a new placeholder
      wearing the old one's clothes.
- [x] **Three unchecked cooldown subtractions.** `current_time_ms - self.last_*_ms`
      in the glucose, ECG and emergency paths panicked in debug and wrapped in
      release on a non-monotonic clock; the wrapped value read as "past the
      cooldown" and fired a duplicate alert. All three are `saturating_sub`
      now. Proven by restoring the old subtraction and catching
      `attempt to subtract with overflow`.
- [x] **The history copy and the returned alert disagreed.** Call sites cloned
      the alert into `alert_history` before triggering, so the recorded entry
      stayed `emergency_contacted: false` while the caller's copy said `true`.
      Triggering before recording fixes it; proven by restoring the old order
      and catching the `history must agree` failure.
      **The lesson:** "returns `()` and touches nothing" is the signature of a
      placeholder, and it hides well when the surrounding types are correct —
      `HealthAlert` had the flag, the module had the message builder, the tests
      exercised the flag through a direct call. Everything needed was present
      except the wiring. A cold read of "what does this function actually do to
      the world?" finds it; test coverage never would, because there was
      nothing wrong to assert.
- [ ] **Nothing consumes the notification yet.** `EarlyWarningSystem` has no
      production caller anywhere in the tree — the module is library-only — so
      the new `EmergencyNotification` is produced and marshallable but not yet
      delivered by anyone. Wiring it needs a product decision on the surface:
      an AT command plus a health-monitoring loop on the ESP32, a CLI
      subcommand on the host, or a Lua-bridge callback. Owner's call.
- [ ] **Alert-history overflow drops the newest alert silently.** All three
      `alert_history.push` sites discard the result (`let _ =`), so once
      `MAX_ALERT_HISTORY` (50) fills up, the heapless `Vec` rejects the new
      entry and nothing says so — the returned alert and the recorded history
      diverge again, in the patient's favour this time but invisibly. The
      caller still gets the alert back, so the impact is small; left to the
      owner because the fix is a policy choice (evict oldest, refuse new, or
      grow the bound).

### 🟢 P2 — audit round 9: the prompt promised four zero-argument tools that failed everywhere (2026-09-25)
Scope: the round-8 lesson applied to *reachability*. A tool can be implemented
and still be dead if the name the model is told to call never reaches the
implementation. Went through `HEALTH_SYSTEM_PROMPT` — the CLI's actual default
system prompt — name by name against every executor.
- [x] **`read_heart_rate()`, `read_glucose()`, `read_ecg()` never supplied
      their own sensor.** `register_builtin_tools` registers them as
      `read_sensor` aliases and the prompt documents them as zero-argument
      calls, but `ToolRegistry::execute` passed the caller's empty argument
      string straight into `execute_read_sensor`, which resolved the sensor to
      `""` and returned `"Unknown sensor"` / `"Invalid sensor type"`. Fixed by
      splitting out `execute_read_sensor_as(implied, args)` — explicit
      `sensor=` still wins, the implied sensor is the fallback. `check_stress_level`
      joined them as `ToolType::StressLevel = 11` (appended, so the existing
      wire discriminants are untouched) and is now registered as a builtin.
- [x] **The host executor had no arm for any of the four.**
      `AgentSimulator::execute_tool` — reachable from the CLI through
      `SimulatorExecutor` — matched neither the three aliases nor
      `check_stress_level`, so all four hit the `_` arm and returned
      `ConfigurationError { field: "tool", reason: MissingField }`. The
      `read_sensor` render body is now a shared `render_sensor` the aliases
      reuse, plus `resolve_sensor` / `resolve_sensor_as`, so the device and the
      host cannot drift here again.
- [x] **Both pinned by a test aimed at one fake LLM call each.** A test written
      against the real path — the scripted `--mock` LLM never emits an alias
      name, which is exactly why nothing caught this — would have needed a new
      mock scenario. `sensor_aliases_supply_their_own_sensor` and
      `read_sensor_aliases_resolve_on_the_host_executor` fail against the old
      dispatch with the two distinct errors above and pass after.
      **The lesson:** "the alias is registered" and "the alias works" are
      different claims. Registration told the model the tool existed; nothing
      checked that the dispatch filled in the one argument that made it
      meaningful. A name in a prompt is a promise the executor has to keep.
- [ ] **Twelve more advertised names have no implementation anywhere.**
      `start_exercise`, `end_exercise`, `get_exercise_goal`,
      `adjust_exercise_goal`, `get_sleep_quality`, `send_health_alert`,
      `queue_voice`, `send_meditation`, `set_dnd`,
      `notify_emergency_contact`, `find_nearest_hospital` — grep finds no
      handler in `magent-core`, `cli` or `host`. Some are honest aliases of
      state that does exist (`queue_voice` → `voice_output`; the emergency pair
      → round 8's contacts and `nearest_er`); the rest name state the codebase
      does not keep at all (exercise sessions, sleep tracking, DND windows).
      Implementing the second group means inventing backing state, which is the
      placeholder pattern these rounds exist to remove — so this needs a
      decision: trim the prompt, or build the state. Owner's call.
- [ ] **The scripted `--mock` LLM answers any unrecognised health task with
      `read_sensor temperature`.** `magent run --mock 'check my heart rate'`,
      `'check my glucose'` and `'what is my stress level'` all return a
      temperature reading and "Task completed successfully". Harmless as a test
      double, but it makes `--mock` a false green for prompt tuning: a task the
      real model would map to a health tool tests the temperature path instead.
      A default that admits "no scripted match" would be more honest. Owner's
      call, since it changes mock output that other tests read.

### 🟢 P2 — audit round 10: the first alert of a device's life was swallowed (2026-09-25)
Scope: back to the published library surface. Round 8 made the emergency
protocol *decide*; round 10 asked when it is allowed to. Worked from a scan for
`Option`-shaped state flattened into a `0` sentinel.
- [x] **`last_*_ms == 0` read as "raised at t=0".** All three cooldowns
      (`glucose_alert_cooldown_ms` 5 min, `ecg_alert_cooldown_ms` 1 min,
      `emergency_cooldown_ms` 30 min) were compared against a field that
      started at `0`, so a device that had never raised anything was treated as
      one that had raised it at power-on. Consequence: on a freshly booted
      monitor, no glucose alert for 5 minutes, no ECG alert for 1 minute, and
      no emergency for 30 — a window in which the patient is on their own, and
      precisely the window in which a device coming out of a crash or a
      power-cycle is most likely to be alarming. Fixed with `Option<u32>` and a
      shared `cooldown_elapsed(last, now, ms)`; `None` is always elapsed, the
      backwards-clock case is preserved, and repeats are still rate-limited.
- [x] **The tests were written around the bug, not at it.** Every glucose test
      used `300_001`, `400_000` or `1_800_000`; the round-8 emergency test's own
      comment explains that "a small timestamp is suppressed" as though it were
      intended. A test suite that never probes `t=0` cannot see a boot-window
      bug, and a comment that documents a workaround is a hint that the
      behaviour deserves a second look.
      **The lesson:** a sentinel that doubles as a legitimate value (`0` = both
      "epoch" and "never") is indistinguishable from correct until something
      asks *which* it means. `Option` makes the question unaskable.
- [ ] **`early_warning` still has no in-tree consumer.** Carried from round 8
      and now the oldest substantial gap in the tree: ~1000 lines of tested
      logic (`process_glucose` / `process_ecg` / `process_heart_rate`,
      `predict_glucose_trend`, `nearest_with_specialty`) that nothing calls.
      Wiring it needs a surface, and the honest options all need a decision:
      a CLI config section (`[emergency] contacts/hospitals`) feeding
      `SimulatorExecutor`, an AT command plus a health-monitoring task on the
      ESP32, or a Lua-bridge callback. **Deliberately not stubbed**: this round
      considered seeding the simulator with a fabricated contact/hospital list
      to make the prompt's `notify_emergency_contact` / `find_nearest_hospital`
      "work", and rejected it — a simulated die temperature is harmless, a
      simulated hospital name and phone number is the kind of invented live
      data the ESP32 prompt explicitly forbids. Real configuration or nothing.
- [ ] **The host executor's emergency tools still return `MissingField`.**
      `notify_emergency_contact` and `find_nearest_hospital` are advertised by
      `HEALTH_SYSTEM_PROMPT` and answered by `AgentSimulator` with the
      unknown-tool error, which tells the model the tool does not exist rather
      than that the host has no contacts configured. Worth a distinct
      not-configured error once the config surface above is decided; the error
      text is the only thing that changes in the meantime, so it waits with it.

### 🟢 P2 — audit round 11: the fourth executor was never audited (2026-09-25)
Scope: enumerate every implementation of the tool contract and diff them against
each other rather than against an expectation. There are **four**:
`magent-core/src/tools.rs` (`ToolRegistry`, embedded), `magent-core/src/simulator.rs`
(`AgentSimulator`, what the CLI's `--mock` path actually uses),
`firmware/esp32-app/src/local_tools.rs` (`Esp32ToolHandler`), and
`host/simulator/src/main.rs` (the `magent-simulator` binary). The first three had
been through rounds 6/7/9; the fourth had never been opened.
- [x] **`host/simulator` is independent of `magent-core` and had zero tests.**
      It re-implements `read_sensor`, `write_gpio`, `flash_read`, `flash_write`,
      `ble_send` and the demo loop from scratch, so every fix applied to the
      shared crates left it behind. It now has a first test module whose six
      tests were each written against a reproduced failure.
- [x] **Unknown sensors silently became temperature.** `_ => SensorType::Temperature`
      meant `glucose`, `heart_rate`, `ecg`, `stress` and `battery` all answered
      `23.5°C` with `success: true` — the round-6 bug, in the copy least likely
      to be noticed because its output goes to a demo console. Fixed with a
      `parse_sensor` shared by the read path, which also accepts the alias
      spellings the other executors accept; a missing `sensor` is now an error.
- [x] **GPIO state semantics diverged from the firmware.** `state:"1"` drove the
      pin **low** (only the exact string `"high"` meant high), an omitted state
      defaulted to `low`, and `"mid"` was accepted as low. All three now match
      `Esp32ToolHandler`.
- [x] **A missing `pin`/`address` became `0`.** The round-11 `unwrap_or(0)`
      class: fixed in this copy and extended to the other two that had the
      pattern, so the fix does not itself create a new divergence
      (`required_arg_parse` in `tools.rs`, `required_int_arg` in `simulator.rs`).
      **Lesson:** with N copies of a contract, "fixed" means fixed in N places;
      a scan for the *pattern* (not the bug) is what finds the missed copy.
- [ ] **The other three executors still differ in smaller ways.** This round
      closed the sensor/GPIO/error-semantics gaps; the tool *sets* themselves
      are not identical (`host/simulator` has no `voice_output`,
      `send_notification` or the health aliases) and nothing enforces that the
      four stay in step. A shared contract test, or generating the host
      executor's table from the core registry, would — but the host crate is
      deliberately dependency-free, so this is a design decision, not a patch.
- [ ] **~11 prompt-advertised names still have no implementation** (carried):
      `start_exercise`, `end_exercise`, `get_exercise_goal`, `adjust_exercise_goal`,
      `get_sleep_quality`, `send_health_alert`, `queue_voice`, `send_meditation`,
      `set_dnd`, `notify_emergency_contact`, `find_nearest_hospital`. Also
      carried: the scripted `--mock` LLM answers unrecognized health tasks with
      `read_sensor temperature`; multi-tool-call dropping; `Retry-After`
      ignored; duplicate `call_N` ids.

### 🟢 P2 — audit round 12: a tool call is not the only tool call in a response (2026-09-25)
Scope: the ReAct loop's turn boundary — specifically, what the runner does with
the *shape* of an LLM response rather than with any single tool. Worked from the
published `parse_tool_call` API and from the conversation's id invariants.
- [x] **`tool_calls[1..]` was dropped without a word.** `parse_tool_call_from_json`
      handled the OpenAI `tool_calls` array by taking `.first()`, and
      `parse_tool_call` returned a single `Option` — so a model that batched
      independent reads ("temperature and humidity") had all but the first
      executed, with no error, no trace event, and a history in which the
      un-run calls had never been requested. `parse_tool_calls` now returns
      them all and the loop runs each as its own assistant/tool pair.
      **The lesson:** an API returning `Option<T>` where the protocol is `Vec<T>`
      is a truncation the compiler cannot flag — the type looked like the
      protocol until the protocol had two elements.
- [x] **A REPL turn re-issued the previous turn's `call_id`.** Ids came from
      `tool_call_count`, which `reset_run_state` restarts every turn while the
      history is kept — so turn two's call was `call_1` again, alongside turn
      one's. Fixed with a session-long `call_id_seq`. The consequence had
      already been *mitigated* one layer down: `prune_dangling_tool_pairs`
      matches by position precisely because ids repeat. That mitigation is kept
      (a hand-built or pre-fix history can still repeat an id) but its comment
      no longer states the root cause as a fact of life.
- [ ] **Still carried:** ~11 prompt-advertised names with no implementation
      (`start_exercise`, `end_exercise`, `get_exercise_goal`,
      `adjust_exercise_goal`, `get_sleep_quality`, `send_health_alert`,
      `queue_voice`, `send_meditation`, `set_dnd`, `notify_emergency_contact`,
      `find_nearest_hospital`); `early_warning` has no in-tree consumer; the
      scripted `--mock` LLM answers unrecognized health tasks with
      `read_sensor temperature`; `Retry-After` is ignored on 429/503.

### 🟢 P2 — audit round 13: the server said when to come back, and we did not listen (2026-09-25)
Scope: the retry path, which round 7 gave a budget but not a policy. Worked
from the question "what does the response carry that the client ignores?".
- [x] **`Retry-After` was ignored.** The retry slept `250ms, 500ms` regardless
      of what the server asked for, so a `429` with `Retry-After: 30` burned
      all three attempts in under a second and reported a failure the server
      had already told it how to avoid. Now `retry_delay(attempt, hint)`
      honours the hint, never shortens our own backoff (no busy loop on
      `Retry-After: 0`), and caps it at 30 s since the value is remote.
- [x] **The decision had no seam, so it could not be observed.** The wait *is*
      the behaviour, and a test cannot assert it without either paying it or
      being able to watch it. `send_with_retry` now delegates to a variant with
      an injected sleep, and the mock `Reply` carries headers — so the header
      is proven to survive the wire, not just the pure arithmetic.
      **Lesson:** when the effect under test is a delay, the seam that makes it
      observable is worth as much as the fix.
- [x] **Resolved 2026-09-26 (audit round 25): the `HTTP-date` form is parsed.**
      The "needs the wall clock, so it is untestable" reasoning was wrong —
      the clock is one injectable parameter. `parse_retry_after(value, now)`
      now handles IMF-fixdate, RFC 850 and asctime, with an in-the-past date
      flooring at zero (so it can only extend a wait). See "audit round 25"
      at the top of this file.
- [x] **Resolved (re-verified 2026-09-26, audit round 30).** The 7
      prompt-advertised names all route now — `simulator.rs` has arms for
      `get_sleep_quality`, `send_health_alert`, `queue_voice`, `send_meditation`,
      `set_dnd`, `notify_emergency_contact` and `find_nearest_hospital`, and the
      CLI's `SimulatorExecutor` delegates to that executor, so
      `every_tool_the_prompt_advertises_is_routable` covers the CLI backend too.
      `early_warning`, `sleep_manager` and `voice_notification` are consumed by
      `AgentSimulator` (its `early_warning` / `sleep` / `voice` fields back the
      health tools). And the scripted `--mock` planner's default arm now answers
      "No tool call needed for this task (simulated planner)" instead of running
      an unrelated `read_sensor temperature`; only the explicit
      `health`/`status`/`verify`/`system` keyword branch reads a sensor, which is
      what that keyword asks for.

### 🟢 P2 — audit round 30: a model-chosen flash address panicked the host simulator (2026-09-26)

Scope: the standalone `host/simulator` binary — the round-29 carried item
("its five-tool catalog was never audited against the other executors").
- [x] **Its flash bounds check overflowed.** `FlashStorage::read`/`write`
      guarded with `address + len > self.data.len()`, where `address` is the
      model's `as_u64()` value and `u64::MAX` survives into `usize`. The sum
      wrapped, the check compared a small number and passed, and the slice
      that followed panicked — `attempt to add with overflow` in debug, a
      range panic in release. Verified by a test that panicked at
      `host/simulator/src/main.rs:227`. `flash_read` and `flash_write` both
      reach this; a panicking `Result` is not a failure the agent loop can
      report back to the model.
- [x] **`erase_sector` wrapped twice more** (`sector * sector_size`, then
      `start + sector_size`). It is not reachable from a tool arm today, but
      it is `pub` and carried the same latent crash, so it was hardened in the
      same pass rather than left as a trap.
- [x] **Fixed with the pattern `magent-core` already uses.** `checked_add` /
      `checked_mul` plus a bound filter, matching `magent-core/src/simulator.rs`'s
      `erase_sector`, which documents this exact bug class ("underflowed … a
      panic … even though the `Result` return advertises a clean failure").
      `host/simulator` is a separate crate with its own copy of the simulator
      stack — it never inherited that fix, which is how the two drifted.
- [x] **The other four tools were checked and matched.** `read_sensor` routes
      on the sensor name and rejects unknown/missing ones; `write_gpio`
      accepts `high|1|low|0` with an omitted state defaulting high, matching
      the firmware; `ble_send` sends the payload, not the argument envelope.
      These divergences were fixed in earlier rounds and are consistent now.
- [x] Regression tests, both proven against the old code:
      `an_address_that_overflows_the_bounds_check_is_an_error_not_a_panic`
      (panicked: `attempt to add with overflow`) and
      `addresses_at_and_past_the_end_are_handled_without_a_panic` (the
      non-overflowing far edge — one past the end is refused, the last
      readable block still reads — so the fix cannot become "reject
      everything").
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests
      (477 unit / 44 repl / 1 pty), `./test-workspace.sh`, `magent-simulator`
      tests (8) and a full `magent-simulator` demo run, firmware `cargo check`.
      `host/simulator/src/main.rs` rustfmt diff count unchanged at 19.
- [x] **Corrected a stale carried note.** Round 13's "7 prompt-advertised names
      with no implementation" is resolved: `simulator.rs` routes
      `get_sleep_quality`, `send_health_alert`, `queue_voice`, `send_meditation`,
      `set_dnd`, `notify_emergency_contact` and `find_nearest_hospital`, and the
      CLI's `SimulatorExecutor` delegates to it. The routability test
      (`every_tool_the_prompt_advertises_is_routable`) asserts this for
      `AgentSimulator`, which is what every host executor wraps.
- [ ] **Carried:** `safety::Watchdog` cannot trip (documented deliberate —
      enforcement lives in firmware `rt_watchdog.rs` / nrf52 `watchdog.rs`);
      `MqttAdapter::send` returns `Ok(())` as a documented stub for a
      receive-only adapter; `magent-core/src/simulator.rs`'s flash uses
      `address: u32`, so its `start + len` cannot overflow on the 64-bit host
      targets the tests run on, but it would on a 32-bit target — left as-is
      because no built target exercises it and the firmware uses `nvs_flash`,
      not this simulator.

### 🟢 P2 — audit round 35: `magent web3 tx --data` panicked on a non-ASCII hex argument (2026-09-27)

Scope: the CLI's blockchain command surface (`cli/src/web3_blockchain.rs`) —
the round-34 lesson (slicing a `&str` by computed byte offsets) applied to a
different crate, this time on a user-supplied flag value.
- [x] **`decode_hex` sliced `&hex[i..i + 2]` on a `&str`.** The only guard was
      "even number of digits", which says nothing about char boundaries. When a
      2-byte window straddles a multi-byte character the slice panics. Reachable
      with a plain CLI typo: `magent web3 tx build 0x11… --data '11€x'`
      panicked at `cli/src/web3_blockchain.rs:820` with `end byte index 4 is not
      a char boundary`. `--data` also feeds `web3 vc`/artifact paths (456, 727,
      743), so the same input class reached several commands.
- [x] Blast radius: the CLI installs a panic hook that prints
      `error: internal error (…) — this is a bug in magent, not in your command
      line`. So a user's malformed argument was surfaced as a bug report request
      and took the panic path instead of the normal usage-error path (exit 2).
- [x] Fix: reject non-ASCII up front with a `Usage` error. Hex is ASCII by
      definition, so no valid input changes; the message is now accurate.
- [x] The CLI's *other* `decode_hex` (`cli/src/web3.rs`, for `--seed`/`--pk`)
      already iterated `as_bytes()` and was never exposed. A workspace grep for
      `from_str_radix(&…)` confirms this was the only `&str`-slice instance.
- [x] Verified by `decode_hex_rejects_non_ascii_without_panicking`
      (`cli/src/web3_blockchain.rs`), which panicked at line 820 on the old code.
      It covers the three alignment cases; the valid path stays covered by the
      existing hex tests and an end-to-end `magent web3 tx build … --data
      0xdeadbeef` (prints the transaction, exit 0 — verified this round).
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests
      (9 suites), `magent-lua` piccolo suite (18), `./test-workspace.sh`,
      firmware `cargo check`. `cli/src/web3_blockchain.rs` rustfmt diff count
      unchanged at the round-23 baseline of 0.
- [ ] **Found, not fixed (out of workspace):** `host/magent-man` (a standalone
      Tauri app, not a workspace member and not built by any sweep)
      `src/usb.rs:115-121` truncates a sanitised AT message at 256 bytes on a
      char boundary, but the sanitisation has already expanded `"` → `\"`, so
      truncation can leave a trailing `\` that escapes the closing quote in
      `AT+AGENT="…"`. Needs a 256-byte-boundary message ending with an escaped
      quote. Left alone because nothing in this repo compiles or tests that
      crate, so a fix could not be verified here.

### 🟢 P2 — audit round 41: the ESP32-S3 build, profile and device paths (2026-09-27)

Scope: the ESP32-S3 specifically — its `board-s3` build/config, the `board-s3`-gated
scheduling code, and the S3 memory profile's test coverage. The round-41 baseline
is that the S3 firmware now **compiles**:

```
cargo +esp check --target xtensa-esp32s3-espidf \
    --no-default-features --features board-s3,wifi,uart        # exit 0 (56 s cold)
cargo +esp check --target xtensa-esp32s3-espidf \
    --no-default-features --features board-s3,wifi,uart,lua    # exit 0
```

Neither variant is built by CI (`.github/workflows/ci.yml` builds only the C61),
so this is the first recorded end-to-end compile of the S3 target in this repo.

- [x] **The S3 memory profile's test suite was red, and no sweep ran it.**
      `magent-core`'s
      `agent_runner::compression_tests::compress_now_bounds_byte_footprint`
      hard-coded a `6 × 200 KiB ≈ 1.2 MiB` history and asserted
      `estimate_bytes(..) > MAX_DYNAMIC_CONTEXT_BYTES`. That ceiling is 512 KiB
      on the C61 but **2 MiB under `s3-8mb-psram`**, so under the S3 profile the
      fixture was *under* the budget and the test panicked on its own
      precondition. `test-workspace.sh` only `cargo check`ed the feature and the
      standing sweep uses `--features std`, so the S3 byte-GC bound
      (REQ-SCHED-001 / mem-3) was advertised but never verified. The
      implementation (`conversation::gc_to_budget`) is correct; the coverage was
      not. Fixed by sizing the fixture from the budget under test, adding
      `s3_profile_keeps_a_history_the_c61_would_drop` (the behaviour the raised
      ceiling buys), and adding **step 5b** to `test-workspace.sh`
      (`cargo test -p magent-core --features std,s3-8mb-psram --lib`).
- [x] **The device temperature read reported a value it never obtained.**
      `local_tools.rs` (`read_sensor sensor=temperature`) and `lua_task.rs`
      (`Esp32Hardware::sensor_read`) both discarded
      `temperature_sensor_get_celsius`'s `esp_err_t` — only `install`/`enable`
      fed the variable that becomes `last`/the `Ok`/`Err` decision. A failed
      read therefore answered `success: true` with `celsius` at its `0.0`
      initialiser (`temperature=0.0 C (err=0)`), and the Lua path returned
      `Ok(0.0)`. Fixed by capturing the read's return code; a failure now yields
      `temperature read failed (err=…)` / `Err(..)` with no fabricated value.
- [x] **`core_affinity::apply_profile` could panic despite documenting the
      opposite (S3-only).** Its doc says "Never panics on its own; a failure to
      set the profile is downgraded to a `log::warn!`", but
      `ThreadSpawnConfiguration::set` panics on a priority outside FreeRTOS's
      `[1, configMAX_PRIORITIES - 1]`, and `unwrap_or_else` cannot catch a
      panic (`esp-idf-hal-0.46.2/src/task.rs:450-458`). Latent today (the
      constants are 8/10/15/20 vs `configMAX_PRIORITIES = 25`). Fixed by
      clamping into range before `set()`, with a warning when clamping, so the
      documented fail-open actually holds.
- [x] **The device's `fetch_web` SSRF guard was bypassable by a redirect
      (S3 and C61 firmware alike).** `local_tools::http_get_once` vetted only the
      *initial* host with `magent_core::at_validate::public_fetch_target_refusal`
      — and then built its client with
      `HttpConfig { ..Default::default() }`, whose
      `follow_redirects_policy` defaults to `FollowRedirectsPolicy::FollowGetHead`.
      The esp-idf client therefore followed a GET `302` *inside C*, via
      `esp_http_client_set_redirection`, and the `esp-idf-svc` wrapper exposes no
      post-redirect URL getter at all — so the guard never saw the host actually
      dialled. A model-supplied public URL that 302s to `http://192.168.x.x/`,
      `http://169.254.169.254/` or the device's own web-admin port was read and
      its body echoed back into the transcript, which is precisely the hole the
      guard exists to close. The **host** path (`magent_core::web`) already
      re-validated after redirects (`Policy::limited(5)` +
      `validate_fetch_url(&final_url)`), so the two SSRF paths had silently
      drifted — the same class of drift the guard's own doc warns about.
- Fix: `http_get_once` now pins the connection with
  `FollowRedirectsPolicy::FollowNone` and surfaces the `Location` header;
  `http_get_following_redirects` walks the chain in Rust (`MAX_REDIRECT_HOPS = 3`,
  tighter than the host's 5 because every hop is a fresh DNS/TCP/TLS handshake on
  the Core-1 real-time thread), re-entering `http_get_once` per hop so the guard,
  the DNS/TCP preflight and the 18 s RT-watchdog feeding all re-run on each new
  host. The next URL comes from a **new host-testable helper**,
  `magent_core::at_validate::resolve_http_redirect`, which accepts only the three
  forms that cannot be mis-joined (absolute, protocol-relative, root-relative) and
  returns `None` for anything else — a relative/opaque `Location` is refused
  rather than guessed, so the 3xx is handed back instead of followed blind.
- Tests: `resolve_redirect_accepts_the_three_safe_forms`,
  `resolve_redirect_refuses_what_it_cannot_revet`, and
  `resolved_redirect_targets_are_classified` (the last feeds each resolved hop
  back through `public_fetch_target_refusal` and asserts the internal targets are
  refused) — all in `magent-core`, so they run in the default host sweep.
- Verified: the three new tests pass; `magent-core --features std --lib` 861,
  `--features std,s3-8mb-psram --lib` 862, `--features esp32 --lib` 570 (all
  pass); C61 `cargo +esp check --target riscv32imac-esp-espidf` exit 0; S3
  `cargo +esp check --target xtensa-esp32s3-espidf
  --features board-s3,wifi,uart` and `…,lua` both exit 0; `./test-workspace.sh`
  prints `✅ 全部通过`.
- Verified-failing evidence (old code → new test's failure): the S3-profile run
  of the untouched test above, before the fix —
  `assertion failed: crate::conversation::estimate_bytes(&runner.messages) > crate::MAX_DYNAMIC_CONTEXT_BYTES`
  (the fixture no longer exceeded the S3 ceiling). After the fix the same test
  passes under both `--features std` and `--features std,s3-8mb-psram`.
- Verified: `cargo check` (C61, exit 0) and the two S3 `cargo +esp check`
  variants above (exit 0); `magent-core --features std,s3-8mb-psram --lib` 858
  pass, `--features std --lib` pass, `--features esp32 --lib` pass; full
  `./test-workspace.sh` prints `✅ 全部通过` including the new step 5b.
- [x] **The S3's native-USB ingress link trusted a byte count the HAL can
      return as `usize::MAX` (S3-only).** The vendored
      `.cargo-patches/esp-idf-hal-0.46.2/src/usb_serial.rs` returns
      `Ok(len as _)` from the C `int` in both `read` (l.142-148) and `write`
      (l.158-164) **without mapping the error case**: unlike `UartRxDriver::read`
      (`.cargo-patches/esp-idf-hal-0.46.2/src/uart.rs`, `-1 | 0 => Err(..)`), a
      driver error reaches `UsbSerialAdapter` as a huge `usize`. On the read
      side that made `UsbSerialAdapter::poll` return `Ok(usize::MAX)`, and
      `magent_core::ingress::IngressGateway::ingest` sliced `&scratch[..n]`
      (`ingress.rs:240`) — an out-of-range slice panic on the **Core-1
      real-time ingress thread**, i.e. a chip-reset from a non-blocking console
      read. On the write side `send`'s `offset += n` with `n = usize::MAX`
      ended the loop and returned `Ok(())`, so a reply that was never
      transmitted was reported to the caller as sent. Both are latent (they
      need the driver to return `-1`), but the read path is the one the ingress
      thread runs every poll.
- Fix, defence in depth: `ingest` now accepts `Ok(n)` only when
  `n <= scratch.len()` and otherwise logs + discards the frame (the adapter
  contract already *said* `n <= buf.len()`; the gateway just did not enforce it
  — `communication/link.rs` now spells the violation out for both the sync and
  async trait docs); `UsbSerialAdapter::poll` rejects an over-`buf.len()` count
  as `AdapterError::Io` and `send` rejects `n > buf.len() - offset`, so the
  bad count never reaches the gateway in the first place. USB's legitimate
  `Ok(0)` ("nothing buffered", which `poll` depends on for `NON_BLOCK`) is
  preserved — only counts past the buffer are refused.
- Tests: `overreported_frame_is_discarded_not_panicked` (a rogue adapter that
  reports `usize::MAX` must be skipped and the next adapter's frame still
  returned) and `overreported_count_alone_yields_no_frame` (same violation with
  no healthy adapter behind it is a clean `Ok(None)`) — `ingress_tests.rs`,
  driven by a new `ChainedAdapter::overreport` field (`--features
  std,ingress,mqtt,web3`).
- Verified: `ingress_tests` 13/13 pass; `magent-core --features std --lib` 861
  pass; `cargo clippy -p magent-core --features std,ingress,mqtt,web3
  --all-targets -- -D warnings` clean; S3 `cargo +esp check --target
  xtensa-esp32s3-espidf --features board-s3,wifi,uart` exit 0 (this compiles
  the patched `UsbSerialAdapter`); C61 `cargo +esp check --target
  riscv32imac-esp-espidf --features board-c61,wifi,uart` exit 0; full
  `./test-workspace.sh` prints `✅ 全部通过`.
- [x] **Two threads could swap each other's core/priority while spawning
      (S3-only).** `core_affinity::apply_profile` writes the **process-global**
      `esp_pthread_cfg_t` (`esp_pthread_set_cfg`) and the *next*
      `pthread_create` reads it back — so the "set profile, then spawn" pair is
      only atomic while one thread is doing it. The S3 firmware spawns from two
      threads: the boot path (and later an `at_dispatch` `AT+OTA` / `AT+RST`,
      which run on the ingress thread) and the Lua host thread, which spawns
      its `llm-worker` at an arbitrary moment (`lua_task.rs:653`). Interleaved,
      A could set `REALTIME_AGENT`, B set `IO_NETWORK`, then A's
      `pthread_create` read B's config — silently placing the ReAct agent on
      Core 0 at priority 10 (below lwIP), or a long TLS download (the OTA
      worker) on the Core-1 real-time core. The module doc asserted the
      mechanism was sound ("we set the whole profile immediately before each
      spawn"), which is only true single-threaded. C61 is unaffected: its
      `apply_profile` is a compile-time no-op.
- Fix: a `SPAWN_LOCK` mutex serialises the set-and-spawn pair, and every
  profile-carrying spawn goes through it — `spawn_thread` takes the lock
  around `apply_profile` + `Builder::spawn`, and a new `spawn_with_profile`
  does the same for the three `at_dispatch` sites that spawn on the default
  stack (`AT+RST`, `AT+RESTORE`, `AT+OTA`, which previously called
  `apply_profile` then `std::thread::spawn` by hand). `apply_profile` is now
  module-private so the unprotected pattern cannot be reintroduced.
- Verified: S3 `cargo +esp check --target xtensa-esp32s3-espidf --features
  board-s3,wifi,uart` and `…,lua` both exit 0; C61 `cargo +esp check --target
  riscv32imac-esp-espidf --features board-c61,wifi,uart` exit 0.
- [x] **Nothing validated the pin number before an S3 pin was driven, so a Lua
      script or an LLM tool call could take GPIO26–32 — the SPI0/1 pins to the
      in-package flash (S3-only).** Both pin entry points took the number
      straight from untrusted input: the agent tool `local_tools::write_gpio`
      (`pin` parsed from the model's `pin=` argument, defaulting to 13) and the
      Lua `HardwareBackend` (`lua_task.rs` — `gpio_write`/`gpio_read`/
      `adc_read`/`pwm_set`, `pin: u8` from the script). On the S3,
      `gpio_reset_pin` on GPIO26–32 detaches the SPI0/1 function from the flash,
      and the next cache miss faults — a `StoreProhibited` on whichever core
      fetches first (usually Core 0 running Wi-Fi/lwIP), with the actual cause
      on the thread that touched the pin. GPIO19/20 (USB-Serial/JTAG) and
      GPIO10/11 (UART0) are the two ingress links and GPIO4/5 the Lua I2C0 bus,
      so driving those is a self-inflicted version of the same outage. This is
      the *class* of mistake that already forced the Lua I2C bus off GPIO8/9
      (`lua_task.rs` comment), left unfixed at the GPIO layer.
- Fix: one `local_tools::reserved_pin_reason(pin: i32) -> Option<&'static str>`
  (`#[cfg(feature = "board-s3")]`; a `None` fallback everywhere else) maps the
  owned pins — `26..=32` flash, `19 | 20` USB, `10 | 11` UART0, `4 | 5` I2C0 —
  to a refusal reason, consulted by both call sites: `write_gpio` returns a
  failed `ToolResult` (`refusing GPIO{pin}: …`) and each Lua binding returns
  `Err(..)`. Out-of-range / nonexistent numbers (the S3 has no GPIO22–25) are
  still left to ESP-IDF, which already returns `ESP_ERR_INVALID_ARG`.
- Verification note: this closes a real hole and is a **candidate** contributor
  to the reported Core-0 `StoreProhibited` (a pin-driven flash fault surfaces on
  Core 0), but it is **not proven** to be that crash's root cause — the Lua-VM
  Core-0 crash is still tracked separately (see the `docs/BACKLOG.md` entry for
  the S3 Lua VM and `docs/LUA_SCRIPTING_S3.md`). The guard removes one way to
  reach that fault; it does not claim to be the cause.
- Verified: S3 `cargo +esp check --target xtensa-esp32s3-espidf --features
  board-s3,wifi,uart` and `…,board-s3,wifi,uart,lua` (the latter compiles every
  guarded Lua binding) both exit 0; C61 `cargo +esp check --target
  riscv32imac-esp-espidf --features board-c61,wifi,uart` exit 0 (compiles the
  `None` fallback).
- Carried (unchanged, judged below the bar for a fix this round):
  - The **C61** reserved-pin map is not encoded: the C61 is single-core with its
    own flash/PSRAM pin assignment, guessed nothing rather than guessed wrong.
    `reserved_pin_reason` returns `None` there; the S3 map should have a C61
    sibling before the C61 Lua/GPIO paths are trusted with untrusted pin
    numbers.
  - [x] **GPIO33–37 follow the memory profile (2026-11-08).** They are the
    octal-PSRAM pins on the 8 MB N8R8 module and plain GPIOs on the 2 MB quad
    dev board. `reserved_pin_reason` now refuses them under
    `#[cfg(feature = "s3-8mb-psram")]`, so `check-s3.sh --octal8mb` /
    `build-s3.sh --octal8mb` get the guard while the quad build does not.
  - The `usb_serial.rs` narrowing itself is left as-is: fixing the vendored
    `read`/`write` to map negatives to `Err` is the vendored-crate owner's call
    (it changes exported behaviour for every `UsbSerialDriver` user), and USB's
    `Ok(0)` "no data" must stay `Ok`. The gateway + adapter guards above are the
    in-repo defence, and they hold regardless.
  - [x] **`UsbSerialAdapter::is_connected` reports the real state (2026-11-08).**
    It now calls `UsbSerialDriver::is_connected()` (→
    `usb_serial_jtag_is_connected`) instead of returning `true`; the ingress
    gateway's skip-disconnected logic and diagnostics are therefore honest.
  - [x] **`configure_psram_thread_stacks()` → `log_stack_allocation_policy()`
    (2026-11-08).** Behaviour unchanged (stacks stay in internal DRAM); the
    name now matches the no-op, which only logs the policy.
  - [x] **Board/profile pairing reconciled (2026-11-08).** `board-s3` no longer
    enables `magent-core/s3-8mb-psram`; the firmware has a separate
    `s3-8mb-psram` feature for the N8R8 8 MB octal module, which also uses
    `sdkconfig.s3.defaults`. The shipped quad-PSRAM 2 MB board therefore keeps
    the default 1 MiB / 512 KiB ceilings, correct for its physical PSRAM.
    `BOARDS.md`, `README.md`, `docs/ESP32_C61_BUILD.md`,
    `docs/DUAL_CORE_SCHEDULING.md` and `docs/LUA_SCRIPTING_S3.md` updated.

### 🟢 P2 — audit round 40: simulated hardware lied about what it did (2026-09-27)

Scope: the less-audited host/HAL crates — `host/nrf52-simulator` and
`magent-hal`'s nRF52 simulation. Two defects of the same shape: a `pub` method
that advertises a computation and returns something else.

- [x] **`StepCounter::tick` could never count more than one step.**
      `host/nrf52-simulator/src/lib.rs:496` (round-40 numbering) read
      `if z > 10.5 && self.steps.is_multiple_of(2) { self.steps += 1; }`. After
      the first increment `steps == 1`, so `is_multiple_of(2)` is false forever:
      the counter latches at 1 for *any* input sequence. The type is advertised
      (the demo prints `Steps`, `read_sensor sensor=steps` reads it, the
      "Track my steps" scenario exists), so the value a caller reads is a
      constant dressed as a count. Fixed with rising-edge detection (a new
      `above_threshold` field): a footfall counts once, a sustained
      above-threshold reading is not re-counted, successive footfalls keep
      counting. `add_steps` now saturates.
- [x] **The accelerometer never produced the spike the counter thresholds
      on.** `Accelerometer::tick` wrote only resting noise `9.8 ± 0.2`, so
      `SmartwatchSimulator::tick` — the documented way to advance the sensors —
      never fed the counter a value over 10.5. Even with the guard fixed, step
      counting stayed at 0 end-to-end. The accelerometer now models a walking
      wearer: once per stride (1000 ticks ≈ 1 simulated second) the vertical
      axis spikes above the threshold and settles back between steps.
- [x] **`RtcTime::tick` rolled `seconds` over one tick late.**
      `magent-hal/src/nrf52/sim.rs:333` compared `fetch_add`'s *pre*-increment
      value against the boundary: `if ticks >= 32768 { … }`, which fires on the
      32 769th tick, not the 32 768th the doc comment promises. Every second of
      uptime was reported one tick late. Fixed by comparing against `32767`.
- Verified-failing regression tests (old code → new test's failure):
  - `step_counter_counts_each_footfall`:
    `assertion left == right failed: left: 1, right: 3`.
  - `step_counter_does_not_double_count_a_sustained_reading`:
    `left: 1, right: 2`.
  - `simulator_tick_advances_the_step_counter`:
    `expected steps to accumulate over 3 s, got 0`.
  - `rtc_rolls_seconds_over_on_the_32768th_tick`:
    `assertion left == right failed: the 32 768th tick completes a second,
    left: 0, right: 1`.
- Verified: full sweep green — `nrf52-simulator` 43 lib tests, `magent-hal` 34
  lib tests; `magent-core` `--features std --lib` 858, `--features esp32 --lib`
  567, `--features std --tests` all suites pass; CLI `--features
  web3,web3_app,email-tools,blockchain` 479 lib + 44 + 12 + 5 + 4 + 2 + 1 + 1
  pass; `magent-lua` under both engines (`mlua` lib 16 + 41/3/7/23 integration;
  `piccolo` lib 15, apps 22, engine_consistency 3, format probes 7+13, 19); CLI
  e2e `magent web3 chain list` exit 0; workspace `cargo clippy --all-targets --
  -D warnings` clean and CLI-feature clippy clean; `./test-workspace.sh` prints
  `✅ 全部通过` (exit 0, feature matrix 16/16); firmware `cargo check` exit 0
  (one pre-existing dead-code warning on `AdapterError::Truncated` only);
  rustfmt `Diff in` counts did not increase (`host/nrf52-simulator/src/lib.rs`
  97 → 93, `magent-hal/src/nrf52/sim.rs` 0 → 0).
- Carried (unchanged): `host/nrf52-simulator`'s `NetworkProcessor` hard-codes
  `connected: true` with no setter, making the `"No network connection"` branch
  in `execute_tool` dead (documented simulation surface); `magent-hal`
  `BleController::receive` (sim.rs:929) counts the injected bytes but stores no
  payload, so a test driver cannot read back what it "received" — a `last_rx`
  field would be a feature addition with no current consumer.

### 🟢 P2 — audit round 39: `transaction_hash` returned an all-zero hash (2026-09-27)

Scope: `magent-core/src/web3/blockchain/transaction.rs` — the carried round-38
candidate "`TransactionRequest::transaction_hash` still returns `Hash::zero()`".
The round-38 concern was that the method returns a bare `Hash`, so an
unencodable input has no error channel; on inspection the *actual* defect is
simpler and does not need that decision.

- [x] **`pub fn transaction_hash(&self, _signature: &[u8; 65]) -> Hash` ignored
      both its receiver and its argument and returned `Hash::zero()`.** The
      body was `// In a real implementation, this would compute the keccak256
      hash of the signed transaction encoding` + `Hash::zero()`. The doc line
      said "Calculate the transaction hash (for display purposes only)". A
      caller that displayed it would print an all-zero hash — a constant dressed
      up as a computed value, the same fake-success class as the round-36
      always-`Ok` validator.
- [x] **The error-contract question resolves itself: there is no unencodable
      input.** The signature is a fixed `&[u8; 65]` and every field of the
      request is already a concrete value, so the only failure mode a `Result`
      would carry is an arithmetic overflow of the EIP-155 `v`. That is
      `saturating_mul`/`saturating_add` on `u64`, which no real chain id can
      reach (`chain_id > 2^63`). `Hash` is therefore an adequate return type;
      no API change.
- [x] Fix: reconstruct the signed encoding from the signature and return its
      keccak256. The signature's `v` byte is the canonical `recid + 27`, so a
      legacy request recomputes the on-chain value as
      `chain_id * 2 + 35 + y_parity` and a typed request uses the raw
      `y_parity` — the same split `TransactionBuilder::sign` makes, reusing the
      round-38 `encode_legacy_signed` / `encode_typed_signed` / RLP helpers so
      the two paths cannot drift. Gated `#[cfg(feature = "web3")]` to match
      `sign` and the `sha3` import; `blockchain` implies `web3`, so no
      feature configuration loses the method.
- [x] Verified by `transaction_hash_matches_the_signed_encoding`
      (`transaction.rs`), which was proven to fail against the old body: it
      signs a chain-137 legacy transaction and an EIP-1559 transaction, asserts
      the method's result is `!= Hash::zero()`, and asserts it equals
      `SignedTransaction.hash` (the keccak256 of the real signed encoding) for
      both. Old code: `assertion left != right failed: a real signature must not
      hash to zero`, `left`/`right` both `Hash([0, …])`.
- [x] Verified: full sweep green — magent-core `std` lib (858), `esp32` lib
      (567), `std,blockchain` lib (1122), `std` integration tests, workspace
      clippy `--all-targets -D warnings`, `magent --features
      web3,web3_app,email-tools,blockchain` clippy and tests (479 lib + all
      integration targets), `magent-lua` both engines, `./test-workspace.sh`,
      firmware `cargo check` (0, one pre-existing `link_adapters.rs:79`
      warning). rustfmt diff count: `transaction.rs` 0 (= HEAD 0).
- [ ] Carried (unchanged): `magent-core/src/security.rs:65-70,338-350` AES-GCM
      nonce from a per-process counter starting at 0 with a hardcoded key (no
      production caller; the module self-documents its simulation key and its
      "not a security boundary" status); `magent-core/src/web3/blockchain/
      esp32_http.rs` `connect()` setting `connected = true` with no I/O (the
      real work is in the injected `Transport`, so this is architectural, not a
      fake success); `host/lua-app/src/hardware.rs` `i2c_read`'s
      `reg.wrapping_add(off)` folding at the 8-bit register-space boundary for a
      legal 256-byte read (matches the mlua engine's accepted behaviour);
      `firmware/esp32-app/src/lua_task.rs` `power_set` returning `Ok(())` as a
      documented platform no-op;
      `host/magent-man/src-tauri/src/usb.rs:115-121` escape-pair truncation
      (crate not in the workspace).

### 🟢 P2 — audit round 38: signed transactions were not submittable (2026-09-27)

Scope: `magent-core/src/web3/blockchain/transaction.rs` and
`secp256k1.rs` — the carried candidate list's "EIP-155 `v` truncates for
`chain_id > 110`", which on inspection turned out to be one symptom of a
broader defect: the module never built a real signed transaction.

- [x] **`sign()` glued the 65 signature bytes onto the signing payload.**
      `raw = encode_for_signing(); raw.extend_from_slice(sig.as_bytes())`. A
      *legacy* transaction is a plain RLP list `[nonce, gasPrice, gas, to,
      value, data, v, r, s]`, so appending 65 bytes produces something no node
      will decode. Only EIP-2718 typed transactions have a
      `type || fields || signature` shape, and even there the appended `v` was
      wrong (next bullet).
- [x] **The legacy signing payload had ten items, not nine.** EIP-155 signs
      `rlp([nonce, gasprice, gaslimit, to, value, data, chainid, 0, 0])`; the
      code emitted `… chainid, 0, 0, 0`. The signed-over hash was therefore not
      the EIP-155 hash. (Found while decoding the payload with a test-side RLP
      reader, which asserts the item count.)
- [x] **EIP-155 `v` truncated to a byte** (`secp256k1.rs:529`):
      `sig.bytes[64] = (chain_id * 2 + 35 + y_parity) as u8`. Polygon 137 →
      309 → `53`. `EthereumSignature` is `[u8; 65]`, so the value cannot live in
      the signature — the fix has to return it separately.
- [x] **Typed transactions carried `recid + 27` instead of `y_parity`.**
      `EthereumSignature` stores `recid + 27` (needed by `recover_from`), but an
      EIP-2930/1559 signature slot holds the raw `y_parity` 0/1. So
      `SignedTransaction.v` (computed as `recid - 27` = 0/1) disagreed with the
      bytes on the wire (27/28). The existing test allowed all four values,
      which is why it never caught this.
- [x] **`r`/`s` were not RLP-encoded as integers.** They are fixed-width 32-byte
      values, but RLP integers must strip leading zero bytes; a signature whose
      top byte is zero was encoded as a 32-byte string, changing the hash.
- [x] Fix (all four): `sign_legacy_eip155` returns
      `(EthereumSignature, u64)` (signature keeps `recid + 27`; full-width `v`
      returned as `u64`, overflow → `BlockchainError`); new
      `encode_legacy_signed` / `encode_typed_signed` emit the real signed forms;
      `typed_unsigned_items` is the single field-list source shared by the
      unsigned and signed encoders; `rlp_encode_u256_be` strips leading zeros.
      `SignedTransaction.hash` is now `keccak256(raw_transaction)` rather than
      the signing payload's hash.
- [x] **API change**: `TransactionSigner::sign_legacy_eip155` is `pub` and its
      return type changes. Its only in-repo caller is `transaction.rs:459`
      (plus its test); nothing in `cli/`, `host/` or the firmware calls it, so
      the blast radius is the crate's own module. This is unavoidable — the
      EIP-155 `v` physically does not fit in the returned signature.
- [x] Verified by `sign_legacy_keeps_v_full_width_above_chain_id_110`
      (old code: `expected EIP-155 v for chain 137, got 53`) and the tightened
      `test_transaction_builder_sign_eip1559_y_parity_is_zero_or_one`
      (old code: signed list decoded to 9 items, needs 12), plus a reworked
      `test_transaction_builder_sign_legacy_eip155` that recovers the signer
      from the encoded `r`/`s`/`v`. Assertions run on a list decoded by a
      test-local RLP reader, not on a re-derivation through the code under test.
- [x] Verified: full sweep green — magent-core `std` lib (858), `esp32` lib
      (567), `std,blockchain` lib (1121), `std` integration tests, core clippy
      `-D warnings`, `magent --features web3,web3_app,email-tools,blockchain`
      clippy and tests, `magent-lua` both engines, `./test-workspace.sh`,
      firmware `cargo check` (0, one pre-existing `link_adapters.rs:79` warning),
      CLI e2e (`magent web3 chain list`, exit 0). rustfmt diff counts:
      `transaction.rs` 0 (= HEAD 0), `secp256k1.rs` 0 (= HEAD 0).
- [x] **Resolved (audit round 39, 2026-09-27).**
      `TransactionRequest::transaction_hash` now returns the keccak256 of the
      signed encoding instead of `Hash::zero()`. The "bare `Hash`, no error
      contract" worry was moot: the input is a fixed-size `&[u8; 65]` and the
      only arithmetic (the EIP-155 `v`) saturates, so there is no unencodable
      input to report. See the round-39 section above.
- [ ] Carried (unchanged): `magent-core/src/security.rs:65-70,338-350` AES-GCM
      nonce from a per-process counter starting at 0 with a hardcoded key (no
      production caller); `host/lua-app/src/hardware.rs` `i2c_read`'s
      `reg.wrapping_add(off)` still folds at the 8-bit register-space boundary
      for a legal 256-byte read (matches the mlua engine's accepted behaviour);
      `host/magent-man/src-tauri/src/usb.rs:115-121` escape-pair truncation
      (crate not in the workspace).

### 🟢 P2 — audit round 37: `Erc721ApprovalForAll::from_event` panicked on short event data (2026-09-27)

Scope: `magent-core/src/web3/blockchain/events.rs` — back to the round-33/35
panic class, but this time the untrusted input is an on-chain log rather than a
CLI flag or a script argument.
- [x] **An event parser indexed a fixed offset without bounding the buffer.**
      `Erc721ApprovalForAll::from_event` read `event.data[31]` behind only
      `!event.data.is_empty()`, so any 1–31 byte `data` panicked —
      `index out of bounds: the len is 1 but the index is 31`, at
      `events.rs:402` on the pre-fix code. The fn is `pub`, returns
      `Result<Self, Web3ErrorKind>`, and is re-exported from `web3::blockchain`:
      a caller that handles `Err` still dies.
- [x] Blast radius: the input is a log returned by an RPC node / produced by an
      arbitrary contract — external, attacker-influenceable, and needing no bad
      input from the agent. `magent-core` denies `clippy::panic_in_result_fn`,
      but indexing falls under `indexing_slicing`, which is not enabled, so this
      compiled clean.
- [x] Root cause is an inconsistency: this was the only one of the four
      `from_event` parsers that touched raw bytes. `Erc20Transfer`,
      `Erc20Approval` and `Erc721Transfer` all route through the guarded
      `Event::{topic_as_address,topic_as_u256,data_as_u256,data_as_*}` helpers,
      each returning `None` on short input. The guard simply wasn't applied.
- [x] Fix: add `Event::data_as_bool()` (the 4th `data_as_*` helper, same 32-byte
      guard, `Some(data[31] != 0)`) and map its `None` to a `BlockchainError`.
      Empty data — which the old code silently coerced to `false` — is now
      reported as malformed, matching the other parsers rather than guessing
      "not approved" from a truncated log.
- [x] Verified by `erc721_approval_for_all_rejects_short_data_instead_of_panicking`
      (same file), which panicked at `events.rs:402` on the old code. It loops
      every length 1..32, covers the previously-silent empty case, and decodes a
      full word both ways.
- [x] Class sweep (this round's discipline): grepped the whole `web3` module for
      unguarded indexing. Every other site is bounded —
      `topic_as_address`/`data_as_address` (`len() < 32`),
      `parse_u256_be`/`parse_u64_be` (`saturating_sub`), `Address::from_hex` /
      `from_checksummed_hex` (`len() != 40` before `copy_from_slice(&bytes[..20])`),
      `Hash::from_hex` (`bytes[..32]`), `EventFilter::with_topic` (`index < 4`),
      `transaction.rs` RLP length-prefix reads (`bytes.len() == 1 && bytes[0]`),
      `uuid_v4` (fixed-size array). No other instance of this class.
- [x] Verified: full sweep green — magent-core `std` lib (858), `esp32` lib
      (567), `std,blockchain` lib (1120), `std` integration tests, core clippy
      `-D warnings`, `magent --features web3,web3_app,email-tools,blockchain`
      clippy and tests (9 suites), `magent-lua` both engines, `./test-workspace.sh`,
      firmware `cargo check` (0, one pre-existing `link_adapters.rs:79` warning),
      and a CLI e2e (`magent web3 chain info 137`, exit 0). rustfmt diff count
      for `events.rs` 0 (= HEAD 0).

### 🟢 P2 — audit round 36: two validators that could not reject anything (2026-09-27)

Scope: after three rounds of panic-class bugs, this round went looking for the
other half of 「真实实现功能」 — code that reports success without doing the work.
Two instances, in different crates, both of the same shape: a function that
checks for the wrong thing and therefore always passes.

- [x] **`CredentialSchema::validate` tested `required` for the wrong JSON
      shape** (`magent-core/src/web3/verifiable_credentials.rs:483`). JSON Schema
      defines `required` as an *array of strings*; the code was
      `if let Some(Value::Object(map)) = self.schema.get("required")`, which no
      spec-compliant schema can match. The loop body was dead code and the
      function returned `Ok(())` unconditionally — a credential missing every
      required claim validated as conformant.
- [x] Reachability: `pub` API, re-exported from `web3::mod`, compiled into the
      CLI's blockchain build (`blockchain` → `verifiable_credentials`). No caller
      exists *yet* — the CLI's `web3 vc verify` uses
      `VerifiableCredential::validate` (a different function, at lines 122/375,
      which is correct). Fixed anyway: an advertised capability that cannot work
      is exactly the "mock path" this series is chartered to remove, and the
      always-`Ok` result is the dangerous direction for a security-adjacent
      validator.
- [x] Fix: read `required` with `as_array`; resolve `id` to the subject's DID
      (a dedicated field, not a claim) and any other name to `subject.claims`;
      treat a present-but-malformed `required` as an error rather than skipping
      it (skipping is how the always-`Ok` behaviour arose). All `"required"`
      fixtures in the workspace (`host/mqtt-mcp/src/tools/*`) are already arrays,
      so the dead object form had no dependants.
- [x] Verified by `schema_validate_enforces_required_fields`
      (same file), which returned `Ok` on the old code. It pins rejection by
      field name, the conformant case, the `id`→DID resolution (including an
      empty subject DID), and the malformed-`required` error.
- [x] **`SimHardware::i2c_write` accepted a payload longer than the 8-bit
      register space** (`host/lua-app/src/hardware.rs:225`). Registers are keyed
      by `reg.wrapping_add(off as u8)`, so offsets ≥ 256 alias registers already
      written: a 300-byte script write silently corrupts the first 44 bytes and
      reports success.
- [x] Reachability: the `mlua` engine caps the payload at `MAX_I2C_LEN` (256) in
      its binding, but the **`piccolo`** binding had no cap — and piccolo is what
      the ESP32-S3 firmware runs on-device (`magent_lua::piccolo_vm::PiccoloVm`).
      The two engines disagreeing about the same script is the real defect; the
      unprotected path is the one that ships on the device.
- [x] Fix: enforce the bound on the shared backend instead of in one engine's
      binding, via a new `hardware::MAX_I2C_WRITE_LEN` (mirroring the existing
      `MAX_I2C_READ_LEN`). Covers both engines and any Rust caller, and keeps the
      `HardwareBackend` contract (script fault → `Err`, never silent corruption).
- [x] Verified by `piccolo_vm_rejects_an_oversized_i2c_write`
      (`host/lua-app/tests/piccolo_tests.rs`), which returned `Ok` on the old
      code, plus a 256-byte boundary case so the check cannot reject valid
      payloads.
- [x] Verified: full sweep green — magent-core `std` lib (858), `esp32` lib
      (567), `std,web3,verifiable_credentials` lib (1119), `std` integration
      tests, workspace clippy, `magent --features
      web3,web3_app,email-tools,blockchain` clippy and tests (9 suites),
      `magent-lua` under both engines (piccolo 19, mlua 16+41+22+23+… suites),
      `./test-workspace.sh` (feature matrix incl. `verifiable_credentials`),
      firmware `cargo check` (0, one pre-existing `link_adapters.rs:79` warning),
      and a CLI e2e (`magent web3 chain list`, exit 0). rustfmt diff counts:
      `verifiable_credentials.rs` 0 (= HEAD 0), `hardware.rs` 0,
      `piccolo_tests.rs` 2 (= round-34 baseline).
- [x] Date note: the two `required`-object-form dependants found by grep
      (`magent-man` Tauri JSON schemas) are generated files, not this schema
      type, and `host/magent-man` is not a workspace member — not affected.

### 🟢 P2 — audit round 34: a Lua pattern with an unterminated `(` panicked the VM (2026-09-26)

Scope: the piccolo `string.*` shims in `host/lua-app/src/piccolo_vm.rs` — the
pure-Rust engine the ESP32-S3 runs, and the layer round 33 had just been
working in (so its hand-written parsing deserved the same reachability
scrutiny as the bindings).
- [x] **`string.match` sliced a reversed range for an unterminated capture.**
      The shim scans `(`…`)` with a depth counter, then does
      `&pat[group_start..i - 1]`. If the `(` is the last thing in the pattern
      the scan loop never runs, so `i == group_start` and `i - 1 < group_start`
      — `&pat[1..0]`, a panic. The doc comment promises `None` for unsupported
      patterns; Lua 5.4 raises "unfinished capture". Neither is a panic.
- [x] Reachable from a script: `string.match` is the canonical way the shipped
      Lua apps read an agent reply (`string.match(action,
      "SET_COOLING_PULSE:(%d+)")`), so a typo or a data-built pattern aborts the
      app. On the ESP32-S3 piccolo is the on-device engine, so it is not a
      caught test failure but a device reset.
- [x] Fix: refuse the pattern before slicing
      (`if depth != 0 { return None; }`), and use plain `i - 1` for the body now
      that the group is known to be closed.
- [x] Verified by `piccolo_string_match_tolerates_an_unterminated_capture`
      (`host/lua-app/tests/piccolo_tests.rs`), which panicked at
      `host/lua-app/src/piccolo_vm.rs:851` (`slice index starts at 1 but ends
      at 0`) on the old code. It also asserts a closed capture still returns its
      group and an unbalanced nested group is refused, so the guard is not
      "reject everything".
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests,
      `magent-lua` tests under both engines (mlua suite; piccolo suite 18 now),
      `magent-lua` clippy under both engines, `./test-workspace.sh`, firmware
      `cargo check`, and the `demo` (mlua) + `piccolo_datalogger_smoke`
      examples. rustfmt diff counts unchanged (`piccolo_vm.rs` 1,
      `piccolo_tests.rs` 2).
- [ ] **Carried:** the shim's other limits are by design and documented —
      only the first capture group is surfaced, and `%b`/`%f`/character
      classes beyond the listed set are unsupported (they return `None`, not a
      panic). Left as-is; the apps do not use them.

### 🟢 P2 — audit round 33: a Lua `hardware.*` read length could abort the host (2026-09-26)

Scope: the `hardware.*` bindings in `host/lua-app` — the point where a
script-supplied integer crosses into a length-typed parameter. Round 30/32
hardened every *address* an untrusted caller could supply; the *length* on the
same calls was never checked.
- [x] **The length went straight into a `Vec` allocation.** `flash_read(addr,
      len)` / `i2c_read(addr, reg, len)` reach a backend that allocates
      `vec![0u8; len]` (`SimHardware::flash_read`, the S3 firmware's
      `i2c_read_device`) or `Vec::with_capacity(len)` (`SimHardware::i2c_read`).
      An out-of-range `len` is a process abort, not the `Err` the
      `HardwareBackend` trait and the crate docs advertise — the opposite of
      the "never a panic" / `#![deny(clippy::panic_in_result_fn)]` guarantee.
- [x] **piccolo engine** (`piccolo_vm.rs`, shared by the host and the ESP32-S3
      firmware): the arg was read as `i64` and cast with `as usize`, so
      `hardware.flash_read(0x100, -1)` became `usize::MAX` and
      `vec![0u8; usize::MAX]` hit a capacity overflow. Both read callbacks now
      go through a `pop_read_len` helper that rejects a negative or oversized
      length before the lock is taken. Verified by a test that panicked with
      `capacity overflow` at `alloc/src/raw_vec/mod.rs:28` on the old code.
- [x] **mlua engine** (`vm.rs`, the host default): `mlua`'s `usize` conversion
      rejects negative integers, but accepts any positive one, so
      `hardware.flash_read(0x100, 9223372036854775807)` reached the allocator
      and aborted (`memory allocation of 9223372036854775807 bytes failed`,
      `SIGABRT`). Both callbacks now share a `check_read_len` guard. Verified by
      a test that aborted the test process on the old code.
- [x] Both engines enforce the same bound: `hardware::MAX_FLASH_READ_LEN`
      (4096 — one flash sector) and `MAX_I2C_READ_LEN` (256 — the bound the I2C
      write path already used). Each regression test also asserts that
      in-range reads still succeed, so the cap cannot quietly reject
      legitimate use.
- [x] The S3 firmware inherits the fix through `magent_lua::piccolo_vm` and was
      confirmed by `cargo check`. Its NVS `flash_read` was already bounded
      (`len.min(required)`); the unbounded `i2c_read_device` buffer was the
      remaining device-side exposure.
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests,
      `magent-lua` tests under both engines (16/41/22/… + 17 piccolo),
      `magent-lua` clippy under both engines, `./test-workspace.sh`, firmware
      `cargo check`, and the `demo` (mlua) + `piccolo_datalogger_smoke`
      examples run end-to-end. rustfmt diff counts on all five touched files
      unchanged (`piccolo_vm.rs` 1, `piccolo_tests.rs` 2, the rest 0).
- [ ] **Carried:** `hardware.flash_write(addr, data)` and the `i2c_write`
      payload are bounded by the *script's own string length*, so they cannot
      drive a huge allocation — but they are not checked against the device
      size either; the backend's own bounds are the only guard there. Not a
      panic risk, so left as-is.

### 🟢 P2 — audit round 32: the nRF52 simulators' flash bounds check could be wrapped (2026-09-26)

Scope: `host/nrf52-simulator` and `magent-hal/src/nrf52/sim.rs` — the round-30
carried item ("`host/simulator` was one copy; the other two were never
checked"). Round 30 found that `SimulatedFlash`'s bound check (`addr + len`)
wraps for a model-supplied address; that fixed `host/simulator`. These are the
two remaining copies of the same type.
- [x] **`host/nrf52-simulator`'s copy had the identical check and is
      reachable.** `flash_read`/`flash_write` take `address` from the tool
      argument via `parse::<usize>()` and call `SimulatedFlash::read`/`write`,
      so `{"address": "18446744073709551615"}` produced `addr + 64` → wrapped
      (`63`, a legal index) → panic on the slice, in a method whose
      `Result<(), String>` is exactly what the tool layer turns into an error
      string. Verified by a test that panicked at
      `host/nrf52-simulator/src/lib.rs:362` with `attempt to add with
      overflow`.
- [x] **`magent-hal`'s chip-faithful copy had it three times.** `read`/`write`
      shared the `address + len` check; `erase` computed `sector * sector_size`
      and then `start + sector_size`, either of which can wrap and let an
      out-of-range sector pass. All three are now
      `checked_add`/`checked_mul`/`saturating_add`, matching
      `magent-core/src/simulator.rs`, `magent-hal/src/esp32.rs` and the
      round-30 fix in `host/simulator` — every library/host copy of the type
      now agrees. The only `address + len` checks left are in
      `magent-core/tests/` fixtures, which no shipped path feeds an address
      into. Verified by a test that panicked at
      `magent-hal/src/nrf52/sim.rs:674` with `attempt to add with overflow`.
- [x] Regression tests added to both crates, each also asserting the last
      readable block still reads and writes (so the guard cannot become
      "reject everything"). `host/nrf52-simulator` exercises the tool path
      (`execute_tool("flash_read"|"flash_write", …)`), not just the type.
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests
      (478 + the other bins), `magent-hal` (33 + 1) and `nrf52-simulator`
      (40 + 1) tests, `./test-workspace.sh`, firmware `cargo check`, and the
      `nrf52-simulator` demo binary run. `host/nrf52-simulator/src/lib.rs`
      rustfmt diff count unchanged at 97; `magent-hal/src/nrf52/sim.rs` at 0.
- [ ] **Carried:** `simulation::Trng::next_range(min, max)` computes
      `max - min + 1`, which underflows for `min > max` and overflows for
      `max == u32::MAX`. It has no caller anywhere in the workspace and is
      `pub` only as part of the simulator's surface, so it was left as-is this
      round rather than changing an API nothing depends on.

### 🟢 P2 — audit round 31: a restricted cron month skipped to the wrong day (2026-09-26)

Scope: `cli/src/scheduler.rs`'s hand-rolled cron matcher — the round-30 carried
area ("`next_after` has no coverage of the month/day rollover arithmetic").
- [x] **`next_after` carried the day-of-month across a month jump.**
      `CronSpec::next_after` implements the restricted-month case by calling
      `advance_to_next_month`, which only rolls `month` (and `year`) over — it
      leaves `day`, `hour`, `minute`, `second` at whatever the previous
      candidate had. The search then resumed at *the same day-of-month and
      time-of-day* in the month it landed on, which is wrong in two ways:
  - [x] **The earlier part of the target month was skipped.** `0 0 * 4 *`
        ("midnight every day in April") from `2026-01-15T06:00:00Z` returned
        `2026-04-16T00:00:00Z` (`1776297600`) — the first half of April never
        fired, and the `dow` carried from January no longer described the date
        it reported.
  - [x] **A day absent from the target month marched the search a year
        forward.** `0 0 * 2 *` from `2026-01-31T12:00:00Z` produced `Feb 31`;
        the day rollover rejected it, so the month loop walked through March…
        December, into the next year, and returned `2027-02-01T00:00:00Z`
        (`1801440000`) — a nightly task that came back a year late.
  - [x] **Fixed** by resetting the jump target to the first moment of the new
        month (`day = 1`, `00:00:00`) and re-deriving `dow` from the new date
        via `day_of_week`, which is what "advance to the next listed month"
        has to mean. `dow` is maintained incrementally on every other path
        (day/hour rollover, `apply_offset`, `rewind_to_prev_day`), so the one
        path that recomputes the date must recompute it too.
- [x] **Checked the rest of the matcher and found it correct.** The dom/dow
      POSIX OR rule was already fixed (2026-09-22): `0 0 1 * 1` fires on the 1st
      *or* a Monday, and `*` in one field pins the other. Hour/minute jumps use
      `.find(|&&x| x > tm.x)` on the sorted field vectors, which is monotone
      and so cannot loop; `normalise_tm`, `is_leap`, `days_in_month` and
      `local_to_epoch` agree with `epoch_to_local` (round-trip test present).
- [x] Regression test
      `scheduler::tests::cron_month_skip_lands_on_the_first_of_the_target_month`,
      proven against the old code: the April assertion returned `1776297600`
      (April 16th) and the February assertion returned `1801440000`
      (2027-02-01), both observed by running the test with the fix reverted.
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests
      (478 unit / 12+2+44+4+1+5+1 across the other bins), `./test-workspace.sh`,
      firmware `cargo check`, `scheduler --help` e2e. `scheduler.rs` rustfmt
      diff count unchanged at 25.

### 🟢 P2 — audit round 29: Ctrl-C left the terminal in raw mode (2026-09-26)

Scope: `cli/src/lineedit.rs`'s raw-mode guard — an item carried for many rounds
as "needs a pty to test, so possibly too fragile". Working the question "what
does this guard fail to cover?".
- [x] **`Drop` cannot run on the signal the module relies on.** The guard
      restores `termios` from `Drop`, and the module doc argues explicitly that
      leaving `ISIG` set means "Ctrl-C still sends `SIGINT` and kills the REPL…
      nothing here has to invent an interrupt path". But `SIGINT`'s default
      disposition ends the process **without unwinding**, so `Drop` never ran:
      the REPL died and the shell came back with `ICANON` and `ECHO` cleared —
      no line editing, invisible keystrokes, until `reset`. The doc's own
      premise ("kills the REPL") is exactly what skipped the cleanup.
- [x] **Only a TTY reaches it, which is why the tests missed it.** `read_line`
      enters raw mode only when `stdin.is_terminal()`; every REPL test pipes
      stdin, so they exercise the `fallback` path and never touch the guard. The
      bug had no coverage by construction, not by oversight.
- [x] **Fixed with an async-signal-safe handler.** `enable` boxes the saved
      `termios` (stable address across the move out of `enable`), publishes it in
      a `static AtomicPtr`, and installs a `SIGINT` handler that restores the
      settings, resets `SIGINT` to `SIG_DFL` and re-raises — so the process dies
      from the signal with its usual status. The handler touches only an atomic
      load, `tcsetattr` (POSIX async-signal-safe), `sigaction` and `raise`.
      `Drop` clears the pointer first, then returns `SIGINT` to the disposition
      it had before, so a caller that installed its own handler keeps it.
- [x] **Rejected: clear `ISIG` and handle `0x03` in the key loop.** Simpler and
      trivially testable, but it only covers the terminal's `^C` byte, not
      `kill -INT` from another process, and it silently disables `^Z`/`^\`.
      Handling the signal is the honest fix; ISIG stays set.
- [x] **The pty test is portable, contrary to the earlier fear.** `openpty`/
      `forkpty` live in `libutil` on glibc, but `posix_openpt`/`grantpt`/
      `unlockpt`/`ptsname` do not — they are in libc on Linux and macOS, and
      `libc` is already a unix-only dependency of the crate. So the portability
      objection that kept this item carried was based on the wrong function set.
- [x] **Regression test proven against the old code:**
      `cli/tests/sigint_restore.rs::sigint_restores_the_terminal_before_the_repl_dies`
      opens a pty, runs `magent run --repl --mock` on it, waits for the prompt
      (asserting raw mode is actually engaged), sends `SIGINT`, and reads the
      terminal's `termios` back. Against the old code it failed at
      `ICANON must be restored… left: 0 right: 0`. The child still died from
      `SIGINT` in both versions, so the test isolates the restore and does not
      merely re-assert that Ctrl-C exits.
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy and tests
      (477 unit / 44 repl / 1 pty), `./test-workspace.sh`, firmware `cargo
      check`, CLI e2e. `lineedit.rs` and `sigint_restore.rs` rustfmt diff counts
      both 0.
- [ ] **Carried:** `host/simulator`'s five-tool catalog (prompt spot-checked
      consistent, not audited against the other executors); `safety::Watchdog`
      cannot trip (documented deliberate — enforcement lives in firmware
      `rt_watchdog.rs` / nrf52 `watchdog.rs`); `MqttAdapter::send` returns
      `Ok(())` as a documented stub for a receive-only adapter.

### 🟢 P2 — audit round 28: the SSRF guard was bypassed by an empty port (2026-09-26)

Scope: a fresh pass over the web-fetch security boundary after round 27's CLI
work; specifically the one URL parser the host tools and the firmware share.
- [x] **The guard waved through `host:` authorities.** `parse_http_authority`
      returned the authority verbatim whenever the port substring failed to
      parse. An empty port is legal (`http://host:/` means the scheme default),
      so `http://127.0.0.1:/admin` parsed to `("127.0.0.1:", 80)`.
      `public_fetch_target_refusal("127.0.0.1:")` then found it to be neither
      IPv4, IPv6, `localhost` nor a `.local` name, and returned `None`.
- [x] **It was exploitable, not theoretical.** `validate_fetch_url` checks only
      the host; the fetch sends the raw URL through `reqwest`, whose URL parser
      accepts `http://127.0.0.1:/admin` as loopback on port 80. So the
      model-generated `fetch_url` tool could reach the device's own HTTP server
      or `169.254.169.254:/latest/meta-data/`. The same bypass covered
      `localhost:/`, `.local:/`, RFC1918 literals and bracketed IPv6
      (`[::1]:/`). Confirmed with the `url` crate that only the *empty* port
      form is dialable — a non-numeric port is rejected client-side — which is
      why the fix is scoped to the empty case.
- [x] **Fixed at the shared parser, not at each caller.** Empty port now yields
      `(host, default_port)`, so the classifier sees the bare host. This follows
      the function's own documented reasoning for the unbracketed-IPv6 case
      (`http://::1/`), which was the earlier instance of "the classifier cannot
      recognise the host string the parser produced".
- [x] **Regression tests proven against the old behaviour:** the parser test
      failed `left: Some(("127.0.0.1:", 80))` / `right: Some(("127.0.0.1", 80))`,
      and both guard tests failed their `is_err()` assertions. New tests:
      `parse_http_authority_drops_an_empty_port`, `empty_port_targets_survive_the_full_guard`,
      `web::tests::validate_fetch_url_rejects_an_empty_port_authority`.
- [x] Verified: full sweep green — magent-core `std` lib (858) and `esp32` lib
      (567) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy,
      `./test-workspace.sh`, `cargo test -p magent --features
      web3,web3_app,email-tools,blockchain` (477), firmware `cargo check`, CLI
      e2e. `at_validate.rs` and `web.rs` rustfmt diff counts both 0.

### 🟢 P2 — audit round 27: a truncation that panics, and an artifact that reaches nothing (2026-09-26)

Scope: the CLI crate's own output/front-end paths, a sweep of the CLI's
`to_stdout` sites after round 26's web3 module work.
- [x] **`summary::truncate` sliced a `str` by bytes and panicked.** It compared
      `s.len()` (bytes) and then took `&s[..max]`, which aborts when `max` lands
      mid-code-point. Both callers pass non-ASCII-reachable text: `summary list`
      truncates the topic column to 24, `summary show` truncates `llm_summary`
      to 80. `validate_topic` bounds topics by *bytes*, so a legal 26-byte CJK
      topic reaches it. Fixed by counting characters, matching the `truncate`
      helpers already in `config.rs`/`prompt.rs`; this was the only one of the
      three that was byte-based — and the only one that crashed. A sweep of the
      crate for the same pattern found the rest already boundary-safe
      (`runner.rs::truncate_utf8`, `tools.rs::clamp_utf8`, ASCII/`&[u8]` sites).
- [x] **`emit`'s no-`--output` branch wrote nothing in the default mode.** It
      called `Output::write_json_str`, a no-op in human mode, so `bind create` /
      `vc issue` / `vc present` computed, signed and validated their artifact and
      then dropped it — `magent web3 vc issue … > cred.json` produced an empty
      file, despite the doc promising stdout. The same branch emitted *two* JSON
      objects under `--json` (the artifact, then the caller's `render_*` display),
      which `output.rs` explicitly promises not to do. Fixed to use
      `write_stdout_line` (reaches stdout in both modes, the `web3 sign`
      pattern) and to return which path was taken so the callers suppress their
      own rendering; the `--json` single-object contract now holds.
- [x] **Both proven against the old code.** The truncate test aborts the old
      implementation with `end byte index 24 is not a char boundary; it is
      inside '日'`; for `emit`, the real binary was built against the restored
      old branch and produced 0 bytes of stdout in human mode and 1001 bytes
      (two objects, `json.loads`: `Extra data: line 2 column 1 (char 739)`) in
      `--json`, versus a single 739-byte object after the fix.
- [x] Verified: full sweep green — magent-core `std` lib (855) and `esp32` lib
      (565) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools,blockchain` clippy,
      `./test-workspace.sh`, `cargo test -p magent --features
      web3,web3_app,email-tools,blockchain` (477), firmware `cargo check`, CLI
      e2e. `summary.rs` and `web3_blockchain.rs` rustfmt diff counts both 0.

### 🟢 P2 — audit round 26: a second "EVM RPC client" that could not make a single call (2026-09-26)

Scope: `magent-core/src/web3/blockchain/client.rs`, reached while surveying
the web3 module for advertised-but-unimplemented surfaces.
- [x] **A `pub` type whose doc promised HTTP RPC and whose code did none.**
      `EvmRpcClient` self-described as an "HTTP-based EVM RPC client". Every
      one of its fourteen `ChainClient` methods funnelled through a private
      `raw_call` placeholder that returned
      `Err(BlockchainError("RPC not configured: …"))` unconditionally; `call()`
      returned `Err("… requires reqwest: …")`. It could not succeed at
      anything, for anyone.
- [x] **It was dead — which is why nothing caught it.** `grep -rn` over the
      whole workspace found zero references outside its own file; no `docs/`
      mention; and `blockchain/mod.rs` re-exports only `BlockchainResult`,
      `ChainClient`, `ChainId` from the module. The one live EVM client is
      `HttpRpcClient`, used by `BlockchainBackend::Std` and
      `cli/src/web3_blockchain.rs`. The struct was `#[allow(dead_code)]`-hidden,
      which is precisely what stops the compiler from asking why a public type
      is unused.
- [x] **Its tests showed the shape of the problem.** `test_evm_client_creation`
      asserted `client.is_ok() || client.is_err();` — a tautology — and
      `test_evm_client_call_returns_error_without_reqwest` asserted the
      placeholder *stayed* broken. Green tests over an unreachable type.
- [x] **Resolved by deletion, not re-implementation.** The real implementation
      already exists (`HttpRpcClient`, wired and tested); rebuilding a duplicate
      behind a `std`/no-`std` shim would manufacture a second copy of working
      code for a consumer that does not exist. Removed the struct, both `impl`
      blocks, the private `hex_encode`/`hex_decode`, and the local
      `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError` copies (duplicates of the
      live `http_types` types that served only the placeholder). No parsing
      coverage lost: `HttpRpcClient` has its own
      `parse_hex_u64`/`parse_hex_wei`/`parse_hex_bytes`/`hex_encode` and tests
      for them.
- [x] **Module doc corrected** from "a concrete HTTP-based RPC client for
      EVM-compatible chains" to what the module is — the `ChainClient` trait
      and its re-exported receipt/log types — pointing at `HttpRpcClient` for
      the concrete implementation. `test_event_filter_builder`, the one test
      touching a live type, is retained; the other thirteen went with the code.
- [x] **Verification is by construction, stated honestly:** a removal cannot be
      proven by a failing assertion the way a behaviour fix can. It is verified
      by `grep` (no reference anywhere in the tree or `docs/`) and by the crate
      compiling without it across the full feature matrix, including
      `--features blockchain` and `--features web3` standalone.
- [x] Verified: full sweep green — magent-core `std` lib (855) and `esp32` lib
      (565) tests, `std` integration tests, workspace clippy,
      `magent --features web3,web3_app,email-tools` clippy,
      `./test-workspace.sh` (all feature-matrix crates, incl. `blockchain`),
      `cargo test -p magent --features web3,web3_app,email-tools`, firmware
      `cargo check`, CLI e2e. `client.rs` rustfmt diff count 0.

### 🟢 P2 — audit round 25: a `Retry-After` date was read as no hint at all (2026-09-26)

Scope: the carried item at line 1796, "only `delta-seconds` is parsed … left
open deliberately rather than half-parsed", plus the same line carried forward
through rounds 20–22.
- [x] **A documented-but-unimplemented branch of RFC 7231.** `parse_retry_after`
      was documented as parsing "an RFC 7231 `Retry-After` value" but returned
      `None` for the `HTTP-date` form, so a `429` that named an instant was
      retried after our own 250 ms backoff, spent both retries inside a second,
      and failed with an error the server had already told us how to avoid.
- [x] **The stated reason for the gap was false.** The old comment said the
      date form "needs the wall clock, which would make this decision
      untestable". It needs *a* clock, and the clock is exactly the kind of
      thing you inject: `parse_retry_after(value, now)` is a pure function of
      its two arguments, and every new test pins `now` exactly.
- [x] **All three required formats**, not just the easy one: IMF-fixdate, the
      obsolete RFC 850 form (two-digit year resolved by the RFC 7231 50-year
      rule), and asctime. A hand-rolled HTTP-date parser plus Hinnant's
      `days_from_civil`; no new dependency.
- [x] **The invariants that already existed still hold.** A past date floors at
      zero, which `retry_delay` raises back to the backoff, so the header can
      only *extend* a wait; the 30 s cap still bounds a remote value.
- [x] **Two independent checks on the arithmetic.** The new civil→Unix
      conversion is cross-checked against the library's own existing
      `unix_to_calendar` (the direction that was already implemented and
      tested), and an end-to-end mock-server test proves a date-bearing `429`
      is actually waited out for the delta.
- [x] **Regression proven against the old behaviour:** with the date
      conversion stubbed to `None`, `retry_after_parses_the_http_date_form`
      failed `left: None` / `right: Some(30s)` and
      `retry_after_is_read_off_the_response_headers` failed `left: None` /
      `right: Some(9s)` — the old code's exact result.
- [x] Verified: full sweep green — magent-core `std` lib (855) and `esp32` lib
      (565) tests, `std` integration tests, workspace clippy, `magent
      --features web3,web3_app,email-tools` clippy, `./test-workspace.sh`,
      `cargo test -p magent --features web3,web3_app,email-tools`, firmware
      `cargo check`, CLI e2e. `agent_runner.rs` rustfmt diff count back at its
      baseline of 1.

### 🟢 P2 — audit round 24: the ESP32 advertised ten sensors it cannot read, and hid the one it can (2026-09-26)

Scope: the carried item at line 1916, "the ESP32 still advertises `battery` …
now visible in the prompt only because `read_sensor` is one tool with twelve
sensor names rather than twelve tools". Round 21's `serves()` filtered whole
tool *names*; this round is the argument-level analogue.
- [x] **`serves()` filtered names, so the argument space leaked.** The registry
      registers `read_sensor` once with the shared twelve-sensor description
      (`temperature`, `accelerometer`, `humidity`, `pressure`, `light`,
      `heart_rate`, `hrv`, `glucose`, `ecg`, `stress`, `battery`, …). The ESP32
      serves that one name, so every one of those sensors was promised to the
      model — while `Esp32ToolHandler::read_sensor` answers all but two of them
      with `unsupported sensor: {sensor}` (success=false). Worse, `memory` —
      which the board genuinely reads via `esp_get_free_heap_size` — was never
      mentioned at all, so the one working sensor was the invisible one.
- [x] **The mechanism.** New `ToolHandler::describe_tool(&self, name) ->
      Option<&'static str>` (default `None`, so no existing handler changes
      behaviour), applied inside `describe_filtered` **only in the served view**.
      `describe()` still shows the full shared description; `describe_served()`
      (used by the LLM system prompt and `inject_skills`'s `tools=` message)
      substitutes the handler's narrower text. Counter + `log::info!` when an
      override fires, mirroring round 21's unserved counter.
- [x] **The ESP32 declares what it really reads.** `Esp32ToolHandler::describe_tool`
      returns `Read a real device value. args: sensor=temperature (chip die
      temperature) | sensor=memory (free heap)` for `read_sensor`, kept in step
      with the arms above it.
- [x] **Two regression tests, both verified to fail against the old code.** One
      in `tools.rs` (a registry built with the real twelve-sensor text + a
      temperature-only handler); one in `agent.rs`
      (`the_llm_prompt_does_not_advertise_sensors_the_handler_cannot_read`,
      under the embedded features, driving the real `build_llm_system_prompt`).
      The pre-fix failure printed exactly
      `- read_sensor: Read a sensor value (temperature, accelerometer, …,
      battery)` — nine sensors the board cannot read.
- [x] **Deliberate boundary, left as-is:** `MiniAgent::pick_tool` still emits
      `sensor=battery` when planning. That is a generic planner and `battery`
      is real on nRF52 and the host simulator; the ESP32 answers with an honest
      `unsupported sensor: battery` refusal (success=false), not fake success.
      Narrowing the planner per-target is a separate, larger change.
- [x] Verified: full sweep green — magent-core `std`/`esp32` lib tests, `std`
      tests, workspace clippy, `magent --features web3,web3_app,email-tools`
      clippy, `magent-core --features esp32` clippy, `./test-workspace.sh`,
      `cargo test -p magent --features web3,web3_app,email-tools`, firmware
      `cargo check`, CLI e2e (`magent run --mock 'what is my battery level'`).

### 🟢 P2 — audit round 23: the `web3 chain|bind|balance|tx|vc` surface was documented but unreachable (2026-09-26)

Scope: the carried item at line 756, "the CLI's `magent web3 chain|bind|balance|tx|vc`
commands are documented but not implemented". Owner chose **full implementation**
of all nine commands over deleting the module.
- [x] **Nothing wired the parser to the executor.** `cli/src/web3_blockchain.rs`
      held `BlockchainAction` + display structs + renderers, but `Web3Action` had
      no blockchain variant and `parse_web3_args` had no `chain`/`bind`/
      `balance`/`tx`/`vc` arms, so every documented invocation died at the
      unknown-subcommand path. Added the variant (dropping `PartialEq`/`Eq`,
      which `serde_json::Value` cannot satisfy) and all nine parser arms.
- [x] **All nine genuinely implemented, not stubbed.** `balance` → live
      JSON-RPC `eth_getBalance`; `bind create` → secp256k1 address derivation +
      EIP-191 `BindingClaim` signature, self-verified before emit; `bind verify`
      → signature + metadata cross-check; `tx build` → real unsigned
      `TransactionRequest`; `vc issue`/`vc present` → real Ed25519 credentials /
      presentations; `vc verify` → structure + expiry; `chain list`/`info` →
      `KnownChain::ALL` (new) + `from_name` (new).
- [x] **Security bug found and fixed while testing.** `bind verify` printed
      `✓ Binding is valid` for a proof whose `did` had been edited: `BindingProof::verify`
      only recovers the signer of the message and never binds the metadata to it.
      The verifier now parses the signed message's `did:` / `address:` /
      `chainId:` lines and cross-checks the envelope's `binding` record; a swapped
      DID or chain id now reports invalid. Regression test
      `message_binds_metadata_detects_a_tampered_did`.
- [x] **Feature composition.** `magent-core`'s `blockchain` feature gained
      `verifiable_credentials` (no new crates — shared Ed25519/bs58 deps), which
      surfaced and let us fix two latent defects in that module and its
      integration test.
- [x] **Honesty in the output.** `bind create` reports
      `Proof signed (not submitted on-chain)`, `bind verify` notes it does not
      confirm an on-chain registration, and `vc verify` notes the signature is
      not cryptographically verified (the issuer's key is not in the credential).
- [x] Verified: `web3_blockchain_subcommands_route` proven to fail against the
      old parser; offline round-trips for bind and vc; full sweep green
      (magent-core std/esp32 lib + std tests, workspace clippy, blockchain
      clippy, `./test-workspace.sh`, `cargo test -p magent --features
      web3,web3_app,blockchain`, firmware `cargo check`, CLI e2e).

### 🟢 P2 — audit round 22: the advertised sleep tool could only ever fail in a CLI run (2026-09-26)

Scope: the carried item "`get_sleep_quality` always reports 'no sleep recorded'
in a CLI session", plus a sweep for other advertised-but-unusable paths.
- [x] **The simulated device was permanently fresh.** `SimulatorExecutor` is the
      backend every `magent run` / `--repl` session uses; it wraps
      `AgentSimulator`, whose `new()` deliberately starts with no sleep history
      (round 16: a device not worn overnight has nothing to report, and the tool
      says `MissingField { field: "sleep" }` rather than inventing a score).
      Correct for a fresh device — but nothing anywhere called `simulate_night`
      outside tests, so the CLI's device never aged: the one advertised sleep
      tool answered every question with
      `The tool did not complete: ConfigurationError { field: "sleep", reason: MissingField }`.
- [x] **The fix is at the executor layer, not the simulator.** `SimulatorExecutor::new()`
      seeds a week of nights (`[430, 452, 468, 471, 489, 495, 502]` min) into the
      wrapped device. This leaves round 16's decision intact —
      `AgentSimulator::new()` still honestly has no sleep, and
      `sleep_quality_without_a_recorded_night_is_an_error_not_a_score` still
      passes. A fresh simulator is a fresh device; the CLI's tool backend is a
      device that has been worn, and the harness is the right place to say so.
- [x] **Seven nights is the report's own window** (`get_sleep_quality` caps its
      average and debt at 7), so the seed exercises the full trend/average/debt
      path rather than one night mislabelled as a week.
- [x] **Regression test verified to fail against the old code:**
      `the_cli_tool_backend_can_answer_the_advertised_sleep_tool` (real_tools.rs)
      failed pre-fix with exactly the user-visible `MissingField { field: "sleep" }`;
      `magent run --mock 'how did I sleep last night'` now returns
      `score 88/100 (Excellent) … last 7 nights avg 87 | sleep debt 0.9 h`.
- [x] **Swept the other advertised-but-unusable candidates and found no lie:**
      `BlockchainBackend::Dummy` returns a real `ConfigurationError`;
      `host/simulator`'s `parse_sensor` honestly errors listing what it supports;
      the ESP32 web tools (`get_weather` wttr.in, `get_news` 60s.viki.moe,
      `get_flight_price`) skip non-positive prices and never fabricate;
      `magent doctor` exit 1 is legitimate (model not pulled); every
      `--mock` route behaved honestly.
- [x] **Resolved 2026-09-26 (audit round 24):** the ESP32 no longer advertises
      `battery` in the prompt — `ToolHandler::describe_tool` narrows `read_sensor`
      to the two sensors the board reads. See "audit round 24" above.
- [ ] **Carried:** `host/simulator`'s five-tool catalog and its own planner are
      still unchecked against the other executors; the `Retry-After` `HTTP-date`
      form — **fixed in round 25**; the CLI's `magent web3 chain|bind|balance|tx|vc`
      commands — **fixed in round 23**; `safety::Watchdog` cannot trip; Ctrl-C can
      leave the terminal in raw mode (owner's call).

### 🟢 P2 — audit round 21: the ESP32's prompt advertised seven tools the board cannot run (2026-09-26)

Scope: the carried item "on the ESP32 the prompt-advertised health tools return
'not implemented on this target' rather than a wrong answer — honest, but a
product gap", plus the `ToolHandler` contract that nothing enforced.
- [x] **The registry, not the handler, was the advertised set.**
      `MiniAgent::build_llm_system_prompt` used `self.tools.describe()`, so the
      prompt listed the full built-in hardware pack regardless of what the
      installed handler could run. On the ESP32 that was 15 advertised names and
      8 served ones: `ble_send`, `read_heart_rate`, `read_glucose`, `read_ecg`,
      `check_stress_level`, `voice_output`, `send_notification` were promised
      and then refused. The `ToolHandler` doc already said a handler "has to
      cover every tool it advertises"; it was prose, not a mechanism.
- [x] **The mechanism.** `ToolHandler::serves` (default `true`, so the
      simulated path and every existing handler keep their behaviour) plus
      `ToolRegistry::describe_served()`. Both prompt paths — the LLM system
      prompt and `inject_skills`'s `tools=` message — now filter through it.
      `describe()` still lists everything, so nothing else that reads the
      registry changed.
- [x] **The ESP32 declares what it serves** in one `SERVED` list, and the
      `handle` fallback logs if a served name has no arm, so the two cannot
      drift silently (the firmware has no test harness, so this is a runtime
      diagnostic rather than a test).
- [x] **Two regression tests, both verified to fail against the old code** —
      one in `tools.rs` (registry + a one-tool handler) and one in `agent.rs`
      (the real `build_llm_system_prompt`, under the embedded features).
- [x] **Resolved 2026-09-26 (audit round 24):** the ESP32 no longer advertises
      `battery` (or the other nine sensors it cannot read). `ToolHandler::describe_tool`
      lets the handler override a served tool's description in the prompt;
      `Esp32ToolHandler` narrows `read_sensor` to `temperature`/`memory`. See
      "audit round 24" above. `MiniAgent::pick_tool` still emits `sensor=battery`
      (a generic planner; the ESP32 refuses honestly), left as a documented boundary.
- [ ] **Carried:** `host/simulator`'s five-tool catalog and its own planner are
      still unchecked against the other executors; `get_sleep_quality` always
      reports "no sleep recorded" in a CLI session — **fixed in round 22**; the
      CLI's `magent web3 chain|bind|balance|tx|vc` commands — **fixed in round 23**;
      the `Retry-After` `HTTP-date` form — **fixed in round 25**.

### 🟢 P2 — audit round 20: the sensor vocabulary was a three-way contract, and only one list was right (2026-09-26)

Scope: the carried item "the four executors' tool sets are not identical, with
nothing enforcing that they stay in step", narrowed to the `read_sensor`
sensor-name vocabulary — the one sub-contract with a machine-checkable
invariant already written into the code.
- [x] **The documented invariant was violated.** The host simulator's
      `read_sensor` arm says its routing and `tools.rs::execute_read_sensor`
      "must agree: a prompt tuned with `--mock` has to behave the same on the
      device it is being tuned for". `tools.rs` resolves **12** names,
      `simulator::parse_sensor` resolved **10**, and the prompt advertised
      **9**. The CLI's executor is `SimulatorExecutor` → `AgentSimulator`, so
      `sensor=battery` / `sensor=memory` were accepted on the device and
      rejected on the host with `InvalidValue`.
- [x] **One source of truth.** `tools::SENSOR_NAMES` (12) is now iterated by
      guard tests in `tools.rs` (value table covers it exactly), `simulator.rs`
      (every name resolves on the host executor) and via
      `agent_runner::advertised_sensors()` (the prompt lists exactly it). The
      prompt guard asserts both lists are ≥12 before comparing, so a parser
      that matched nothing cannot make the comparison vacuously pass.
- [x] **`battery` and `memory` are genuinely implemented**, not just accepted:
      `SimSensorManager::read_battery_mv` (starts at 85%, matching the device's
      canned `Battery:85%`, drains toward 3200 mV) and `read_free_heap`.
      Aliases `batt` / `mem` / `heap` / `free_heap` / `sram` were added to both
      `parse_sensor` and `normalize_sensor`.
- [x] **The offline planner can now reach them.** `--mock` gained `battery` and
      `memory` routes; the `memory` route excludes `flash` so "read from flash
      memory" still routes to `flash_read` (asserted).
- [x] **Three regression tests, all verified to fail against the old code**
      (`every_canonical_sensor_resolves_on_the_host_executor`,
      `the_prompt_advertises_exactly_the_canonical_sensors`,
      `device_status_sensors_route_without_hijacking_flash`).
- [ ] **Carried:** the vocabulary is unified, but the *tool* sets still are
      not. `tools.rs` (contract = `MiniAgent::pick_tool` + `BUILTIN_TOOL_NAMES`),
      `host/simulator` (5 tools, its own planner) and the ESP32's
      `local_tools.rs` (8 names) remain unchecked against each other; on the
      ESP32 the prompt-advertised health tools return "not implemented on this
      target" rather than a wrong answer — honest, but a product gap. The
      ESP32 executor advertises `battery` only to reject it — **fixed in round 24**
      (the prompt now names only `temperature`/`memory`), though `pick_tool`
      still emits `sensor=battery` (generic planner; honest refusal on-device).
      `get_sleep_quality` always reports "no sleep
      recorded" in a CLI session (nothing seeds a night outside tests) — **fixed in
      round 22**; only
      the `delta-seconds` `Retry-After` form is parsed. *(Round 21 closed the
      "prompt advertises unserved tools" half of this by filtering the prompt
      through the handler; round 24 closed the argument-space half via
      `describe_tool`; the tool/sensor sets themselves remain open — see the
      round 24 section.)*

### 🟢 P2 — audit round 19: the prompt was a contract nothing checked, and `/tools` listed a different tool set (2026-09-25)

Scope: the standing carried item "the four executors' tool sets are not
identical, with nothing enforcing that they stay in step", plus the tool
listing the CLI shows the user.
- [x] **The prompt↔executor contract is now executable.**
      `agent_runner::advertised_tools()` parses `HEALTH_SYSTEM_PROMPT` itself
      (a second hand-maintained list is what drifts), and
      `simulator::tests::every_tool_the_prompt_advertises_is_routable` fails on
      any advertised name that reaches the unknown-tool arm. It checks the
      error, not `Ok`/`Err`, since most tools legitimately fail a bare `{}`.
- [x] **The guard reproduces rounds 16 and 18 at once.** Removing those three
      arms makes it name exactly `get_sleep_quality`,
      `notify_emergency_contact`, `find_nearest_hospital`.
- [x] **The guard guards itself** (≥24 names + five spot-checks). That
      assertion caught a real bug in the first parser version, which sliced the
      name up to `)` and so skipped all 24 lines.
- [x] **`/tools` no longer lies.** It read `ollama::TOOL_DEFINITIONS`, the
      nRF52-era five-tool catalog, while claiming to be "the same catalog the
      LLM is handed" — it now renders the live prompt's tool lines, so the user
      sees the 24 tools (with signatures) the model actually has. Covered by
      the guard test plus a CLI REPL test that fails against the old listing.
- [ ] **Carried:** the guard covers the CLI executor (`AgentSimulator`) against
      the CLI prompt only. `tools.rs` (the embedded registry, whose contract is
      `MiniAgent::pick_tool` + `BUILTIN_TOOL_NAMES`), `host/simulator` (5
      tools, its own planner) and the ESP32's `local_tools.rs` (8 names) are
      still not checked against anything, and are still not identical to each
      other — on the ESP32 the prompt-advertised health tools return "not
      implemented on this target" rather than a wrong answer, which is honest
      but is a product gap. `get_sleep_quality` always reports "no sleep
      recorded" in a CLI session (nothing seeds a night outside tests); only
      the `delta-seconds` `Retry-After` form is parsed. *(Round 20 narrowed this
      by unifying the `read_sensor` sensor vocabulary across all three lists;
      the tool-name sets remain open — see the round 20 section.)*

### 🟢 P2 — audit round 18: the emergency tools were advertised, implemented nowhere, and backed by a module with no caller (2026-09-25)

Scope: the last two names in `HEALTH_SYSTEM_PROMPT` with no implementation
anywhere (`notify_emergency_contact`, `find_nearest_hospital`,
`agent_runner.rs:745-746`), and `early_warning` — the last module in the tree
with no consumer.
- [x] **Both tools existed only as prompt text.** In all four executors they hit
      the unknown-tool arm. Now implemented in the simulator, backed by
      `EarlyWarningSystem`'s contact registry and distance-ranked hospital list.
- [x] **`notify_emergency_contact` resolves through the registry.** No name →
      the primary contact (the list is priority-sorted for exactly this); an
      unknown name → `InvalidValue`; an empty registry → `MissingField`. The
      notification is queued through the module and read back, and reported as
      *queued, awaiting delivery* — `magent-core` has no transport, and "sent"
      would be a fabricated success.
- [x] **`find_nearest_hospital` ranks by distance and honours the specialty.**
      An unknown specialty is `InvalidValue`; a known specialty no registered
      hospital has is a missing hospital — never substituted with a different
      hospital.
- [x] **`EarlyWarningSystem::queue_notification`** added: the module's entry
      point for a caller-decided notification, sharing the awaiting-delivery
      slot but bypassing the alert cooldown (which rate-limits *repeated*
      sensor alerts, not a user's explicit request). `alert_id = 0` marks it as
      not alert-driven.
- [x] **Seven new tests — all seven fail against the pre-fix code**, five with
      the unknown-tool error.
- [ ] **Carried:** `notify_emergency_contact` / `find_nearest_hospital` are
      implemented in the simulator only — `tools.rs` (embedded registry),
      `host/simulator`, and the ESP32's `local_tools.rs` still have no arm for
      either name, so a device prompt advertising them behaves differently from
      a `--mock`-tuned one; the four executors' tool *sets* are still not
      identical — round 19 added a guard for the CLI executor against the CLI
      prompt, but nothing yet checks the other three.
      `get_sleep_quality` always reports "no sleep recorded" in a CLI session
      (nothing seeds a night outside tests); only the `delta-seconds`
      `Retry-After` form is parsed.

### 🟢 P2 — audit round 17: the offline planner answered every unrecognised task with a temperature reading (2026-09-25)

Scope: `RealAgentRunner::generate_simulated_response` — the `--mock` planner,
the only path that demonstrates the tool set without a model.
- [x] **No route existed for any advertised health tool.** Every health task
      ("how did I sleep last night", "start a running session", "set do not
      disturb", "what is my heart rate") fell through to the default arm, which
      planned `read_sensor temperature` and then said "Task completed
      successfully" — the one path meant to demonstrate the tools was
      demonstrating the wrong one, and never touched rounds 14–16's work. Ten
      routes added for the advertised health set, ordered ahead of the generic
      `["health", "status", "verify", "system"]` arm so "health alert" is not
      swallowed.
- [x] **Route ordering inside the exercise block.** *end* and *adjust/get* are
      matched before *start*; "end my run" and "adjust my exercise goal" both
      contain "exercise"/"run". `start_exercise` receives the activity the task
      named, falling back to `running` only when none is named.
- [x] **The default arm ran an unrequested tool.** An unmatched task got a
      temperature reading and a success line. Now
      `{"result": "No tool call needed for this task (simulated planner)"}`.
      The same bug in `host/simulator`'s independent planner is fixed too.
- [x] **A failed tool was reported as done.** The planner now reads the last
      tool observation and answers `"The tool did not complete: <error>"`,
      JSON-escaped so a `ConfigurationError { field: "sleep", .. }` payload is
      not dropped by `parse_result`.
- [x] Three tests that had encoded the fabricated success were corrected
      (two chain tests stubbed only the first tool; the REPL turn-keying test's
      first turn ended in an unreachable `ble_send`), and a new integration test
      pins the honest-failure behaviour.
- [x] Five new `simulated_planner_tests` — **all five fail against the pre-fix
      planner**, four by planning `read_sensor` for a health task.
- [ ] **Carried:** `early_warning` (1809 lines) was still the only module with
      no in-tree consumer at this point — wired in round 18; the
      `notify_emergency_contact` / `find_nearest_hospital` names were then
      implemented in the simulator only (round 18), not in the other three
      executors; `get_sleep_quality` always reports "no sleep recorded" in a
      CLI session, because nothing seeds a night outside tests; only the
      `delta-seconds` `Retry-After` form is parsed; and the four executors'
      tool *sets* are still not identical, with nothing enforcing that they
      stay in step.

### 🟢 P2 — audit round 16: the last advertised tool, and a debt invented from missing nights (2026-09-25)

Scope: the last unimplemented name in `HEALTH_SYSTEM_PROMPT`
(`agent_runner.rs:736`), and the module that was supposed to back it.
- [x] **`get_sleep_quality` answered the unknown-tool arm.** Now backed by
      `sleep_manager` (928 lines, no caller) — the third and final orphaned
      wearable module wired in as many rounds.
- [x] **No recorded night is an error, not a score.** `MissingField { field:
      "sleep" }`. `AgentSimulator::simulate_night()` generates a night from the
      same resting HR/HRV model the daytime reads use; `SleepQuality::new`
      computes the score, so the reply carries the module's number rather than
      one formatted by hand.
- [x] **`calculate_sleep_debt` inflated the debt with nights nobody recorded.**
      Three recorded nights against `days = 7` gave `56 h − 18 h = 38 h`, because
      the target was `8 h × days` while the actual was summed over the records
      present. `average_sleep_score` already capped its window at the history
      length; the pair disagreed over the same data. Capped identically now, so
      an unworn device reports 0 h.
- [x] **The same fix removed a fabricated recommendation.**
      `get_sleep_recommendations` fires when the debt exceeds 2 h, so with no
      data at all the device advised "睡眠债务约56.0小时，建议增加睡眠时间". The
      old behaviour was documented as intentional ("no data = worst case") —
      a bug with a comment, unexercised because the module had no consumer.
- [x] The reply labels the trend with the number of nights it is really over
      (`last N nights avg`), rather than calling three nights "7-night".
- [ ] **Carried:** `early_warning` (1809 lines) was the only module with no
      in-tree consumer at this point — wired in round 18, which also implemented
      `notify_emergency_contact` and `find_nearest_hospital` (in the simulator,
      not in the other three executors); `get_sleep_quality` always reports "no
      sleep recorded" in a CLI session, because nothing seeds a night outside
      tests — a real device would populate it from overnight sensing; the
      scripted `--mock` planner's health routing gap was fixed in round 17;
      only the `delta-seconds` `Retry-After` form is parsed; and the four
      executors' tool *sets* are not identical, with nothing enforcing that they
      stay in step.

### 🟢 P2 — audit round 15: the alert tools, and the second module with no caller (2026-09-25)

Scope: the four alert/notification names in `HEALTH_SYSTEM_PROMPT`
(`agent_runner.rs:731-734`). Same question as round 14, applied to the other half
of the prompt.
- [x] **All four answered the unknown-tool arm.** `queue_voice`,
      `send_health_alert`, `send_meditation` and `set_dnd` had no arm in
      `execute_tool`. Six of the seven new behaviour tests fail against the old
      code with `ConfigurationError { field: "tool", reason: MissingField }`.
- [x] **`voice_notification.rs` (991 lines) had no consumer.** It shipped a
      priority voice queue, a multi-channel health alert, a notification
      history, and a do-not-disturb window — reachable from nothing. It is now
      the simulator's notification backend.
- [x] **`set_dnd` changes what `queue_voice` then does.** A state change nothing
      observes is not a feature: with a 22–07 window active (hour 0 is inside
      it), a priority-5 cue never reaches the queue while a priority-9 cue
      does. The test asserts the queue depth, not the reply text.
- [x] **The alert reply reports what the pipeline did, not what was asked.**
      The manager drops the voice cue and the notification independently under
      DND, so "delivered" was untrue for a suppressed severity-6 alert. The
      reply now reads both channels before and after and says which fired.
- [x] **Timestamps are uptime, not a fabricated date.** The device is not
      guaranteed a wall clock, so `uptime_ms()` / `current_hour()` derive the
      hour from a monotonic `Instant`. A fresh simulator is at hour 0 — what a
      just-booted device sees — and the DND tests are written against that fact
      rather than assuming an arbitrary hour.
- [x] **Scales and clocks validate rather than clamp.** `severity`/`priority`
      outside `1..=10` and a DND hour outside `0..=23` are `InvalidValue`;
      absent `text`/`script`/`enabled`/hours are `MissingField`; an unreadable
      bool is `InvalidValue`, not absent. A rejected call leaves no queue entry,
      no notification, and DND off.
- [x] Added `extract_bool_arg` (JSON bool / quoted string / `1`/`0` / legacy
      `key=value`) and the `missing_field` / `invalid_value` constructors, so the
      round-14 arms' inline error literals are no longer repeated per arm.
- [ ] **Carried:** `early_warning` (1809 lines) and `sleep_manager` (928 lines)
      still have no in-tree consumer; `get_sleep_quality`,
      `notify_emergency_contact` and `find_nearest_hospital` remain
      unimplemented (the last two would require fabricating contact/hospital
      data — deliberately not done; `get_sleep_quality` was wired in round 16);
      the scripted `--mock` LLM still answers unrecognized
      health tasks with `read_sensor temperature`, so the CLI e2e does not yet
      exercise these paths; only the `delta-seconds` `Retry-After` form is
      parsed; and the four executors' tool *sets* are not identical, with
      nothing enforcing that they stay in step.

### 🟢 P2 — audit round 14: the advertised exercise tools, and the module with no caller (2026-09-25)

Scope: the four `exercise` names in `HEALTH_SYSTEM_PROMPT`
(`agent_runner.rs:726-729`). Worked from the question "the prompt tells the
model to call these — what happens when it does?".
- [x] **All four answered the unknown-tool arm.** `execute_tool` had no arm for
      `start_exercise` / `end_exercise` / `get_exercise_goal` /
      `adjust_exercise_goal`, so each returned
      `ConfigurationError { field: "tool", reason: MissingField }`. A model
      following the documented prompt was told the tool it was just told to use
      does not exist. Six of the seven new tests fail against the old code with
      exactly that error.
- [x] **`sports_coach.rs` (968 lines) had no consumer.** It shipped a real
      `SportsCoach` state machine — `ExerciseGoal`, `ExerciseType`,
      `ExerciseState`, `session_duration`, `session_progress` — reachable from
      nothing. It is now the simulator's exercise backend. This is the third
      orphaned module (after `sleep_manager` and `voice_notification`) to be
      examined; unlike those two it needed no invented data, which is why it was
      the one worth wiring first.
- [x] **A started session is visible to the rest of the simulator, not just
      recorded.** `start_exercise` sets `is_exercising`, which
      `simulate_heart_rate` reads — so the next `read_heart_rate` returns
      ~141 BPM instead of ~70. `end_exercise` clears it. This is the assertion
      that separates "the tool ran" from "the tool printed a string".
- [x] **Elapsed time is measured, not invented.** `end_exercise` reads a
      monotonic `Instant`, so a zero-length session reports `0s` rather than a
      plausible-looking duration. `end_exercise` drives the coach's state
      machine by that real delta.
- [x] **The not-configured and invalid-value disciplines hold.** No session →
      `MissingField` on `end_exercise`; no goal → `MissingField` on the two
      goal readers; a type the build does not know → `InvalidValue` (distinct
      from an absent type); an intensity outside `0.0..=1.0` → `InvalidValue`,
      rejected rather than clamped. A rejected `start_exercise` leaves
      `is_exercising` false, and a rejected `adjust_exercise_goal` leaves the
      stored goal untouched.
- [x] **Two dead no-op `impl core::fmt::Write` blocks removed.** In
      `sports_coach.rs::ExerciseGoal` and `early_warning.rs::EarlyWarningSystem`
      — `write_str` returned `Ok(())` and discarded its input, so any `write!`
      into either type would have silently built an empty string. Both were
      unreachable and are gone rather than left as a trap.
- [x] `extract_float_arg` added alongside the integer/string readers: an
      integer reader reports `0.7` as *absent*, which would turn "set intensity
      to 0.7" into a missing-field error.
- [ ] **Carried:** `early_warning`, `sleep_manager` and `voice_notification`
      still have no in-tree consumer; the remaining prompt-advertised names
      `get_sleep_quality`, `notify_emergency_contact`,
      `find_nearest_hospital` are unimplemented (the last two would require
      fabricating contact/hospital data — deliberately not done; the alert
      quartet was wired in round 15); the scripted
      `--mock` LLM still answers unrecognized health tasks with
      `read_sensor temperature`, so the CLI e2e does not yet exercise this path;
      only the `delta-seconds` `Retry-After` form is parsed; and the four
      executors' tool *sets* are not identical, with nothing enforcing that they
      stay in step.

### 🟢 P3 — unified error diagnosis (2026-09-24)
- [x] **Panics were indistinguishable from handled failures.** `main` installs a
      hook that keeps Rust's default report, states the failure is a bug in
      magent and where to report it, and under `--json` writes
      `{"ok":false,"code":"internal_error","exit":101}` to stdout — so the
      "stdout is exactly one JSON document" promise now survives a crash. Exit
      status stays 101. Reached end-to-end via the `MAGENT_TEST_PANIC` test seam
      (inert unless set); pinned by `cli/tests/panics.rs`.
      **Why a test seam:** the hook is only reachable by actually panicking, and
      there is no naturally reachable panic in the CLI — the alternative was an
      untested hook. `MAGENT_TEST_OLLAMA_KEY` is precedent for env-gated seams.
- [x] **Exit codes were undocumented.** `magent --help` now prints `EXIT CODES:`
      (0 / 1 / 2 / 101 / 130 / 141), including the `--json` behaviour at code 1
      and the fact that 141 is SIGPIPE, not an error. Pinned by
      `help_text_documents_every_exit_code`.
- [ ] **Error *chains* are not modelled.** CLI errors are `Result<_, String>`, so
      there is no `std::error::Error::source()` chain to print under `--verbose`
      — every cause is flattened into one message at the point it is created
      (`ConfigError::DirIo { path, source }` is the exception, and it already
      renders well). Converting the error types would touch every module's
      signature. Left to the owner: the current messages name the cause, and the
      panic hook covers the "the message is useless" case, so the payoff is
      smaller than the churn.

### 🟠 P2 — swallowed-`Result` audit (2026-09-24)
Scanned every non-test `let _ = `, `.ok()`, `unwrap_or_default()` and
`unwrap_or_else(|_| …)` in `cli/src` and `magent-core/src` (the ~415 raw
hits are mostly `let _ = writeln!(…)` output IO, which is meant to be
ignored). One real defect; the rest are deliberate — recorded here so the
next pass doesn't re-litigate them.
- [x] **`reqwest::blocking::Client::builder().build()` failure silently
      swapped for `Client::new()`** (`agent_runner.rs`, both clients) — fixed.
      `Client::new()` panics on the *same* TLS-initialisation failure and
      drops the 60s timeout, so the "fallback" was strictly worse than the
      error. Now a `Result<Client, String>` field + `http_client()`
      accessor; send paths report it, probes report not-connected.
- [x] **Deliberate, no change:** `runner.rs` `let _ = store.delete(topic)`
      before an overwrite (`delete` is idempotent; documented inline).
- [x] **Deliberate, no change:** `agent_runner.rs`
      `response.text().unwrap_or_default()` on the **error** path — it
      enriches an already-failing HTTP error with the server's message, and
      a lossy read must not destroy that diagnostic.
- [x] **Deliberate, no change:** `cli/blockchain_executor.rs`
      `let _ = self.manager.init()` — `BlockchainManager::init()` returns
      `Ok(())` under `std`, `esp32` and the no-network fallback alike, so
      there is no error to lose; an uninitialised manager is reported by the
      tool call itself ("Blockchain client not initialized").
- [x] **Deliberate, no change:** `main.rs`'
      `config::load().unwrap_or_else(|_| defaults)` — it feeds only the
      `io.json_default` / `io.no_color` defaults. `config show`/`export`
      still fail loudly on a corrupt file and `run` warns separately; the
      remaining silent callers (`version`, help, `doctor`) never read the
      config. Warning here would duplicate `run`'s warning.
- [x] **Deliberate, no change:** the ten
      `serde_json::to_string(…).unwrap_or_default()` calls in
      `cli/web3_blockchain.rs` and the `unwrap_or_else(|_| "{}")` in
      `output.rs`/`doctor.rs` serialize `#[derive(Serialize)]` structs of
      primitives or an already-built `serde_json::Value` — those cannot
      fail, so a fallback path would be unreachable code. (The old
      `unwrap_or_default` *would* have written an empty line, but only if
      serialization of a `String`/`usize`/`bool` struct could fail.)

### 🔴 P1 — `scheduler run-once` made a failed queue look like a clean one (2026-09-24)
- [x] **A queue where every task failed still exited 0 and printed `finished N
      task(s)`.** `run-once` is the unattended, CI-facing path, so from the
      shell's point of view a red queue was indistinguishable from a green one.
      Fixed: `execute_run_once` counts failures and returns
      `SchedulerError::TasksFailed { failed, total }`; `main.rs` maps it to exit
      **1** (runtime failure, matching `magent run` on a failed task — exit 2
      stays reserved for rejected usage), the summary becomes `finished N
      task(s), M FAILED`, and the `--json` envelope gains `failed`. The daemon
      ignores the variant so it keeps ticking.
      **Behaviour change** — the owner chose "任一任务失败即非 0"; a caller
      relying on the old always-0 exit will now see 1. Guarded by
      `cli/tests/scheduler.rs::run_once_exits_non_zero_when_a_task_fails` and
      `...::run_once_exits_zero_when_every_task_succeeds`.
- [x] **A failed task printed nothing at all** — no line named it and the
      summary hid it, contradicting the `execute_run_once` doc comment. Now
      prints `"bad" FAILED: exit 1: error: could not load system prompt`, with
      `one_line_failure` selecting the child's own last non-empty line instead
      of the `[Config] applied …` preamble it arrives wrapped in.
- [x] **A non-zero child was recorded as a *state file* problem.** `run_one_task`
      reused `SchedulerError::State { path: "<magent run> (exit N)" }`, so the
      durable record blamed a healthy file and never named the task. Now returns
      `TaskRun { name, exit, detail }`; the state file says `scheduled task
      "bad" failed (exit 1): …`. Pinned by
      `state_file_records_a_task_failure_against_the_task` (asserts the error
      does *not* contain `scheduler state file`) and the unit test
      `task_failure_is_reported_as_a_task_failure`.
- [x] **`last_error` truncation kept the head, dropping the cause.** `LAST_ERROR_MAX`
      clipping kept the *start* of the message, so a long task pushed the actual
      reason off the end (demonstrated with a 300-char task). Now keeps the
      *tail*, marked with a leading `…`. Pinned by
      `task_stats_truncation_keeps_the_cause_not_the_noise`; the pre-existing
      `task_stats_truncates_long_errors` had only ever used a uniform `x`
      payload, so it passed while the contract was wrong.
- [x] **`last_error` recorded a config dump instead of the cause** — found
      (2026-09-24) while adding the `--json` error envelope. Every `magent run`
      prints `[Config] applied provider=…, model=…` (~380 chars) on stderr first,
      so the preamble filled `LAST_ERROR_MAX` and the actual error was what got
      cut. `task_failure_detail` now drops the `[Config]`/`[Backend]` status
      lines and labels the rest by stream; `one_line_failure` strips that label
      back off for the transcript. Pinned by
      `task_failure_detail_drops_the_config_preamble`.
- [x] **A fresh state file wrote `schema_version: 0`.** `Default` was derived,
      yielding `u32::default()`, which contradicted the `1` that serde writes for
      a missing field and that the daemon documents. Now a hand-written `Default`
      carries `default_schema_version()`. Pinned by
      `default_state_carries_the_current_schema_version`.

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
- [x] **`--json` never emits an *error* envelope** — fixed (2026-09-24).
      `Output::error` became `Output::fail(code, exit, msg)`: JSON mode now writes
      `{"ok":false,"error":…,"code":…,"exit":…}` to stdout *and* keeps the human
      line on stderr, so `magent --json … | jq` gets a structured failure instead
      of a parse error. Codes: `run_error`, `set_prompt_error`, `summary_error`,
      `config_error`, `scheduler_error`, `scheduler_interrupted`,
      `scheduler_tasks_failed`, `web3_error`. `ok` matches `doctor`'s existing
      field. Parse errors (bad flags) remain human-only and exit 2 — they precede
      the `Output` and the caller could not have parsed `--json` anyway.
      Pinned by `cli/tests/json_output.rs`.
      **Follow-on found while fixing this:** `scheduler run-once --json` wrote its
      result envelope *and* returned `TasksFailed`, producing two objects on
      stdout. `Output` now latches after the first document
      (`write_json`/`write_json_str`/`write_stdout_line`) and `fail` skips its own;
      the run-once envelope carries `ok`/`error`/`code`/`exit` itself.
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
      regression is otherwise caught only on hardware. The `config_nvs` NVS
      backend added 2026-11-08 is in the same position: its argument/range logic
      deliberately lives in the host-tested `magent_core::at_validate`, leaving
      only the thin `nvs_*` calls (and the `AT+AGENTCFG` wiring) unexercised
      locally.
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
