# mAgent(AI 智能体芯片)· Executive Summary

> **一句话定位**: 把大模型驱动的 ReAct AI Agent 塞进 256 KB RAM / 1 MB Flash 的 MCU, 同时满足航级软件工程标准(零 panic、有限内存、有限时间、有界执行)。

---

## 我们解决的问题

传统 AI Agent 框架(LangChain、AutoGen …) 跑在云端,假设"内存无限、进程可重启"。而航空、航天、工业、医疗场景要求的是:

| 维度 | 传统 Agent | MCU 航级要求 |
|---|---|---|
| 内存 | GB 级 | **256 KB** |
| 异常 | 进程崩溃由 systemd 重启 | **零 panic** · 全路径 `Result` |
| 栈空间 | OS 动态分配 | 主任务 ≤ **8 KiB**, 其余 ≤ **4 KiB** |
| OTA | 回滚由 OS 负责 | bootloader **原子回滚** + Secure Boot v2 + Flash 加密 |
| 实时性 | 尽力而为 | **有界执行时间** + 迭代步数预算 |

**mAgent = 重新设计的 Agent 内核**, 不是把 Python Agent 移植到单片机。

---

## 核心数字(已实测)

| 平台 | 架构 | RAM | Flash | 二进制 | 实测状态 |
|---|---|---|---|---|---|
| **nRF52840** | ARM Cortex-M4F @ 64 MHz | 256 KB | 1 MB | **194 KB**(占用 18.9%) | ✅ Ready(可穿戴主推) |
| **ESP32-C61** | RISC-V 32-bit @ 160 MHz | 320 KB + 512 KB PSRAM | 8 MB | **607 KB**(占用 7.4%) | ✅ Ready(联网) |

**一份代码 → 双芯片**:`magent-core` 通过正交 feature flag( `arch-cortex-m` / `arch-riscv` / `arch-xtensa`)让同一份 Agent 代码既能编译进 nRF52840 裸机固件, 也能编译进 ESP32-C61 固件, 还能跑在桌面模拟器对接 Ollama。

---

## 5 大差异化(Why mAgent?)

1. **航级安全**: 全 `Result` 零 panic、有限预算(内存/栈/迭代/时间)、Watchdog、Flash wear-leveling 寿命延长 10×、`SRS` 需求矩阵可追溯。
2. **ReAct on MCU**: `magent-core/src/agent.rs` 实现 `Think → Tool Call → Observe → Repeat` 状态机, 805 行, 严格受 Budget 约束。
3. **双架构原生**: ARM + RISC-V 一套代码, 不用为每颗芯片重写。
4. **可插拔 Ingress**: `LinkAdapter` 把 BLE / MQTT / stdin 抽象成同一接口, 配合 Ed25519 `SignedMessage` 做端到端完整性。
5. **完整工程化**: CLI、桌面模拟器、6 个端到端示例、4 个 MCP Server、Miri/Kani/llvm-cov 验证闭环。

---

## 典型落地场景

| 场景 | 平台 | 价值 |
|---|---|---|
| 可穿戴健康监测(心率 / 睡眠 / 运动教练) | nRF52840 | 离线 Agent 实时分析 · BLE 直连手机 · 1MB Flash 装 6 个 Skill |
| 工业 IoT 网关(LLM 诊断) | ESP32-C61 | Wi-Fi 6 + 8MB Flash · PSRAM 给 LLM RPC 缓冲 |
| 航天载荷边缘 AI | nRF52840 / ESP32-C61 | 满足 ECSS-E-ST-40C · 内存可预测 · 全路径无 panic |
| 远程医疗设备 | nRF52840 | 网络中断仍可本地决策 · 触觉 + 语音反馈 |
| 资产追踪 + Web3 身份 | nRF52840 | `did:key` 链上身份 · 离线签名 |

---

## 一句话差异化

**唯一同时满足"MCU 裸机 + 航级安全 + 真正 ReAct Agent + LLM RPC + ARM/RISC-V 双架构"的开源项目。**

- **仓库**: https://github.com/arksong/magent
- **许可**: MIT · 版本 0.1.0(2026-08)
- **技术栈**: Rust(`no_std`) · Embassy · esp-rs · TLS(aws-lc-rs · mbedTLS)
- **更多细节**: 详见 `docs/PROJECT_OVERVIEW.md`(完整白皮书)

---

*扉页用 · 单页 · A4 / 16:9 PPT 均可直接套用 · 数据截至 2026-08-23*
