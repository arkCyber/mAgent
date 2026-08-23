# mAgent(原名 MicroAgent)—— 航级安全嵌入式 AI 智能体芯片解决方案

> **一句话定位**:
> 把大语言模型驱动的 **ReAct 智能体**, 塞进 256 KB RAM / 1 MB Flash 的 MCU 里, 同时满足航级软件工程标准(无 panic、有限内存、有限时间、有界执行)。

---

## 1. 项目愿景

在大模型时代,大多数 AI Agent 框架( LangChain、AutoGen、CrewAI …)都跑在云端服务器或带 Linux 的边缘盒子上,假设"内存无限、文件系统稳定、进程可以随时被 kill 重启"。这与**航空、航天、工业控制、医疗器械**等场景对软件的要求完全背道而驰:

| 维度 | 传统 Agent 框架 | 嵌入式 / 航级要求 |
|---|---|---|
| 内存 | GB 级 | KB 级(nRF52840 仅有 256 KB) |
| 异常处理 | 进程崩溃后由 supervisord 重启 | **不允许 panic**, 必须 `Result` 全路径 |
| 栈空间 | 由 OS 动态分配 | 主任务 ≤ 8 KiB, 其余 ≤ 4 KiB |
| OTA | 失败回滚由 systemd 负责 | 必须由 bootloader 原子回滚 |
| 网络 | TLS 默认 OS 证书 | 证书常驻 ROM,签名固件 + Flash Encryption |
| 实时性 | 尽力而为 | **有界执行时间**, 每次 ReAct 迭代有步数预算 |

**mAgent 的目标,就是用 Rust + Embassy + 严格的安全约束,在裸机(MCU bare-metal,无 OS)上跑出与云端等价能力的 AI Agent**。它不是把 Python Agent 移植到单片机,而是**重新设计**一套适合 MCU 的 Agent 内核。

---

## 2. 核心亮点(Why mAgent?)

### 2.1 航级安全(Aerospace-Grade Safety)

- **零 Panic 路径**: 全代码 `Result<T, E>` 全程传播, `unwrap()` / `expect()` 在 release build 全部 lint 禁止(`panic_in_result_fn = deny`)。
- **有限预算(Budget)**: 内存预算、栈深度预算、迭代步数预算、单步时间预算,任一超界立即 `Err`,不静默"假装成功"。
- **看门狗 + 故障分类**: `Watchdog` 守护主循环,`Fault` 分类(瞬态/永久/可恢复),自动降级或安全停机。
- **Flash 磨损均衡**: 动态/静态/混合三种 wear-leveling 策略,Flash 寿命延长最高 10×;扇区擦写次数达阈值前绝不标记 worn-out。
- **可追溯的需求矩阵**: 每条 `REQ-XXX-NNN` 可追溯到代码位置与验证脚本,对照 DO-178C / ECSS-E-ST-40C / NASA NPR 7150.2 / MISRA-Rust 2024。

### 2.2 双架构芯片原生支持

| 平台 | 架构 | 频率 | RAM | Flash | 无线 | 状态 |
|---|---|---|---|---|---|---|
| **nRF52840** | ARM Cortex-M4F | 64 MHz | 256 KB | 1 MB | BLE 5.3 / Thread / Zigbee / 802.15.4 | ✅ Ready(主推,可穿戴) |
| **ESP32-C61** | RISC-V 32-bit | 160 MHz | 320 KB + 512 KB PSRAM | 8 MB | Wi-Fi 6 + BLE 5.3 | ✅ Ready(联网) |
| ESP32-C3 / C6 | RISC-V 32-bit | — | — | — | 同 C61 | 🔄 兼容(沿用 C61 配置) |
| ESP32 / S3 | Xtensa LX6/LX7 | — | — | — | — | 🔄 进行中(需 Xtensa 工具链) |

二进制实测: nRF52840 **194 KB**( Flash 占用 18.9% / RAM ~2.3%), ESP32-C61 **607 KB**( Flash 占用 7.4%)。

