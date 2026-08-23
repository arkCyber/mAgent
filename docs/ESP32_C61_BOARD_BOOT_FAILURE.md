# ESP32-C61 启动问题诊断与修复记录（最终成功）

> **日期**: 2026-08-23
> **板子**: ESP32-C61-DevKitC-1-N8R2
> **状态**: ✅ **固件编译、烧录、启动全部成功，agent/ingress 线程正常运行**

---

## 一、最终结论

1. **板子本身没有硬件故障。**
2. 之前的"TG0 WDT 复位循环、ROM 卡死"是 **C61 原生 USB-JTAG 接口问题**：通过该接口（`303A:1001`，`/dev/cu.usbmodem101`）烧录后 ROM 在早期启动阶段挂起。改用 **CP2102N USB-UART 桥**（`10C4:EA60`，`/dev/cu.usbserial-10`）后，ROM 能正常加载 bootloader 和 app。
3. 固件成功启动后，agent 线程与 ingress 线程都正常启动并持续运行。

---

## 二、启动成功的完整日志（节选）

```
I (474) magent_esp32_app: [magent] v0.1.0 booting (esp-idf-svc 0.52 / ESP32-C61 std)
I (475) [magent] boot phase 1/8: logger up
I (503) [magent] identity loaded from NVS (pubkey=1cfe85e55b299de8...)
I (511) [magent] boot phase 4/8: peripherals ready
I (626) [magent] boot phase 5/8: esp_wifi ready
I (645) [magent] boot phase 7/8: wifi done
I (651) [agent] thread starting
I (655) [agent] MiniAgent ready
I (660) [agent] result: Task completed successfully
I (666) [ingress] thread starting
I (677) [ingress] gateway ready
I (689) [magent] all threads running
I (694) [magent] boot phase 8/8: all systems nominal
```

`[ingress] error: ingress gateway has no adapters registered` 是 **dummy 模式**的正常提示（Cargo.toml 未启用 `uart` feature），不是故障。

---

## 三、排查/修复历程（从失败到成功）

### 阶段 1：build + flash 通过，但板子不启动
- 通过原生 USB-JTAG 烧录后，串口反复输出 `rst:0x7 (TG0_WDT_HPSYS)`，PC `0x40036716`（ROM 区），**从未进入 bootloader**。
- 用默认 PlatformIO hello_world、单独 bootloader、擦除后重烧，结果都一样 → 曾误判为"硬件故障"。
- **真相**：这是 **USB-JTAG 接口问题**，与固件无关。

### 阶段 2：换 CP2102 UART 桥后，ROM 正常加载
- ROM 输出变为 `rst:0x3 (RTC_SW)` + `load:...` + `entry 0x4083b970`，能加载并进入 bootloader。
- 但 bootloader 因 **app 超出默认 factory 分区（1MB）** 而失败：
  `Image length 1201376 doesn't fit in partition length 1048576`
- **修复**：启用自定义分区表（factory 0x180000 = 1.5MB）。

### 阶段 3：应用启动早期崩溃（逐一修复）
| 崩溃 | 根因 | 修复 |
|------|------|------|
| `Store access fault` @0x3f400000 | `diag_marker` 写错 UART0 地址 | 移除 `app_main` 里的 `diag_marker` 调用 |
| `Load access fault` @0x80（`__retarget_lock_acquire_recursive`） | bindings(picolibc) 与 libc(newlib) 的 `FILE`/`_REENT` 结构不匹配 | sdkconfig 设 `CONFIG_LIBC_PICOLIBC=y`（自动发出 `esp_idf_libc_picolibc` cfg） |
| `Stack protection fault`（pthread 任务） | agent 16KB / ingress 8KB 栈溢出 | agent 栈→64KB，ingress 栈→32KB |
| agent panic：`max_memory` OutOfRange | `with_max_memory(512KB)` 超上限 256KB | 改为 128KB |

---

## 四、构建/烧录要点（已验证可用）

