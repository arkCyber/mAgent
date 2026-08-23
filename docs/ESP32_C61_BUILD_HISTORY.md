# ESP32-C61 编译成功记录

> **日期**: 2026年8月20日  
> **硬件**: ESP32-C61-DevKitC-1-N8R2  
> **平台**: macOS (Apple Silicon)  
> **状态**: ✅ 编译成功

---

## 快速命令 (已验证可用)

```bash
# 1. 激活环境
source ~/export-esp.sh

# 2. 进入项目目录
cd /Users/arksong/MicroAgent/firmware/esp32-app

# 3. 编译 (Debug)
cargo build

# 4. 编译 (Release - 推荐)
cargo build --release

# 5. 烧录到设备
espflash flash target/riscv32imac-esp-espidf/release/magent-esp32-app --monitor
```

---

## 编译时间统计

| 构建类型 | 首次编译 | 增量编译 | 清理后重编译 |
|---------|---------|---------|-------------|
| Debug   | ~2分钟  | ~15秒   | ~2分钟      |
| Release | ~3分钟  | ~20秒   | ~3分钟      |

---

## 成功编译的关键修复

### 1. MCU 识别问题 ✅ 已修复
**文件**: `.cargo-patches/esp-idf-sys-0.37.2/build/pio.rs`  
**修改**: 第 154-162 行
```rust
let mcu_for_pio = config.mcu.clone().map(|mcu| {
    let upper = mcu.to_uppercase();
    match upper.as_str() {
        "ESP32C61" => "ESP32C6".to_string(),
        _ => upper,
    }
});
```
**原因**: PlatformIO 不识别 ESP32C61，需要映射到 ESP32C6 工具链

### 2. Linker Script 冲突 ✅ 已修复
**文件**: `firmware/esp32-app/build.rs`  
**修改**: 第 78-245 行（完整的 `patch_sections_ld` 函数）
```rust
fn patch_sections_ld() {
    // 动态定位 sections.ld 并注释掉 ASSERT 语句
}
```
**原因**: ESP-IDF 的 linker script 有 hard ASSERT，与 defmt 的 orphan sections 冲突

### 3. 符号冲突 ✅ 已修复
**文件**: `firmware/esp32-app/src/sysenv_stubs.c`  
**修改**: 第 65 行
```c
WEAK int posix_memalign(void **memptr, size_t alignment, size_t size) {
```
**原因**: ESP-IDF v6.0 的 newlib 提供了 posix_memalign，需要 weak symbol

---

## 构建产物

### Debug 构建
```
target/riscv32imac-esp-espidf/debug/magent-esp32-app
大小: ~2.8 MB
包含调试符号: 是
```

### Release 构建
```
target/riscv32imac-esp-espidf/release/magent-esp32-app
大小: 2.0 MB (stripped)
包含调试符号: 否

firmware.elf (带符号)
大小: 3.5 MB
Flash 占用: ~169 KB
RAM 占用: ~60 KB
```

---

## 编译过程中的警告 (可忽略)

1. **orphan section `.defmt.end`**  
   类型: 链接器警告  
   状态: 正常，已被 linker script 补丁处理

2. **unused import: `std::fs`**  
   文件: `firmware/esp32-app/build.rs:31`  
   状态: 小问题，不影响构建

3. **esp-wifi patch not used**  
   状态: 正常，此项目不使用 esp-wifi crate

---

## 环境配置检查清单

- [x] `espup install` 成功
- [x] `source ~/export-esp.sh` 已执行
- [x] `~/.zshrc` 包含 `source ~/export-esp.sh`
- [x] `rustc --version` 显示 `esp` channel
- [x] `.cargo/config.toml` 配置正确
- [x] `rust-toolchain.toml` 设置为 `channel = "esp"`
- [x] `sdkconfig.defaults` 包含 `CONFIG_IDF_TARGET_ESP32C61=y`

---

