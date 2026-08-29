# magent-lua — aerospace-grade audit

This document is the Design-Assurance record for the Lua scripting host. It
maps every safety claim to a verifiable test and to the exact code that
guarantees it. It follows the project's aerospace discipline: **no `unwrap`/
`panic` in production paths**, explicit error propagation, and `unsafe` only
at the VM boundary with a `// SAFETY:`-style rationale.

## 1. Claim inventory → evidence

| # | Claim | Mechanism | Verified by |
|---|---|---|---|
| C1 | Production code never panics on script input | `LuaHostError` returned from every fallible path; `clippy::panic_in_result_fn` denied | `cargo clippy`; tests `parse_error_is_result_not_panic`, `runtime_error_is_result_not_panic` |
| C2 | Lua `longjmp` never unwinds the Rust stack | `mlua`'s `Result` is the only bridge; `From<mlua::Error>` maps to `LuaHostError::Lua` | `error.rs`, `vm.rs::run_script`; the two C1 tests |
| C3 | Scripts cannot reach `os` / `io` / `debug` / `package` / `ffi` | VM built with `StdLib::TABLE\|STRING\|MATH` only | `sandbox.rs::new_sandbox`; `sandbox_blocks_os_library`, `sandbox_blocks_io_library` |
| C4 | A runaway loop cannot hang the device | instruction-count hook; `Err` stops the VM | `sandbox.rs::enforce_budget`; `infinite_loop_hits_instruction_budget` |
| C5 | Runaway memory cannot exhaust the device | `set_memory_limit(512 KiB)` | `sandbox.rs::enforce_budget` |
| C6 | `hardware.*` round-trips real state | bindings call `HardwareBackend` backed by `magent-hal` RAM adapters | `gpio_write_then_read_roundtrips`, `flash_write_then_read_roundtrips`, `i2c_write_then_read_roundtrips` |
| C7 | Backend failure propagates, not silently swallowed | every closure `.map_err(mlua::Error::RuntimeError)` | `flash_read_out_of_range_is_an_error`, `unknown_sensor_is_an_error` |
| C8 | `agent.reason()` drives the real agent | `agent::reason` → `MiniAgent::run` behind a mutex | `agent_reason_returns_nonempty_answer` |
| C9 | Legitimate scripts still work | base lib + table/string/math present | `script_can_compute_but_not_touch_os` |
| C10 | Agent decisions are dispatched deterministically | `Action::parse` + `apply_action` map command strings to hardware | `apply_known_action_writes_gpio`, `apply_pulse_action_parses_duty`, `apply_unknown_action_errors` |
| C11 | A per-tick handler error does not kill the app | `AppRuntime::tick` returns the error but the loop can run again | `runtime_contains_per_tick_error` |
| C12 | Runtime exposes a live heartbeat | monotonic `tick_count` / `uptime_ms` | `runtime_heartbeat_is_monotonic`, `runtime_ticks_and_dispatches_known_action` |
| C13 | Runtime exposes a watchdog | `is_stale(timeout)` reflects last completed tick | `runtime_watchdog_detects_stale_loop` |
| C14 | Config persists across a (simulated) reboot | `nvram` stores/overwrites/removes on flash | `nvram_set_get_roundtrip`, `nvram_overwrite_replaces_previous`, `nvram_remove_deletes_key`, `nvram_rejects_overlong_key_and_value` |
| C15 | Agent decision drives real hardware (end-to-end) | mock LLM → `agent.reason` → `Action` → `apply_action` | `agent_decision_drives_pwm` |
| C16 | Simulated sensor surface matches agent tool names | `SimHardware::sensor_read` covers temp/hr/hrv/battery/memory/glucose | `lua_sensor_surface_covers_agent_tools` |
| C17 | The real `main.lua` boots and ticks through the runtime | end-to-end smoke test | `runtime_boots_the_real_main_lua` |
| C18 | Runtime exposes a health snapshot for the supervisor | `AppRuntime::health` returns uptime/ticks/errors/stale | `runtime_health_tracks_errors`, `runtime_health_reports_stale` |
| C19 | Sandbox blocks `debug` / `package` / `ffi` | stdlib allow-list is only TABLE/STRING/MATH | `sandbox_blocks_debug_library`, `sandbox_blocks_package_library`, `sandbox_blocks_ffi_library` |
| C20 | Memory cap is actually enforced | `set_memory_limit(512 KiB)` rejects a ~5 MB allocation | `sandbox_enforces_memory_limit` |
| C21 | Overlong script inputs are capped at the binding layer | `MAX_PAYLOAD_LEN` / `MAX_I2C_LEN` / `MAX_REASON_LEN` | `hardware_rejects_overlong_payloads` |
| C22 | Instruction budget resets per script | resettable `Budget` handle in `run_script`/`call` | regression: `infinite_loop_hits_instruction_budget` still passes |
| C23 | Event loop can be stopped cleanly across threads | `stop_flag` + `run_until_stop` | `runtime_can_stop_cleanly_via_shared_flag`, `run_until_stop_honors_max_ticks`, `run_until_stop_respects_stop_flag` |
| C24 | App can be hot-reloaded without leaking stale state | `LuaVm::reload_state` rebuilds a clean VM; `AppRuntime::reload` resets counters | `reload_replaces_app_state`, `reload_resets_error_health` |
| C25 | ADC channel reads a settable voltage | `hardware.adc_read(pin)` round-trips `set_adc` | `lua_adc_reads_set_voltage` |
| C26 | Full pipeline: real `main.lua` → `agent.reason` → action → hardware | script itself drives the fan PWM via a mock agent | `main_lua_agent_drives_fan_via_full_pipeline`, `main_lua_stays_idle_when_cold` |
| C27 | Robustness: edge inputs / wrong types / NVRAM boundaries never panic | covered edge cases + boundary sizes | `action_parse_handles_edge_inputs`, `wrong_lua_arg_types_error_not_panic`, `nvram_handles_boundary_sizes`, `nvram_many_keys_roundtrip`, `i2c_transfer_writes_then_reads` |

