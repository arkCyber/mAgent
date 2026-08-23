# Software Requirements Specification (SRS)

TRACE: This document is the project's single source of truth for safety-critical
requirements. Every requirement is identified by a stable `REQ-XXX-NNN` code,
verified by one or more artifacts listed under **Verification**, and traceable
to a gap or feature in the **Source** column.

## Conformance Targets

| Target | Mapping |
|---|---|
| DO-178C / ED-12C | Software Levels A–E → mapped to `REQ-SAFE-*` |
| ECSS-E-ST-40C | Software-engineering requirements → wired to `REQ-*` |
| MISRA-C 2012 / MISRA-Rust 2024 | Coding rules → `cargo clippy --workspace` |
| NASA NPR 7150.2 "Power of Ten" | Static rules → `REQ-SAFE-001`, `REQ-SAFE-003` |
| Embedded Rustacean's "Don't" list | FFI/safety → `REQ-SAFE-001`, `REQ-DOC-001` |

## Requirements Register

| ID | Category | Description | Verification | Source / Status |
|---|---|---|---|---|
| REQ-FW-001 | Firmware | 固件构建可在 `riscv32imac-unknown-none-elf`(`esp32c61`) 目标上通过 | `cargo build -p magent-esp32-app` | GAP-001 / GAP-016 |
| REQ-FW-002 | Firmware | 启动到 Wi-Fi STA 连通 ≤ 5 s | 实机测试 | GAP-012 |
| REQ-FW-003 | Firmware | 主任务栈 ≤ 8 KiB,其余任务 ≤ 4 KiB | `sdkconfig.defaults` + 启动打印 | esp-idf 默认 |
| REQ-FW-004 | Firmware | 默认启用 Secure Boot v2 与 Flash Encryption | `espsecure.py` 烧录脚本 | GAP-014 |
| REQ-FW-005 | Firmware | OTA 升级失败回滚到上一分区 | ESP-IDF bootloader rollback | GAP-015 |
| REQ-NET-001 | Network | 区块链 RPC 必须使用 mbedTLS(走 ESP-IDF 内部) | `sdkconfig.defaults` + 单测 | GAP-009 |
| REQ-NET-002 | Network | RPC 失败必须返回 `Err`,禁止"假装成功" | 单测 + clippy lint | GAP-003 (已修) |
| REQ-NET-003 | Network | 请求 URL 基址必须支持 `<scheme>://<host>[:<port>][/<path>]` | 单测 `test_config_from_url_*` | 已修 |
| REQ-NET-004 | Network | 任何 HTTP 路径必须经过 `EspHttpClient::post` trait 抽象 | 代码评审 | 新增 |
| REQ-SAFE-001 | Safety | 所有 `unsafe` 块必须 4 行 `SAFETY: ...` 注释 | `cargo miri test` + PR review | ECSS-E-ST-40 6.7 |
| REQ-SAFE-002 | Safety | `Box::new` / `Vec` / `String` 不得在 ISR 中分配 | clippy `large_types_passed_by_value` + 评审 | NASA NPR 7150.2 |
| REQ-SAFE-003 | Safety | 全局可变量必须 `Atomic` 或 `Mutex` | clippy `mutable_key_type` + miri | MISRA-C 9.1 |
| REQ-SAFE-004 | Safety | `Debug` 在 `no_std` 路径必须使用 `core::fmt::Debug` | 编译 | 已修 |
| REQ-SAFE-005 | Safety | 扇区未超阈值前不得标记为 worn-out | `wear_leveling` 单测 | 已修 |
| REQ-CFG-001 | Config | 编译期硬切 `riscv32imac` ELF,C61 必须用 | `cargo config` + `build.rs` panic-if | GAP-001 |
| REQ-CFG-002 | Config | PSRAM 必须按 ESP-IDF 优先级自动分配 | `sdkconfig.defaults` | GAP-013 |
| REQ-VFY-001 | Verification | 所有库单元测试通过 (`cargo test --workspace --lib`) | `tools/ci/dangerous_patterns.sh` | 已达成 410/410 |
| REQ-VFY-002 | Verification | `cargo clippy --workspace --all-targets -- -D warnings` 0 警告 | 同上 | 已达成 (host bins) |
| REQ-VFY-003 | Verification | `cargo deny check` 0 high vulnerability | `tools/ci/dangerous_patterns.sh` + `deny.toml` | 已达成 (allowlist in deny.toml) |
| REQ-VFY-004 | Verification | `cargo miri test`(轻量子集)0 UB | `tools/ci/miri.sh` | 新增 |
| REQ-VFY-005 | Verification | `cargo kani` 0 unknown(LLM HTTP 抽离线验证) | `tools/ci/kani.sh` | 新增 |
| REQ-VFY-006 | Verification | 代码覆盖率 ≥ 80%(`cargo llvm-cov`) | `tools/ci/coverage.sh` | 新增 |
| REQ-VFY-007 | Verification | 形式化 SRS 追溯矩阵(`tools/ci/srs_trace.py`) | 该脚本输出非空 | 新增 |
| REQ-DOC-001 | Documentation | 文档与代码 100% 一致(`tick_hal` 之类的死引用清零) | `docs_lint.sh` | GAP-004 |
| REQ-DOC-002 | Documentation | 每个 `REPLACE` / `NEW` 都必须在 commit msg 中带 `REQUIREMENT=REQ-…` | `srs_trace.py` | 新增 |

## Traceability Matrix

`tools/ci/srs_trace.py` walks the tree and emits `docs/SRS_TRACE.md` with the
forward (requirement → implementation site) and backward (implementation site
→ requirement) tables. Run it locally in < 2 s and commit any new SRS entries
together with the code that satisfies them.

## Verification Schedule

| Tier | Tooling | Trigger | Exit |
|---|---|---|---|
| 1 | `cargo test` | 每次 commit | 0 fail |
| 2 | `cargo clippy -- -D warnings` | 每次 PR | 0 warn |
| 3 | `cargo deny check` | 每次 PR | 0 advisory |
| 4 | `cargo miri test -p magent-core` | 每晚 + PR | 0 UB |
| 5 | `cargo llvm-cov --workspace` | 每周 | ≥ 80% |
| 6 | `espflash monitor` | 每次合并 | 启动日志无 panic |
| 7 | Hardware fuzz (`cargo-fuzz`) | 每次发布 v0.2+ | 0 crash in 1h |
