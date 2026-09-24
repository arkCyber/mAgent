# mAgent 板卡配置（编译开关）

同一份固件源码通过 **Cargo feature 编译开关** 同时支持两块芯片：

| 板卡 | Cargo feature | 目标 target | CPU | 内部 SRAM | PSRAM |
|---|---|---|---|---|---|
| **ESP32-C61** (默认) | `board-c61` | `riscv32imac-esp-espidf` | RISC-V 160MHz | ~134KB 动态 | 2MB |
| **ESP32-S3** | `board-s3` | `xtensa-esp32s3-espidf` | Xtensa 240MHz | ~390KB 动态 | 8MB |

## 为什么需要 S3
C61 内部 SRAM 动态区仅 ~134KB，WiFi + agent 已接近上限。**S3 有 ~390KB 内部动态堆 + 8MB PSRAM，可以同时跑 WiFi(DeepSeek) + agent + Lua App**，端到端解决智能体的云端对话问题。

> BLE 已于 2026-09-20 从固件中整体移除（Bluedroid 控制器需要 ~244KB
> **连续、可 DMA 的内部 DRAM**，两块板卡在 WiFi + agent 并存时都无法提供；
> 详见 `docs/BACKLOG.md`「BLE removed outright」）。`AT+BLE=…` 现在统一返回
> `+CMDER:9`。因此板卡选择的差别不再包含 BLE。

## 代码中的编译开关
- `firmware/esp32-app/Cargo.toml` → `[features]`：`board-c61`（默认）/ `board-s3`，同时联动 `esp-println` 的芯片 feature。
- `src/main.rs` → 启动日志与 agent 名称按板卡切换；`board-s3` 决定 `lua` 运行时与 PSRAM 配置。
- `src/core_affinity.rs` → `#[cfg(feature = "board-s3")]`：S3 是双核，核心亲和 / 优先级档位按板卡切换；C61 单核走另一分支。
- `sdkconfig.c61.defaults` / `sdkconfig.defaults` → CPU 频率（C61=160MHz / S3=240MHz）等板卡级 kconfig 差异在这里，不在 Rust 代码里。

## 构建命令

### ESP32-C61（默认，无需额外参数）
```bash
cd firmware/esp32-app && RUSTC_BOOTSTRAP=1 cargo build --release
```

### ESP32-S3（需先装 Xtensa 工具链）
```bash
cargo install espup && espup install        # 一次
cd firmware/esp32-app && ./build-s3.sh       # 或手动：
#   MCU=ESP32S3 \
#   ESP_IDF_SDKCONFIG_DEFAULTS=.../sdkconfig.s3.defaults \
#   RUSTC_BOOTSTRAP=1 cargo build --target xtensa-esp32s3-espidf --features board-s3 --release
```

## 板卡专用 sdkconfig
- `sdkconfig.defaults` → C61（flash/PSRAM/WiFi 削减/分区表等）
- `sdkconfig.s3.defaults` → S3（8MB octal PSRAM、S3 分区、mbedTLS、lwIP）

> **注意**：S3 配置文件与 `build-s3.sh` 是基于 ESP32-S3 标准配置的起点；**需在真实 S3 板卡上烧录验证**（WiFi + BLE 共存、DeepSeek 端到端、PSRAM 8MB 识别）。C61 配置已在本仓库验证可用。


## ESP32-S3 构建已打通（2026-08-27 验证）

`./build-s3.sh` 现在可以端到端构建出 Xtensa 固件（ELF + 经 espflash 生成可烧录 .bin）。关键点：

- **工具链**：必须用 ESP-rs 的 `esp` 工具链（`cargo +esp`），它带完整 Xtensa LLVM 后端；stable 的 LLVM 无法处理 `-mtext-section-literals`。
- **bindgen 目标**：esp-clang 默认按 RISC-V 解析，需 `BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"` 让 bindgen 正确 targeting xtensa（修复 riscv/csr.h）。
- **l32r 字面量对齐**：`RUSTFLAGS="-C llvm-args=-mtext-section-literals"`，把字面量放进对齐的 .literal 节（修复 "l32r: misaligned literal target"）。
- **C 交叉编译**：S3 是 Xtensa，`CC`/`CXX` 必须指向 `toolchain-xtensa-esp-elf` 的 `xtensa-esp32s3-elf-gcc`/`g++`，否则 secp256k1_sys 等 C 依赖会编成 RISC-V 对象（EM:243）导致链接失败。
- **生成 .bin**：esptool `elf2image` 会因 .literal/.text 段落在同一 64KB flash 映射页而报错，改用 `espflash save-image --chip esp32s3 <elf> <bin>` 即可（内部合并段）。

```bash
cd firmware/esp32-app && ./build-s3.sh          # 产出 target/xtensa-esp32s3-espidf/release/magent-esp32-app
espflash save-image --chip esp32s3 \\
  target/xtensa-esp32s3-espidf/release/magent-esp32-app \\
  target/xtensa-esp32s3-espidf/release/magent-esp32-app.bin
```

烧录（真实板子）：`espflash flash --chip esp32s3 -b 460800 <bin>`（WiFi+BLE+agent 已在 S3 上构建成功，需真实硬件验证联机/PSRAM 8MB）。

## 2026-09-15：S3 导入 & 验收定稿

**烧录（唯一推荐路径）**：

```bash
cd firmware/esp32-app && ./flash-s3.sh --port /dev/cu.usbmodem101
```

- 分区表由 `partitions.s3.csv`（factory **2MB**）直接交给
  `espflash --partition-table`。**不要再烧 PIO 构建根目录下的
  `partitions.bin`**：该副本由早期构建遗留，可能仍是 1MB factory，会让
  bootloader 拒绝 1.5MB 的 app 镜像而进入复位循环
  （`Image length ... doesn't fit in partition length ...`）。
- `espflash` 会同时完成 Xtensa ELF → 镜像转换（`esptool elf2image` 处理
  不了本镜像的 .literal/.text 布局）。

**命令通道**：S3 开发板控制台在**原生 USB-Serial/JTAG**（GPIO19/20）。
固件在 ingress 里额外注册 `UsbSerialAdapter`（`link_adapters.rs`），
所以一根 USB 线即可收发：日志镜像 + `AT…`/自然语言命令，回复按来源链路
回送（`src=Rs232 { port: "USB-JTAG" }`）。不再需要外接 USB-TTL 到
GPIO10/11（UART0 链路仍然注册，作为第 0 号适配器保留）。

**一键验收**：

```bash
python3 tools/s3_full_test.py /dev/cu.usbmodem101 --turns 5
```

覆盖启动/堆/AT 面/ingress/agent/内存漂移，以及板子拿到 IP 后的
`web_admin` 与 `AT+HTTPGET` 回环；详见
[`../../docs/ESP32_S3_PORT_SUCCESS.md`](../../docs/ESP32_S3_PORT_SUCCESS.md)。