### 2.3 芯片无关的 ReAct 内核

`magent-core` 是一个 **`no_std` 默认**的库,通过**正交 feature flag**(`arch-cortex-m` / `arch-riscv` / `arch-xtensa` / `nrf52` / `esp32` / `ble` / `wifi` / `web3` / `std`)让**同一份 Agent 代码**既能编译进 nRF52840 裸机固件,也能跑在 x86_64/macOS 的桌面模拟器,还能对接 ESP32 上的 LLM RPC。

- **ReAct 状态机**: `Think → Tool Call → Observe → Repeat`,在 `magent-core/src/agent.rs`(805 行)中实现,迭代步数严格受 Budget 约束。
- **Skill & Tool Registry**: 技能以 JSON 描述注册、Flash 持久化;工具以 `fn() -> Result<Json, _>` 描述,运行时可裁剪。
- **可插拔 Link Adapter**: `LinkAdapter` trait 把 BLE / MQTT / stdin 抽象成同一种"外部数据入口",配合 `IngressGateway` 把外部消息路由进 Agent loop,可选地包成 `web3::SignedMessage`(Ed25519 签名)做端到端完整性校验。

### 2.4 安全 + Web3 可选能力

- **BLE 加密**: AES-128/256 CCM,证书配对,消息级认证标签。
- **Web3 身份**: Ed25519 密钥 + `did:key` multibase,链上身份可移植。
- **加密 Keystore**(CLI 侧): ChaCha20-Poly1305 AEAD + Argon2 KDF,passphrase 派生密钥,文件落盘加密。
- **mbedTLS / aws-lc-rs** 适配 RISC-V 32-bit 目标(为 ESP32-C6/C61 替换 ring 而打补丁)。

### 2.5 完整的工程化闭环

- **CLI**: `magent run …` 直接对接 Ollama(或内置 mock 后端)做端到端跑通。
- **桌面模拟器**: `host/simulator` 与 `host/nrf52-simulator` 让无硬件用户也能体验 Agent 行为。
- **示例库**(`examples/`): 6 个独立 crate,每个都自带 README + 断言式端到端用例:
  - `wearable-pipeline` 心率 → MQTT + 邮件 + 摘要分流
  - `event-router` 多协议事件路由
  - `topic-watcher` 头/尾压缩
  - `email-mqtt-bridge` SMTP → MQTT
  - `mqtt-roundtrip` 真实线缆级 MQTT 3.1.1 回环
  - `magent-mqtt-ingest` rumqttc → MqttAdapter → IngressGateway(签名模式)
- **MCP 工具**: `host/email-mcp`、`host/mqtt-mcp`、`host/mcp-tool-executor` 让 Agent 真正调用外部能力(发邮件、发 MQTT 消息)。
- **CI 工具链**: `cargo test` + `cargo clippy -- -D warnings` + `cargo deny check` + `cargo miri test` + `cargo llvm-cov`(≥ 80% 覆盖率)+ 形式化 SRS 追溯脚本。

---

## 3. 仓库结构(Workspace 视图)