## 2. Thread-safety rationale (C5/TLM)

`SharedAgent` and `SharedHardware` are `Arc<Mutex<..>>`. The VM is
**single-threaded by design** — each `LuaVm` and its bindings live on one
thread (mlua's default `MaybeSend` mode). This is why `MiniAgent` need not be
`Send`. On firmware, the whole VM runs on one dedicated FreeRTOS worker thread
(see `docs/LUA_SCRIPTING_S3.md`); do **not** share one VM across threads.

## 3. Failure-containment boundary

* `LuaVm::run_script` is the single choke-point: a Lua error is converted to
  `Err(LuaHostError::Lua)`.
* `agent.reason` returns `Err(LuaHostError::Agent)` if the mutex is poisoned
  or the agent fails.
* `hardware.*` closures return `Err(mlua::Error::RuntimeError)` on backend
  failure, which becomes `LuaHostError::Lua` at `run_script`.

No code path performs a bare `unwrap()`, `expect()`, or `panic!()` outside
`#[cfg(test)]` / the demo's top-level error formatting.

## 4. Known limitations & follow-ups (honest)

* **`MiniAgent` heuristic answer format.** With no LLM backend the embedded
  agent returns a canned heuristic string (e.g. `"Task: Tool result: 25.5°C"`),
  so the demo's `string.match(action, "COOL")` won't match. On the ESP32‑S3 the
  firmware sets a DeepSeek `LlmBackend` (via `set_llm_backend`) so `reason`
  returns real decisions. The binding is agnostic to the answer content.
* **Instruction budget is per-script, reset before each `run_script`/`call`**
  via a resettable [`Budget`](src/sandbox.rs) handle, so a long-lived VM never
  exhausts a cumulative budget across many scripts. A single script that
  exceeds `INSTRUCTION_BUDGET` is still stopped.
* **`magent-core` newer-toolchain clippy warnings** (e.g. `is_multiple_of`,
  `div_ceil`) are pre-existing and unrelated to this crate; they surface when
  `-D warnings` is applied to the whole dependency graph.