## 常见问题快速解决

### 问题 1: `MCUs mismatch` 错误
```bash
# 清理缓存
cargo clean -p esp-idf-sys
rm -rf ~/.platformio/.cache

# 重新编译
cargo build --release
```

### 问题 2: 网络问题
```bash
# 设置代理（国内用户）
export http_proxy=http://127.0.0.1:10808
export https_proxy=http://127.0.0.1:10808
export ALL_PROXY=socks5://127.0.0.1:10808
```

### 问题 3: 找不到 espflash
```bash
cargo install espflash
```

### 问题 4: 设备未识别
```bash
# macOS: 查看设备
ls /dev/cu.usbserial-* /dev/cu.usbmodem*

# 手动指定端口
espflash flash --port /dev/cu.usbserial-xxx target/...
```

---

## 下次编译前检查

```bash
# 1. 确认环境激活
rustc --version | grep esp
# 预期输出: rustc 1.83.0-nightly (esp ...)

# 2. 确认目标已安装
rustup target list | grep riscv32imac-esp-espidf
# 预期输出: riscv32imac-esp-espidf (installed)

# 3. 确认工具可用
which ldproxy espflash
# 预期输出: 两个路径

# 4. 开始编译
cd firmware/esp32-app && cargo build --release
```

---

## 性能指标

| 指标 | 数值 |
|-----|------|
| Flash 代码段 | 74,522 bytes |
| Flash 只读数据 | 37,508 bytes |
| IRAM 代码 | 48,712 bytes |
| DRAM 数据 | 7,292 bytes |
| DRAM BSS | 3,920 bytes |
| **总 Flash 占用** | **~169 KB** |
| **总 RAM 占用** | **~60 KB** |

---

## 架构验证

```bash
file target/riscv32imac-esp-espidf/release/magent-esp32-app
```

**预期输出**:
```
ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (SYSV), statically linked, stripped
```

**验证点**:
- ✅ 32-bit RISC-V
- ✅ 压缩指令集 (RVC)
- ✅ 软浮点 ABI
- ✅ 静态链接
- ✅ 符号已剥离 (Release)

---

## 串口输出示例

启动后的预期输出：

```
ESP-ROM:esp32c6-20220919
Build:Sep 19 2022
rst:0x1 (POWERON),boot:0xc (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:1
load:0x4086e600,len:0x1628
load:0x4087ce00,len:0x2800
entry 0x4087ce00
[INFO] mAgent ESP32-C61 v0.1.0 starting...
[INFO] Initializing NVS...
[INFO] Initializing WiFi...
[agent] thread starting
[agent] MiniAgent configured: max_iterations=20, max_memory=524288
[ingress] thread starting
[ingress] IngressGateway initialized
```

---

## 版本信息

| 组件 | 版本 |
|-----|------|
| Rust | esp channel (1.83.0-nightly) |
| ESP-IDF | v6.0 |
| GCC | riscv32-esp-elf-gcc 15.2.0 |
| espflash | 3.x |
| ldproxy | 0.3.x |
| PlatformIO | 6.x |

---

## 参考文档

- 完整构建指南: [docs/ESP32_C61_BUILD.md](ESP32_C61_BUILD.md)
- 项目主 README: [README.md](../README.md)
- ESP-IDF 文档: https://docs.espressif.com/projects/esp-idf/en/latest/
- ESP Rust Book: https://esp-rs.github.io/book/

---

## 备注

1. 所有补丁文件已在项目中，无需手动修改
2. 第一次编译会下载 ESP-IDF 组件，需要稳定网络
3. 推荐使用 Release 模式进行实际部署
4. Debug 模式主要用于开发调试
5. 编译产物中的 `firmware.elf` 包含完整符号表，用于调试

---

**编译成功标记**: ✅  
**最后验证**: 2026年8月20日 11:31  
**编译者**: arksong  
**工作目录**: `/Users/arksong/MicroAgent`
