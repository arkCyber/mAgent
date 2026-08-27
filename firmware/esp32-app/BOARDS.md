# mAgent 板卡配置（编译开关）

同一份固件源码通过 **Cargo feature 编译开关** 同时支持两块芯片：

| 板卡 | Cargo feature | 目标 target | CPU | 内部 SRAM | PSRAM |
|---|---|---|---|---|---|
| **ESP32-C61** (默认) | `board-c61` | `riscv32imac-esp-espidf` | RISC-V 160MHz | ~134KB 动态 | 2MB |
| **ESP32-S3** | `board-s3` | `xtensa-esp32s3-espidf` | Xtensa 240MHz | ~390KB 动态 | 8MB |

## 为什么需要 S3
C61 内部 SRAM 动态区仅 ~134KB，WiFi + BLE + agent 三者无法共存（内存不足 / PSRAM 栈与 WiFi 的 CPU_LOCKUP）。**S3 有 ~390KB 内部动态堆 + 8MB PSRAM，可以同时跑 WiFi(DeepSeek) + BLE + agent**，端到端解决智能体的云端对话问题。

## 代码中的编译开关
- `firmware/esp32-app/Cargo.toml` → `[features]`：`board-c61`（默认）/ `board-s3`，同时联动 `esp-println` 的芯片 feature。
- `src/ble_config.rs` → `bt_controller_config()` 用 `#[cfg(feature = "board-*")]` 切换 CPU 频率（C61=160 / S3=240）；`build_device_info()` 切换芯片型号字符串。
- `src/main.rs` → 启动日志与 agent 名称按板卡切换。

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
