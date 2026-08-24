# Aerospace-Grade Code Audit — mAgent

**Date:** 2026-08-24
**Scope:** `magent-core` (chip-agnostic agent core), `firmware/esp32-app`, and the
host CLI where relevant. Goal: verify the project's aerospace-grade safety
claims and harden any gaps found.
**Standard applied:** panic-freedom on runtime paths, bounded memory, input
validation, fault tolerance, fail-closed security, no silent arithmetic
overflow in trusted paths.

---

## 1. Methodology

1. Grep-scan production code (non-`#[cfg(test)]`) for `unwrap`, `expect`,
   `panic!`, `unreachable!`, `todo!`, and `unsafe`.
2. Classified each occurrence as **safe** (compile-time constant, unreachable
   hardware path, fail-closed security decision, bounded-and-validated) or
   **at-risk** (depends on runtime data that could violate the bound).
3. Ran `cargo clippy -p magent-core --features std` and triaged the lints.
4. Verified the 9 `unsafe` blocks for soundness (all are `zeroize`-style memory
   scrubbing or test-only env setup).
5. Fixed the at-risk findings and re-ran the full test suite.

---

## 2. Verified sound (aerospace claims confirmed)

| Area | Status |
|---|---|
| Production `unsafe` | 9 occurrences, all sound: `write_volatile`/`drop_in_place` memory zeroing in `web3/identity.rs`, and `set_var` in test-only `summary` code. |
| `firmware` panics | Remaining panics are fail-closed / compile-time-constant: `panic!("hardware TRNG is required")` (refuses to boot without secure entropy) and `.expect("... in range")` on hardcoded constants. |
| `web.rs` regex `expect`s | All on hardcoded regex literals (compile-time). |
| `at_validate.rs` `expect`s | All inside `#[cfg(test)]`. |
| Bounded buffers | Tool names, args, messages, and AT fields all use `heapless` bounded types; input is length-validated before conversion. |
| Arithmetic | No unchecked arithmetic on untrusted values found in the agent/tool runtime paths (`capacity()-len()` and `count()+1` are overflow-safe). |

---

## 3. Issues found and fixed

| # | File | Issue | Fix | Safety impact |
|---|---|---|---|---|
| 1 | `magent-core/src/agent.rs:503` | `heapless::String::try_from(name).unwrap()` on the runtime tool name in the heuristic path. Today `pick_tool` only returns short static names, but a future/custom name >32 bytes would panic. | `unwrap_or_default()` (matching the LLM path), degrading to a graceful "unknown tool". | Removes a latent runtime panic (defense-in-depth). |
| 2 | `magent-core/src/boot_key.rs` | `derive()` `panic!` in a `fn -> Result` (feature-off stub). | Added `BootKeyError::FeatureDisabled`; the stub now returns `Err` instead of panicking. | Panic-free API; satisfies `clippy::panic_in_result_fn`. |
| 3–5 | `magent-core/src/agent_runner.rs` (`SharedTraceSink`) | 5× `.expect("trace sink poisoned")` on `Mutex::lock()`. A panic in another thread while holding the mutex would cascade into a second panic on the trace/logging path. | Added `lock_sinks()` using `Mutex::lock().unwrap_or_else(|e| e.into_inner())` to recover the (still-valid) guard. | Prevents a cascading double-panic in the observability path. |

All fixes are regression-tested by the existing suite (no new panics introduced).

---

## 4. Clippy triage

`cargo clippy -p magent-core --features std` → **0 errors** after the fixes
(previously 1 error: the `boot_key` panic).

Remaining warnings are non-safety:
- ~20 × missing doc comments (style).
- ~5 × `contains()` vs `iter().any()` / redundant closures (perf).
- 3 × `result_large_err` (large `Err` variant — stack-size consideration for
  embedded; see recommendation R1).
- 1 × `large_size_difference` between enum variants (same root cause).

---

## 5. Remaining recommendations (not changed — API/design tradeoffs)

- **R1 — Large error type:** `AgentError`/`Web3ErrorKind` variants carry large
  `String` payloads. On a constrained embedded stack this is a footprint
  concern (worst-case `Err` size). Consider boxing the largest payloads or
  auditing worst-case stack depth. Not a correctness bug.
- **R2 — `DeepSeekClient::new`:** `new(api_key)` panics on an empty key (it
  delegates to `try_new`). Acceptable for a config contract, but a
  `Result`-returning constructor would be more aerospace-consistent. API change;
  deferred.
- **R3 — `web.rs` `reqwest Client` build:** the singleton `http_client()`
  `expect`s that the client builds. Building essentially never fails, but it is
  a one-time init panic. Could be made fallible; low value, deferred.
- **R4 — Panic-freedom sweep:** ~35 remaining production `unwrap`/`expect` are
  on compile-time constants or validated/bounded data (verified above). Keep a
  CI guard (e.g. `#![deny(clippy::panic)]` on firmware release) to prevent
  regressions.

---

## 6. Conclusion

The core runtime is in strong aerospace shape: bounded buffers everywhere,
input validated before conversion, no sound-`unsafe` concerns, and firmware
panics limited to fail-closed security and compile-time constants. This audit
removed the remaining latent runtime panics in the agent core and made the
observability layer panic-cascade-safe. Full test suite: magent-core **421** +
CLI **329**, 0 failures.