```bash
# 1) 构建（必须在固件目录）
cd firmware/esp32-app
export MCU=ESP32C61 RUSTC_BOOTSTRAP=1
source ~/export-esp.sh
cargo build --release

# 2) 生成 app bin
esptool.py --chip esp32c61 elf2image --flash_size 8MB \
  target/riscv32imac-esp-espidf/release/magent-esp32-app \
  -o target/riscv32imac-esp-espidf/release/magent-esp32-app.bin

# 3) 生成自定义分区表（bootloader/partitions.bin 在 esp-idf-sys 构建 out 下）
python3 <framework>/components/partition_table/gen_esp32part.py \
  firmware/esp32-app/partitions.csv target/.../custom-partitions.bin

# 4) 完整烧录（经 CP2102 UART 桥 /dev/cu.usbserial-10）
esptool.py --chip esp32c61 --port /dev/cu.usbserial-10 --baud 460800 write_flash \
  0x0 <bootloader.bin> 0x8000 <custom-partitions.bin> 0x10000 <magent-esp32-app.bin>

# 5) 复位并读串口
esptool.py --chip esp32c61 --port /dev/cu.usbserial-10 --after hard_reset run
# 用 python3 pyserial 读取 /dev/cu.usbserial-10 @115200 观察日志
```

### 重要：使用 CP2102 UART 桥而非原生 USB-JTAG
- 原生 USB-JTAG 端口：`/dev/cu.usbmodem101`（`303A:1001`）→ 此板不可用（ROM 挂起）
- **CP2102 UART 桥**：`/dev/cu.usbserial-10`（`10C4:EA60`）→ 正常工作

---

## 五、需要后续跟进（非阻塞）

1. **UART ingress**：✅ 已启用并验证真实收发 —— ingress 线程注册 UART0，收到字节后以设备 Ed25519 身份签名生成可验证信封（`signed envelope: {"signer": did:key:...}`）。
   ⚠️ **回环噪声已修复**：之前 ingress 收到几百字节的"控制台日志回环"。根因是 **UART0 TX/RX 引脚接反**——C61 的 UART0 是 **TX=GPIO11、RX=GPIO10**（见 `soc/esp32c61/include/soc/uart_pins.h`），代码误传 `tx=gpio10, rx=gpio11`，导致 ingress 读到控制台 TX 引脚。已改为 `tx=gpio11, rx=gpio10`，实测只收到真实发送的数据（14B `hello-clean-42`）。
2. **Wi-Fi 联机**：固件已正确读取/写入 `arkSong@iPhone`，但关联失败 —— 需在 iPhone 热点开启"最大兼容性"（2.4GHz）后重测。
3. **重试 PSRAM**：✅ 已重开成功 —— `CONFIG_SPIRAM=y`，检测到 2MB PSRAM（80MHz、ECC），heap 从 ~176KB 提升到 **~1.9MB**（1792KB PSRAM + 161KB 内部）。agent `max_memory` 已提到 256KB（上限）。
   ⚠️ **注意**：线程栈分配在**内部 RAM**（不在 PSRAM）。启用 PSRAM 后内部 RAM 减少，agent 64KB + ingress 32KB 栈会 OOM；已调为 agent 48KB + ingress 24KB。后续若内存紧张可再调，或考虑把任务栈路由到 PSRAM（`CONFIG_FREERTOS_TASK_CREATE_ALLOW_EXT_MEM`）。

4. **agent 本地工具（无网络）**：✅ 已实现 ——
   - magent-core `ToolRegistry` 新增 `ToolHandler` 真实硬件钩子（`tools.rs`），`MiniAgent` 通过 `set_tool_handler` 注入。
   - 固件 `local_tools.rs` 的 `Esp32ToolHandler`：`write_gpio` 驱动真实 GPIO，`read_sensor temperature` 读内部温度传感器。
   - **UART 命令 → ingress → agent → 真实硬件**通道已打通（共享任务句柄）。
   - 修复 magent-core bug：`MiniAgent::run()` 未重置 `state`，导致 agent 只能跑一次任务（后续返回 "No result available"）。已修复为可复用。
   - 改进 `observe()`：最终结果包含真实工具返回值（如 `temperature=34.4 C`、`GPIO13 set to high`）。
   - **实测**：`read the temperature` → `temperature=34.4 C`；`turn on the led` → `GPIO13 set to high (err=0)`。magent-core 279 个单元测试通过。

