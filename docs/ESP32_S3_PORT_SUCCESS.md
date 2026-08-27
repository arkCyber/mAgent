# ESP32-S3 移植成功记录（最终成功）

> **日期**: 2026-08-27
> **板子**: ESP32-S3-WROOM-1-N8R8（实际 4MB flash 变体）
> **状态**: ✅ Xtensa 固件在真实硬件上启动成功，agent 线程正常运行

## 一、最终结论
1. ESP32-S3 移植代码（board-s3 + web_admin.rs + fetch_web）**已在真实 S3 板上烧录并启动**，agent 线程持续运行（实测温度传感器工具返回 temperature=32.6 C）。
2. 全程解决 7 类阻塞，均已修复并固化到 build-s3.sh / build.rs。

## 二、关键修复
1. **工具链**：cargo +esp（ESP-rs 工具链）；esp 默认对齐字面量，无需 -mtext-section-literals。
2. **bindgen**：BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"（esp-clang 默认 RISC-V）。
3. **C 交叉编译**：CC/CXX 指向 Xtensa 的 xtensa-esp32s3-elf-gcc/g++（否则 secp256k1_sys 等编成 RISC-V）。
4. **双 DROM 段修复**（build.rs）：把 .defmt.* 并入 .flash.rodata，消除 appdesc/rodata 间隙，bootloader 接受镜像（之前报 Invalid app image header 复位循环）。
5. **sdkconfig 修复**：PlatformIO 的 idf.py 只读标准名 sdkconfig.defaults；已把 sdkconfig.defaults 改为 S3 配置（4MB + partitions.s3.csv），C61 用 sdkconfig.c61.defaults（build-c61.sh 临时交换）。
6. **网络代理**：SOCKS5 127.0.0.1:10808 + PySocks（PlatformIO 下载）。

## 三、构建 / 烧录
```bash
cd firmware/esp32-app && ./build-s3.sh              # 构建 S3（sdkconfig.defaults=S3，4MB）
./flash-s3.sh --port /dev/cu.usbserial-XXX          # 烧录 bootloader+分区+app
# C61（避免 sdkconfig 冲突）：./build-c61.sh（临时交换 sdkconfig.defaults 为 C61 后构建）
```

## 四、待后续验证（非阻塞）
WiFi 联机 + BLE + DeepSeek 端到端、PSRAM 识别、web_admin 网页（http://<ip>/）、fetch_web 出站抓取。