```
MicroAgent/
├── Cargo.toml                    # 虚拟 workspace,统一 Lint / Profile / Patch
├── magent-core/                  # ★ 芯片无关的 ReAct 内核 (no_std 默认, ~20 个模块)
│   ├── agent.rs                  #   ReAct 状态机 + MiniAgent
│   ├── agent_runner.rs           #   完整 runner + LLM 接入(3172 行)
│   ├── skills.rs / tools.rs      #   技能/工具注册表
│   ├── safety.rs                 #   预算、fault、watchdog
│   ├── ollama.rs                 #   LLM 后端
│   ├── web3/                     #   Ed25519 + did:key
│   ├── communication/            #   LinkAdapter (BLE/MQTT/manual)
│   └── …(health_sensors、sports_coach、sleep_manager、early_warning、voice_notification)
│
├── magent-hal/                   # HAL 抽象(nRF52 / ESP32 适配, 与 core 解耦)
│
├── firmware/                     # 固件: 一个 crate 一颗芯片
│   ├── nrf52-app/                #   nRF52840(ARM Cortex-M4F)固件 ✅
│   ├── esp32-app/                #   ESP32-C61(RISC-V)固件 ✅
│   └── integration-test/         #   nRF52 真机端到端测试
│
├── host/                         # 主机端: 模拟器 + MCP 工具
│   ├── simulator/                #   桌面 Agent 模拟
│   ├── nrf52-simulator/          #   nRF52 行为级模拟
│   ├── email-mcp/                #   Email MCP Server
│   ├── mqtt-mcp/                 #   MQTT MCP Server
│   └── mcp-tool-executor/        #   MCP 工具执行器
│
├── cli/                          # `magent run …` 命令行
├── tools/                        # 基准测试 / 算法 demo / E2E
├── examples/                     # 6 个独立端到端示例
├── docs/                         # 23 份技术文档
│   ├── SRS.md / SRS_TRACE.md     #   需求追溯矩阵
│   ├── ARCHITECTURE.md           #   架构说明
│   ├── PLATFORM_COMPARISON.md    #   平台对比
│   ├── HARDWARE.md               #   硬件集成
│   ├── NRF52_BUILD_GUIDE.md      #   nRF52 构建
│   ├── ESP32_C61_BUILD.md        #   ESP32-C61 构建
│   └── …(MQTT / LLM / API / Security / Audit 等)
│
├── .cargo-patches/               # 6 个本地 patch(esp-wifi、esp-idf-sys、rustls、… 适配 RISC-V)
├── build.sh / flash.sh / test.sh # 一键构建 / 烧录 / 测试
└── ollama_test.py                # Python 联调脚本
```

---

## 4. 技术架构

### 4.1 分层模型

```
┌──────────────────────────────────────────────────────────────┐
│  Application Layer   │  技能 JSON / Tool JSON / 用户脚本     │
├──────────────────────────────────────────────────────────────┤
│  Agent Runtime       │  ReAct 状态机 · MiniAgent · Budget    │
│                      │  (magent-core/src/agent.rs)          │
├──────────────────────────────────────────────────────────────┤
│  Ingress Gateway     │  LinkAdapter(BLE/MQTT/Manual)         │
│                      │  SignedMessage(Ed25519) 验签          │
├──────────────────────────────────────────────────────────────┤
│  HAL 抽象            │  Gpio / Flash / Ble / Sensor / Power  │
│                      │  (magent-hal)                        │
├──────────────────────────────────────────────────────────────┤
│  芯片驱动            │  embassy-nrf / esp-idf-svc           │
│                      │  (firmware/nrf52-app, esp32-app)      │
├──────────────────────────────────────────────────────────────┤
│  裸机硬件            │  nRF52840 / ESP32-C61                │
└──────────────────────────────────────────────────────────────┘
```

### 4.2 Feature Flag 矩阵

| Feature | 引入依赖 | 适用场景 |
|---|---|---|
| (default) | (无) | CI / 文档 / 静态检查 |
| `std` / `host` | reqwest + serde/std | 桌面测试 (x86_64 / Linux / macOS) |
| `arch-cortex-m` | cortex-m / cortex-m-rt | 任何 ARM Cortex-M 芯片 |
| `arch-riscv` | riscv / riscv-rt | RISC-V 芯片 (ESP32-C3 / C6) |
| `arch-xtensa` | xtensa-lx / xtensa-lx-rt | Xtensa 芯片 (ESP32 / S3) |
| `nrf52` | arch-cortex-m + embassy-nrf + nrf-softdevice | nRF52840 固件 |
| `esp32` | arch-riscv | ESP32 系列固件 (RISC-V 默认) |
| `ble` / `wifi` / `thread` | (marker) | 通信能力开关 |
| `monitoring` | (marker) | 健康监测钩子 |

设计原则: **架构(Cortex-M / RISC-V / Xtensa)与芯片家族(nRF52 / ESP32)解耦**,任意组合,不绑死单一供应商。

