# Dual-Core Real-Time Scheduling (REQ-SCHED-001)

This document consolidates the dual-core scheduling architecture implemented
for the ESP32-S3 (`board-s3`) firmware across four phases — **P0** core
binding, **P1** cross-core LLM pipeline, **P2** priority + fixed frequency, and
**P3** WCET measurement + watchdog isolation + end-to-end latency benchmark.

> **Scope.** The ESP32-C61 (single-core RISC-V, the default build target) is
> unaffected by the core/priority/watchdog code — every scheduling change is
> `#[cfg(feature = "board-s3")]` gated and degrades to a no-op on the C61. The
> C61 keeps its existing single-core scheduling and disabled watchdogs.

## Why

The ESP32-S3 is a dual-core Xtensa LX7 part (PRO = Core 0, APP = Core 1). For
aerospace-style *deterministic latency*, we hard-partition work across the two
cores so the operator-facing real-time path is never stalled by heavy,
non-deterministic network work:

- **Core 0 (PRO, I/O domain):** Wi-Fi protocol stack (ESP-IDF's own tasks),
  Wi-Fi supervisor, web-admin HTTP server, SNTP supervisor, and the LLM /
  HTTP / OTA network workers. These block on lwIP / TLS and are *non-critical*.
- **Core 1 (APP, real-time domain):** the MiniAgent ReAct FSM, the AT
  parse+dispatch ingress thread, and the Lua app host. These must stay
  responsive to the operator.

## P0 — Core binding (`src/core_affinity.rs`)

Every `std::thread` is a FreeRTOS task created via `xTaskCreatePinnedToCore`.
`core_affinity::ThreadProfile` bundles **core + FreeRTOS priority** so a single
spawn expresses the whole scheduling intent, and `spawn_thread()` applies it
immediately before spawning:

| Profile            | Core      | Priority | Threads |
|--------------------|-----------|----------|---------|
| `REALTIME_INGRESS` | Core 1    | 20       | ingress (AT parse + hardware dispatch) |
| `REALTIME_AGENT`   | Core 1    | 15       | agent (ReAct FSM), Lua host |
| `IO_NETWORK`       | Core 0    | 10       | Wi-Fi sup, web-admin, SNTP, LLM/HTTP/OTA workers |
| `UNPINNED`         | any       | 8        | delay+reboot threads |

Priorities stay **below** the ESP-IDF radio stack (Wi-Fi ~23, lwIP tcpip ~18) —
the old default of 24 could starve the radio — while the ingress path sits
above the reasoning path on Core 1.

## P1 — Cross-core LLM pipeline (`src/llm.rs`)

The DeepSeek TLS/HTTP call must **not** run on the real-time agent thread (an
8s HTTPS round-trip there would starve Core 1). Instead:

- The agent installs a thin `ChannelLlmBackend` that forwards the request over
  an mpsc channel to a dedicated `llm-worker` pinned to **Core 0**, and blocks
  on a one-shot reply channel.
- The `recv_timeout` wait is a condvar wait → it **yields Core 1** to the
  higher-priority ingress/hardware tasks, while the TLS/JSON runs on Core 0.

The Lua host gets its own worker (same pattern). Each worker owns one
`Esp32DeepSeekBackend` (panic-free, bounded 8s TLS timeout).

## P2 — Priority + fixed frequency (sdkconfig)

- FreeRTOS priorities are assigned via `ThreadProfile` (see table above).
- `CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ_240=y` (S3) / `_160=y` (C61) pins the CPU
  to max clock — a Kconfig *choice*, so the numeric value is derived from the
  `_<MHz>` selector.
- `CONFIG_PM_ENABLE=n` disables esp_pm ⇒ no DFS / light-sleep frequency drops,
  giving the Core 0 / Core 1 partitioning a repeatable timing floor.

## P3 — WCET measurement + watchdog isolation + E2E latency

### Latency / WCET metrics (`src/latency_metrics.rs`)

Lock-free atomic timing channels record **count / min / avg / max (WCET)** in
microseconds. `u32`/`i32` atomics only (the 32-bit targets have no 64-bit
atomics). Exposed via a periodic `[latency]` log line:

| Channel        | Measures |
|----------------|----------|
| `llm_rt`       | Cross-core LLM round-trip (agent wait) |
| `at_dispatch`  | Ingress AT parse + dispatch + render |
| `e2e_reply`    | Command received at UART → reply queued (direct AT path) |
| `agent_task`   | One ReAct task execution (tools + LLM + decision) |

### Watchdog isolation (`src/rt_watchdog.rs`)

Enables the ESP-IDF **Task Watchdog Timer** but subscribes **only** the two
critical real-time threads (agent + ingress, both on Core 1). The Core-0
network workers — which legitimately block on lwIP/TLS — are deliberately **not**
subscribed, so network-induced blocking never trips the watchdog.

- `CONFIG_ESP_TASK_WDT_EN=y`, `CONFIG_ESP_TASK_WDT_INIT=n` — ESP-IDF does NOT
  auto-subscribe the main task (which IDLEs in WFI), so only the RT threads we
  explicitly subscribe are monitored.
- `rt_watchdog::arm()` sets a **18s** timeout + panic-on-trigger + subscribes
  both idle cores (fed by the RTOS scheduler).
- Feed points cover every long RT hop: loop tops, the LLM wait (1s slices),
  the HTTPGET wait (1s slices), `fetch_web` entry, and after `agent.run`.
  Worst-case feed gaps: agent ≈12s, ingress ≈2s — both well under 18s.

The Lua host is **not** subscribed (its `AppRuntime` loop is a black box that
never feeds — subscribing it would false-trip on any long-running script).

## Audit findings & operational notes

- **`panic = "abort"` (release):** any panic — including in the LLM worker —
  aborts the whole board. This is an intentional *fail-fast* posture; the code
  is panic-free by construction (no `unwrap`/`expect` in the LLM backend or
  the new modules). The agent's `catch_unwind` wrapper is therefore **dead
  code** under `panic="abort"` (it requires unwinding). If soft per-task
  recovery is ever wanted, the firmware must switch to `panic = "unwind"` —
  a significant change with its own embedded-toolchain risks.
- **Agent restart** leaks one worker + `ChannelLlmBackend` per restart (the
  existing one-shot-leak pattern). Rare (config-failure path only). A shared
  single worker would eliminate it but is a larger refactor.
- **`e2e_reply`** covers the direct AT path only (not the agent-routed path).
- **Watchdog is hardware-enforced** and must be validated on real hardware
  before production; the 18s timeout reflects the current worst-case designed
  RT blocking (~12s).
- All scheduling code is S3-gated; the C61 build is unchanged and clean.

## PSRAM & memory hardening (REQ-SCHED-001 / mem-1..3)

For the ESP32-S3-WROOM-1-N8R8 (8 MB Octal PSRAM) target:

- **Budget ceilings** (`magent-core` `s3-8mb-psram` feature, enabled by the
  firmware's `board-s3`): `MAX_CONFIGURABLE_MEMORY` 1 MiB → 4 MiB and
  `MAX_DYNAMIC_CONTEXT_BYTES` 512 KiB → 2 MiB — raised but still bounded vs the
  8 MB pool so a runaway LLM reply / long context cannot exhaust PSRAM.
- **Byte-budget GC** (`conversation::gc_to_budget`): the host `RealAgentRunner`
  bounds the live `Vec<Message>` context cache to `MAX_DYNAMIC_CONTEXT_BYTES`,
  **unconditionally** (independent of `CompressionPolicy`) — heap-blast
  protection is not optional. Dropped counts fold into the compression stats /
  trace / summary; the live footprint is exposed as `RunReport.approx_bytes`.
- **Firmware heap guard** (`Esp32DeepSeekBackend`): refuses to parse the LLM
  JSON into a `serde_json::Value` when free heap < 64 KiB, returning a clean
  error instead of an OOM abort.
- **Stack protection**: `CONFIG_SPIRAM_ALLOW_STACK_EXTERNAL_MEMORY=n` forces all
  task stacks into internal SRAM (matches `configure_psram_thread_stacks()`).
- **Observability**: WCET / latency channels (llm_rt / at_dispatch / e2e_reply /
  agent_task) are exposed on the web-admin `/api/status` JSON and the HTML
  dashboard, plus the periodic serial `[latency]` log.

> **N8R8 Octal build path.** `build-s3.sh` uses `sdkconfig.defaults` (a quad
> 2 MB profile). For the 8 MB Octal PSRAM N8R8, merge the base with the Octal
> overlay so the full pool is used:
> `ESP_IDF_SDKCONFIG_DEFAULTS=".../sdkconfig.defaults:.../sdkconfig.s3.defaults"`
> (the overlay sets `CONFIG_SPIRAM_MODE_OCT=y` + the stack protection). The
> merged build compiles; validate on hardware before production.