5. **可靠性 & 错误处理加固**：✅ 已完成 ——
   - **WiFi 初始化非致命化**：抽出 `setup_platform()`，事件循环/外设/NVS/EspWifi/BlockingWifi 任一失败都只记警告并跳过，**固件照常启动并运行 agent/ingress 线程**（本地工具 + UART 不需要网络）。
   - **`connect_wifi` 不再 panic**：`set_configuration`/`start` 失败改为记警告并返回（原来 `.expect()` 会重启整板）。
   - **线程 spawn 非致命**：spawn 失败记错误并继续（主忙循环喂看门狗），句柄用 `Option`。
   - **agent 循环 `catch_unwind`**：单个任务/tool 执行 panic 不会杀死 agent 线程，记错误后继续服务下一条命令。
   - `MiniAgent::new` 失败优雅处理（记错误 + 重试）。
   - 剩余 8 个 `.expect()` 均在**安全路径**（编译期常量：agent 名称/迭代/内存；或不可达硬件：TRNG/固定 32 字节 Ed25519 seed）。

6. **崩溃自动重启恢复（看门狗 + 崩溃循环检测）**：✅ 已完成 ——
   - **看门狗**：主忙循环持续喂 TG0 看门狗；ESP-IDF panic 处理器已自动重启（"Rebooting..."）。
   - **崩溃循环检测 + 安全模式**：NVS `boot_count` 计数器记录连续启动次数；连续 3 次快速重启 → 判定崩溃循环 → 进入**安全模式**（跳过 WiFi，保留 agent/ingress/UART 供诊断）；运行稳定 60s 后重置计数器。
   - **实测闭环**：3 次快速重启 → 安全模式（`boot #1→#2→#3` + `safe mode, skipping Wi-Fi`，但 agent/ingress/UART 照常）→ 60s 稳定 → `boot considered stable` → 重启 `boot #1` 恢复正常。
   - **agent 持续运行真实温度读取**（32-33°C），无崩溃。

7. **magent-core recovery.rs / safety.rs 审计**：✅ 已完成 ——
   - **发现 `recovery.rs` 是孤儿模块**（未在 `lib.rs` 声明，`RecoveryManager` 是死代码）→ 已接入 crate。
   - **修复 `execute_with_retry` 退避未生效**（`let _delay = ...` 只是占位）：新增 `set_delay` 延迟钩子，`RetryWithBackoff` 现在真正按指数退避等待（上限 5s）。
   - **修复 `get_strategy` 非穷尽 match**：补齐 `BufferOverflow`/`StackOverflow`/`OperationTimeout`/`InvalidStateTransition`/`Web3Error`/`Unknown`。
   - **新增测试**：`backoff_is_exponential_and_capped`、`retry_with_backoff_applies_delay_hook`。
   - magent-core 单元测试 **281 通过**（原 279 + 2 新增）。

8. **健康监控 & 稳定性加固**：✅ 已完成 ——
   - **心跳检测**：agent/ingress 线程每轮更新 `Heartbeat`；主循环检测心跳是否超过 15s 陈旧（检测线程**挂起**而非崩溃，panic 处理器抓不到）。
   - **健康指标日志**：agent 每 ~60s 记录 `[health] agent alive — uptime, free_heap`，空闲堆 < 64KB 时告警（`LOW FREE HEAP`）。
   - **修复真实 bug（心跳暴露）**：`esp-idf-hal` 的 `UartDriver::read` 用 `delay::BLOCK` **无限阻塞**等待数据，导致 ingress 线程卡死（`embedded_io::Read` 阻塞）。改为 `UartAdapter::poll` 先用 `remaining_read()` 检查 RX 是否有数据，无数据立即返回 `Ok(0)` → **ingress 不再挂起**，心跳恢复正常。
   - **实测**：`[health] agent alive — uptime 60082 ms, free_heap 1909291 B`；`stale=False`（两线程心跳正常）；`crash=False`。

9. **双向通信（agent 结果回传主机）**：✅ 已完成 ——
   - magent-core `IngressGateway` 新增 `send_to_adapter(index, data)`（+ `IngressError::AdapterSendFailed`），可向指定 link 适配器发送回复。
   - 固件新增共享 `reply_outbox`：agent 执行完**来自 UART 命令**的任务后把结果写入回复箱；ingress 线程每轮清空回复箱并经 `gw.send_to_adapter(0, ...)` 通过 UART 回传给 host。
   - **实测闭环**：host 发 `turn on the led` → 板子执行 → host 收到原始回复帧 `RESULT[turn on the led]: Task: Tool result: GPIO13 set to high (err=0)`。