### 4.3 一次编译、双端运行

```bash
# 同一份 magent-core 代码 → nRF52840 固件
cargo build -p magent-nrf52-app --target thumbv7em-none-eabihf --release

# 同一份 magent-core 代码 → ESP32-C61 固件
MCU=esp32c6 cargo build -p magent-esp32-app --release

# 同一份 magent-core 代码 → 桌面模拟器(对接 Ollama)
cargo build -p magent --features std,web3 --release
```

### 4.4 内存预算与栈隔离

- **主任务栈 ≤ 8 KiB**, 其余任务 ≤ 4 KiB(在 `sdkconfig.defaults` 强制)。
- **所有数据结构优先 heapless**( `heapless::Vec / String<N>`, N 由编译期 budget 决定)。
- **`Box` / `Vec` / `String` 禁止在 ISR 中分配**(`large_types_passed_by_value` lint)。
- **全局可变量**必须 `Atomic` 或 `Mutex`(`mutable_key_type` lint)。
- **栈深度监控**在每个 ReAct 迭代后采样,超阈值立即 `Err` 而非继续。

### 4.5 OTA 与安全启动

- **Secure Boot v2** 默认开启(ESP32): 签名固件,防止恶意刷机。
- **Flash Encryption** 默认开启: 落盘数据自动加密。
- **OTA 升级失败自动回滚**: 由 ESP-IDF bootloader 在下次启动时检测并回退到上一分区。
- **BLE 配对证书** + **消息级认证标签**: 防止中间人攻击与重放。

---

## 5. 典型应用场景

| 场景 | 平台 | 价值 |
|---|---|---|
| **可穿戴健康监测**(心率 / 睡眠 / 运动教练) | nRF52840 | 离线 Agent 实时分析, BLE 直连手机, 1 MB Flash 装得下 6 个 Skill + Tool 套件 |
| **工业 IoT 网关**(传感器聚合 + LLM 诊断) | ESP32-C61 | Wi-Fi 6 + 8 MB Flash 容纳更复杂的多步推理;PSRAM 给 LLM RPC 缓冲 |
| **远程医疗设备**(采集 + 紧急预警) | nRF52840 | 触觉反馈 + 语音播报, 紧急情况本地决策(网络中断仍可工作) |
| **航天载荷边缘 AI**(在轨推理) | nRF52840 / ESP32-C61 | 满足 ECSS-E-ST-40C 软件工程要求, 全路径无 panic, 内存可预测 |
| **资产追踪 / 数字身份**(BLE + Ed25519) | nRF52840 | `did:key` 身份在链上可验证, 离线签名 |
| **多协议事件路由**(MQTT / Email / 摘要) | 桌面 / ESP32 | 6 个 example 给出参考实现, 1 小时跑通端到端 |

---

## 6. 开发者体验

### 6.1 三步上手

```bash
# 1) 安装目标
rustup target add thumbv7em-none-eabihf      # nRF52840
rustup target add riscv32imac-esp-espidf     # ESP32-C61

# 2) 安装工具
cargo install probe-rs espflash cargo-binutils

# 3) 编译(任选一颗芯片)
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
# 或
MCU=esp32c6 cargo build -p magent-esp32-app --release
```

### 6.2 端到端 demo(< 5 分钟)

```bash
# 跑可穿戴管线示例(无需硬件)
cd examples/wearable-pipeline && cargo run --release
# 看到 ✅ 5/5 assertions passed 即通过

# 跑 MQTT 签名回环
cd examples/magent-mqtt-ingest && cargo run --release
# 看到 ✅ ingress + verify 通过即通过
```

### 6.3 真机烧录

```bash
# nRF52840 DK
probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/magent-nrf52-app

# ESP32-C61 DevKit
espflash flash --monitor target/riscv32imac-esp-espidf/release/magent-esp32-app
```

---

## 7. 路线图与差异化

| 维度 | mAgent | 传统嵌入式 AI(厂商封闭) | 云端 Agent 框架 |
|---|---|---|---|
| 部署形态 | MCU 裸机 | MCU 裸机 | 服务器 / 边缘盒 |
| 内存下限 | **256 KB** | ~512 KB | GB 级 |
| 编程语言 | Rust(`no_std` 友好) | C / C++ | Python / Node |
| 异常处理 | **全 `Result`,零 panic** | 多 `assert` | try/except 静默 |
| 需求追溯 | **REQR-矩阵 → 代码 → CI** | 私有 | 不适用 |
| LLM 后端 | Ollama / 自定义 RPC | 无 | OpenAI / Anthropic |
| 双架构 | **ARM + RISC-V 一套代码** | 厂商各一套 | 不适用 |
| 许可 | MIT | 商业 / NDA | 多为商业 |

**核心差异化**: **唯一同时满足"MCU 裸机 + 航级安全 + 真正 ReAct Agent + LLM RPC + 双架构"的开源项目。**

---

## 8. 合规与质量

- **DO-178C / ED-12C**: 软件等级 A–E → `REQ-SAFE-*` 映射
- **ECSS-E-ST-40C**: 软件工程要求 → `REQ-*` 全覆盖
- **NASA NPR 7150.2**: 静态规则 → `REQ-SAFE-001 / 003`
- **MISRA-Rust 2024**: 编码规则 → workspace `clippy::` lint 强制
- **Embedded Rustacean's "Don't" List**: FFI/安全 → `REQ-SAFE-001 / REQ-DOC-001`
- **代码覆盖率**: ≥ 80%(`cargo llvm-cov` 每周验证)
- **形式化追溯**: `tools/ci/srs_trace.py` 输出 `docs/SRS_TRACE.md`
- **CI 流水线**: `cargo test` → `cargo clippy -- -D warnings` → `cargo deny check` → `cargo miri test` → `cargo kani` → `cargo llvm-cov`

---

## 9. 常见问题(FAQ)

**Q1: 为什么是 Rust 而不是 C/C++?**
A: 所有权 + 借用检查器在编译期就消除了大量内存安全 bug; `Result` 让"全路径错误处理"成为强制习惯; `no_std` 友好, 与 Embassy 生态无缝集成。

**Q2: 单片机能跑得动大模型吗?**
A: Agent 内核本身只有 ~20 KB, LLM 推理在云端 / 局域网 PC; 芯片负责"决策调度 + 工具调用 + 协议接入"。如果未来要把 LLM 也搬上 MCU, 我们已经有 `web3` feature 走签名 + 压缩的轻量化路径。

**Q3: 与 Embassy / RTIC 的关系?**
A: 我们基于 Embassy 异步运行时, 但不依赖其 RTOS 抽象(因为是裸机), 而是把 Embassy 的 `embassy-nrf` / `embassy-time` 当成 HAL 用。

**Q4: 商业项目能用吗?**
A: MIT 协议, 自由使用; 商业闭源集成欢迎, 但请保留版权与许可声明。

**Q5: 是否支持 Thread / Zigbee?**
A: nRF52840 原生支持 802.15.4, 已通过 `thread` feature 标记; Zigbee 在路线图上。

---

## 10. 联系与贡献

- **仓库**: https://github.com/arksong/magent
- **许可**: MIT
- **版本**: 0.1.0(详见 `CHANGELOG.md`)
- **贡献**: 欢迎 PR, 请先阅读 `magent-core/src/safety.rs` 的安全准则与 `CONTRIBUTING.md`
- **问题反馈**: GitHub Issues / Discussions

> **致谢**: 感谢 Embassy、esp-rs、probe-rs、rustls、ed25519-dalek 等开源社区, 让裸机 AI Agent 成为可能。

---

*本文档由项目维护者审阅, 数据来源于 `README.md`、`docs/SRS.md`、`docs/ARCHITECTURE.md`、`docs/PLATFORM_COMPARISON.md`、根 `Cargo.toml` 与 `magent-core/src/lib.rs`, 截至 2026-08-23 仍保持准确。*
