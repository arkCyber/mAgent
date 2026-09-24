# mAgent 生产环境部署指南

**目标受众**: DevOps工程师、系统集成商、生产部署团队
**版本**: v0.1.0
**最后更新**: 2026-09-22

> **AT 命令与响应格式已于 2026-09-22 逐条对照固件源码核验。**
> 核验中修正了若干条照搬 ESP-AT 官方文档而来的**不存在**的命令与响应
> （`AT+CIPSTA`、`AT+LOGLEVEL`、`AT+MEMINFO`、`AT+SLEEP`），以及错误的
> `+GMR` / `+WIFIPASSUPGRADE` / `/api/status` 字段名和 NVS 键名。
> 本文档中出现的所有 `AT+` 命令现在都存在于
> `magent-core/src/at.rs` 的命令表中。若后续固件增删命令，请同步此处。

---

## 目录

1. [生产环境清单](#1-生产环境清单)
2. [平台选择](#2-平台选择)
3. [安全配置](#3-安全配置)
4. [固件构建](#4-固件构建)
5. [批量烧录](#5-批量烧录)
6. [网络配置](#6-网络配置)
7. [监控和日志](#7-监控和日志)
8. [OTA更新](#8-ota更新)
9. [故障恢复](#9-故障恢复)
10. [性能调优](#10-性能调优)

---

## 1. 生产环境清单

### 1.1 硬件要求

#### nRF52840 平台
- **MCU**: nRF52840 (Cortex-M4F @ 64MHz)
- **RAM**: 256 KB (最小使用 ~205KB)
- **Flash**: 1 MB (固件占用 ~290KB)
- **外设**: BLE 5.3, Temperature sensor
- **电源**: 1.7V - 3.6V
- **功耗**:
  - 活动模式: ~5mA
  - 空闲模式: ~1mA
  - 深度睡眠: <5μA

#### ESP32-C61 平台
- **MCU**: ESP32-C61 (RISC-V @ 160MHz)
- **RAM**: 320 KB SRAM + 2 MB PSRAM
- **Flash**: 8 MB
- **外设**: WiFi 6（**BLE 已从本固件移除**，`AT+BLE` 返回 error 9）
- **电源**: 3.0V - 3.6V
- **功耗**:
  - WiFi活动: ~120mA
  - WiFi待机: ~20mA
  - Light sleep: ~800μA
  - Deep sleep: ~5μA

#### ESP32-S3 平台（旗舰）
- **MCU**: ESP32-S3 (Xtensa LX7 dual-core @ 240MHz)
- **RAM**: 512 KB SRAM + 4 MB PSRAM (Quad)
- **Flash**: 4 MB (最小推荐 8MB用于OTA)
- **外设**: WiFi 2.4GHz, USB（**BLE 已从本固件移除**，`AT+BLE` 返回 error 9）
- **电源**: 3.0V - 3.6V
- **功耗**:
  - 双核活动: ~150mA
  - 单核活动: ~100mA
  - Light sleep: ~3mA
  - Deep sleep: ~10μA

### 1.2 软件要求

```bash
# 构建工具链
rustc 1.87+
cargo 1.87+

# ARM平台（nRF52840）
rustup target add thumbv7em-none-eabihf
cargo install probe-rs --features cli

# RISC-V平台（ESP32-C61）
# 使用标准Rust工具链

# Xtensa平台（ESP32-S3）
cargo install espup
espup install

# 烧录工具
cargo install espflash  # ESP32系列
```

---

## 2. 平台选择

### 2.1 平台对比矩阵

| 特性 | nRF52840 | ESP32-C61 | ESP32-S3 | 推荐场景 |
|------|----------|-----------|----------|----------|
| **连接** | BLE 5.3 | WiFi 6 | WiFi 2.4GHz | |
| **功耗** | 极低 | 中等 | 中高 | nRF: 可穿戴 |
| **算力** | 64MHz ARM | 160MHz RISC-V | 240MHz Xtensa x2 | S3: 云端LLM |
| **内存** | 256KB | 320KB+2MB | 512KB+4MB | S3: 复杂任务 |
| **成本** | $$$ | $$ | $$$ | C61: 性价比 |
| **开发难度** | 中 | 低 | 中 | C61: 快速原型 |
| **OTA支持** | 🔄 | ✅ | ✅ | ESP: 远程更新 |
| **Secure Boot** | ✅ | ✅ | ✅ | 全平台安全 |

### 2.2 推荐使用场景

**选择 nRF52840 当你需要**:
- 超低功耗（电池供电>1年）
- BLE生态系统集成
- 可穿戴设备、传感器节点
- 本地边缘AI推理

**选择 ESP32-C61 当你需要**:
- WiFi 6 高速连接
- 性价比优先
- 快速开发原型
- 中等复杂度任务

**选择 ESP32-S3 当你需要**:
- 云端LLM集成（DeepSeek/OpenAI）
- 双核并行处理
- 大量RAM（AI模型）
- HTTP管理面板
- 已验证的硬件稳定性

---

## 3. 安全配置

### 3.1 Secure Boot v2（ESP32系列）

#### 启用Secure Boot

```bash
cd firmware/esp32-app

# 1. 生成签名密钥（仅一次，妥善保管！）
espsecure.py generate_signing_key --version 2 \
  secure_boot_signing_key.pem

# 2. 使用生产配置构建
cp sdkconfig.prod.defaults sdkconfig.defaults
cargo build --release

# 3. 签名并烧录（带Secure Boot）
./flash-secure.sh --port /dev/ttyUSB0 \
  --signing-key secure_boot_signing_key.pem \
  --apply
```

**重要警告**:
- ⚠️ Secure Boot烧录是**不可逆的**，一旦启用无法关闭
- ⚠️ 签名密钥丢失将导致**无法更新固件**
- ⚠️ 建议先在测试板上验证完整流程

#### Secure Boot验证

```bash
# 读取eFuse状态
espefuse.py --port /dev/ttyUSB0 summary | grep SECURE_BOOT

# 预期输出：
# SECURE_BOOT_EN (BLOCK0)              Secure Boot enable bit            = 1 R/W (0x1)
```

### 3.2 Flash加密（ESP32系列）

```bash
# Flash加密与Secure Boot同时启用
./flash-secure.sh --port /dev/ttyUSB0 \
  --signing-key secure_boot_signing_key.pem \
  --flash-encryption \
  --apply

# 验证Flash加密
espefuse.py --port /dev/ttyUSB0 summary | grep FLASH_CRYPT
```

### 3.3 WiFi凭据保护

**DBO2 (Device-Bound Obfuscation v2)**

```bash
# WiFi密码自动加密存储在NVS
# 使用设备唯一密钥派生（HKDF + HMAC）

# AT命令配置（密码明文仅在内存中）
AT+CWJAP="MyNetwork","MySecurePassword"

# 密码在NVS中以加密形式存储
# 每个设备的密钥不同（基于eFuse MAC + Boot Key）
```

**从DBO1升级到DBO2**:
```bash
# 升级旧密码格式（必须显式传 1；其它值返回错误，避免误操作重新封装）
AT+WIFIPASSUPGRADE=1

# 验证升级
AT+WIFIPASSUPGRADE?
# 返回以下三者之一:
#   +WIFIPASSUPGRADE:CURRENT   ← 已是 DBO2（升级成功）
#   +WIFIPASSUPGRADE:LEGACY    ← 仍是旧格式（升级失败/未执行）
#   +WIFIPASSUPGRADE:NO_ENTRY  ← NVS 中没有 WiFi 密码
```

### 3.4 TLS证书验证

```bash
# 生产环境配置（sdkconfig.prod.defaults）
CONFIG_ESP_TLS_INSECURE=n
CONFIG_ESP_TLS_SKIP_SERVER_CERT_VERIFY=n
CONFIG_MBEDTLS_CERTIFICATE_BUNDLE=y
CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_CMN=y

# 自定义CA证书（可选）
# 将证书放入 main/certs/ca_cert.pem
# 在代码中加载：
esp_tls_cfg_t cfg = {
    .cacert_buf = ca_cert_pem_start,
    .cacert_bytes = ca_cert_pem_end - ca_cert_pem_start,
};
```

---

## 4. 固件构建

### 4.1 生产构建配置

**Cargo.toml profile**（已优化）:
```toml
[profile.release]
opt-level = "z"        # 优化大小
lto = true             # 链接时优化
codegen-units = 1      # 单编译单元
debug = false          # 移除调试符号
strip = true           # 移除符号表
panic = "abort"        # panic时直接abort
```

**ESP32 sdkconfig.prod.defaults**:
```ini
# 安全配置
CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y
CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK=y
CONFIG_SECURE_BOOT_V2_ENABLED=y
CONFIG_SECURE_FLASH_ENC_ENABLED=y

# 优化配置
CONFIG_COMPILER_OPTIMIZATION_SIZE=y
CONFIG_COMPILER_OPTIMIZATION_ASSERTIONS_DISABLE=y
CONFIG_LOG_DEFAULT_LEVEL_WARN=y

# 关闭调试功能
CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=n  # 生产环境关闭USB控制台
```

### 4.2 构建脚本

**nRF52840**:
```bash
#!/bin/bash
# build-nrf52-production.sh

set -e

echo "Building nRF52840 production firmware..."

# 清理
cargo clean -p magent-nrf52-app

# 构建
cargo build -p magent-nrf52-app \
  --release \
  --target thumbv7em-none-eabihf

# 生成HEX文件
cargo objcopy --release --target thumbv7em-none-eabihf -- \
  -O ihex \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app.hex

# 生成二进制文件
cargo objcopy --release --target thumbv7em-none-eabihf -- \
  -O binary \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app.bin

# 计算校验和
sha256sum target/thumbv7em-none-eabihf/release/magent-nrf52-app.bin

echo "Build complete!"
ls -lh target/thumbv7em-none-eabihf/release/magent-nrf52-app.*
```

**ESP32-S3**:
```bash
#!/bin/bash
# build-esp32s3-production.sh

set -e

cd firmware/esp32-app

echo "Building ESP32-S3 production firmware..."

# 使用生产配置
cp sdkconfig.prod.defaults sdkconfig.defaults

# 构建
./build-s3.sh

# 生成完整固件镜像（包含bootloader和分区表）
espflash save-image \
  --chip esp32s3 \
  --merge \
  target/xtensa-esp32s3-espidf/release/magent-esp32-app \
  magent-esp32s3-v0.1.0-production.bin

echo "Build complete!"
ls -lh magent-esp32s3-v0.1.0-production.bin
```

### 4.3 版本管理

```bash
# 版本号来自 Cargo 包版本；magent-core 只导出这一个常量：
# magent-core/src/lib.rs
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

# 通过AT命令查询
AT+GMR
# 返回（板型取决于构建时的 board-s3 / board-c61 feature）:
#   +GMR:mAgent v0.1.0 / AT v0.2 / esp32-s3
```

> 注意：固件**没有**嵌入 git hash 或构建日期（未使用 `vergen`）。
> 需要按构建追溯时，用 `git describe --always --dirty` 在构建机上记录，
> 不要指望 `AT+GMR` 能给出提交号。

---

## 5. 批量烧录

### 5.1 烧录站设置

**硬件需求**:
- USB Hub (供电充足，推荐有源Hub)
- 多个烧录器或开发板
- 测试夹具（Pogo Pin）

**软件配置**:
```bash
# 批量烧录脚本
#!/bin/bash
# batch-flash.sh

FIRMWARE="magent-esp32s3-v0.1.0-production.bin"
PORTS=$(ls /dev/ttyUSB* 2>/dev/null)

if [ -z "$PORTS" ]; then
  echo "No devices found"
  exit 1
fi

fail=0
for PORT in $PORTS; do
  echo "Flashing $PORT..."
  espflash write-bin 0x0 $FIRMWARE \
    --port $PORT \
    --chip esp32s3 \
    --baud 921600 &
done

# 必须收集每个后台任务的状态：只 `wait` 不看返回值，会在烧录失败时
# 照样打印 "All devices flashed successfully!"。
for job in $(jobs -p); do
  wait "$job" || fail=1
done

if [ "$fail" -ne 0 ]; then
  echo "至少有一个设备烧录失败" >&2
  exit 1
fi
echo "All devices flashed successfully!"
```

### 5.2 验证测试

```bash
#!/bin/bash
# verify-flash.sh
# 关键：每条命令都要**读回响应**并断言，否则"验证"只是写了一遍串口。
set -uo pipefail

PORT=$1
stty -f "$PORT" 115200 cs8 -cstopb -parenb -crtscts -echo
exec 3<>"$PORT"

send() { printf '%s\r\n' "$1" >&3; }

# 1. 检查固件版本
send 'AT+GMR'
timeout 5 cat <&3 | grep -q '^+GMR:mAgent v' \
  || { echo "FAIL: 版本查询无响应"; exit 1; }

# 2. 重启并等待重新就绪（AT+RST 不返回 OK，直接重启）
send 'AT+RST'
sleep 5
send 'AT+GMR'         # 重启后再次握手，确认真的起来了
timeout 5 cat <&3 | grep -q '^+GMR:' \
  || { echo "FAIL: 重启后无响应"; exit 1; }

# 3. 读取设备公钥（无设备身份时返回 +IDENT:NO_IDENTITY）
send 'AT+IDENT?'
timeout 5 cat <&3 | grep -qE '^\+IDENT:(NO_IDENTITY|INVALID|[0-9a-f]{64})' \
  || { echo "FAIL: 设备身份查询异常"; exit 1; }

exec 3<&-
echo "Verification complete for $PORT"
```

### 5.3 烧录记录

**数据库schema**:
```sql
CREATE TABLE flash_records (
  id INTEGER PRIMARY KEY,
  device_mac TEXT UNIQUE NOT NULL,
  firmware_version TEXT NOT NULL,
  flash_timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
  operator TEXT,
  secure_boot_enabled BOOLEAN,
  flash_encrypted BOOLEAN,
  test_passed BOOLEAN,
  notes TEXT
);
```

---

## 6. 网络配置

### 6.1 WiFi批量配置

**方法1: 通过AT命令**

注意：直接 `printf ... > $PORT` 只是"把命令写进去"，不看回复就无法判断成功
与否（串口写成功 ≠ 命令被接受）。要真正验证，必须**读回响应**：

```bash
#!/bin/bash
# 用法: ./configure-wifi.sh /dev/cu.usbserial-XXXX
set -euo pipefail

PORT=$1
SSID="ProductionNetwork"
PASSWORD="SecurePassword123!"

# 配置串口行规程（115200 8N1，关掉回显/流控）
stty -f "$PORT" 115200 cs8 -cstopb -parenb -crtscts -echo

# 双向打开一次，后面复用，避免每次重开串口丢状态
exec 3<>"$PORT"

# 发送配网命令（AT 命令以 \r\n 结尾）
printf 'AT+CWJAP="%s","%s"\r\n' "$SSID" "$PASSWORD" >&3

# 等待并读取响应；期望出现 OK（失败会是 ERROR / +CWJAP:... 失败码）
timeout 15 cat <&3 | grep -q '^OK' || { echo "配网失败"; exit 1; }

# 回读实际生效的配置；第 5 个字段是密码封装格式
printf 'AT+CWJAP?\r\n' >&3
timeout 5 cat <&3
# 期望输出: +CWJAP:"ProductionNetwork",,0,0,DBO2
#            OK
# 若第 5 个字段是 PLAINTEXT_LEGACY，说明还没升级到 DBO2，
# 请执行 AT+WIFIPASSUPGRADE=1。

exec 3<&-   # 关闭
```

**方法2: 通过NVS预置**

> 命名空间和键名必须与固件一致，否则设备永远连不上：
> 命名空间是 **`magent`**（不是 `wifi`），键名是 **`wifi_ssid`** / **`wifi_pass`**
> （不是 `ssid` / `password`）。

```bash
# 创建NVS分区镜像（0x6000 = partitions.csv 里 nvs 分区的大小）
python $IDF_PATH/components/nvs_flash/nvs_partition_generator/nvs_partition_gen.py \
  generate wifi_config.csv wifi_nvs.bin 0x6000

# wifi_config.csv内容:
# key,type,encoding,value
# magent,namespace,,
# wifi_ssid,data,string,ProductionNetwork
# wifi_pass,data,string,SecurePassword123!

# 烧录NVS分区（0x9000 = partitions.csv 里 nvs 分区的偏移）
esptool.py write_flash 0x9000 wifi_nvs.bin
```

> ⚠️ 这么预置的密码是**明文存储**（开机按 `LegacyPlaintext` 读取，能连上）。
> 上电后请立刻执行 `AT+WIFIPASSUPGRADE=1` 把它重新封装成设备绑定的 DBO2，
> 用 `AT+WIFIPASSUPGRADE?` 确认返回 `CURRENT`。

### 6.2 静态IP配置

> **当前固件不支持静态IP。** 没有 `AT+CIPSTA` 命令；网络地址一律由 DHCP 分配。
> 不要把 ESP-AT 官方文档里的 `AT+CIPSTA`/`+CIPSTA:` 示例照搬到这里。

可用的只有下面两条**只读**查询：

```bash
# 查询当前 IP（无参 Query 形式）
AT+IFCONFIG?
# 返回: +IFCONFIG:"192.168.1.100"      ← 未连接时是空串 +IFCONFIG:""

# 查询 STA MAC（无引号，纯十六进制）
AT+CIPSTAMAC?
# 返回: +CIPSTAMAC:aa:bb:cc:dd:ee:ff
```

需要在现场固定地址时，请在 DHCP 服务器上做 MAC 绑定。

---

## 7. 监控和日志

### 7.1 HTTP状态端点

```bash
# 设备状态查询
curl http://192.168.1.100/api/status

# 响应示例（实际字段名；注意 uptime 是**毫秒**不是秒）:
{
  "version": "0.1.0",
  "wifi_state": 2,
  "ip": "192.168.1.100",
  "ssid": "ProductionNetwork",
  "rssi_dbm": -45,
  "wifi_reason": 0,
  "wifi_reason_label": "ok",
  "free_heap_b": 2184320,
  "heap_low_water_b": 2100000,
  "uptime_ms": 86400000,
  "latency": {"llm_rt": {…}, "at_dispatch": {…}, "e2e_reply": {…},
              "e2e_agent": {…}, "agent_task": {…}}
}
```

对应的 HTML 面板（同一端点，浏览器直接打开）还会额外显示 heap 低水位与延迟
分位数。

### 7.2 日志级别配置

> 没有 `AT+LOGLEVEL`。日志级别用 **`AT+SYSLOG`**，参数是数字 0–5：

```bash
# 设置日志级别
AT+SYSLOG=2   # 0=Off 1=Error 2=Warn 3=Info 4=Debug 5=Trace

# 查询当前级别（返回的也是数字）
AT+SYSLOG?
# 返回: +SYSLOG:2
```

### 7.3 远程监控集成

**Prometheus导出器**（计划功能）:
```
http://192.168.1.100/metrics

# HELP magent_uptime_seconds Total uptime in seconds
# TYPE magent_uptime_seconds counter
magent_uptime_seconds 86400

# HELP magent_free_heap_bytes Free heap memory in bytes
# TYPE magent_free_heap_bytes gauge
magent_free_heap_bytes 2184320
```

---

## 8. OTA更新

### 8.1 OTA服务器设置

```bash
# 简单HTTP服务器
python3 -m http.server 8080

# 将固件放在可访问位置
cp magent-esp32s3-v0.1.1.bin /var/www/firmware/
```

### 8.2 触发OTA更新

```bash
# AT命令触发
AT+OTA=http://192.168.1.200:8080/magent-esp32s3-v0.1.1.bin

# 响应（只有这一条：任务已启动，下载在后台线程进行）:
# +OTA:STARTED
#
# 下载/校验/重启过程只写到串口日志（[ota] OTA completed; device rebooting
# 或 [ota] OTA failed: …），不会再有 +OTA: 前缀的后续行。
```

### 8.3 OTA回滚保护

```bash
# 固件首次启动后必须确认有效
# 在首次启动60秒内，固件自动调用：
esp_ota_mark_app_valid_cancel_rollback();

# 如果在此之前设备重启（崩溃），bootloader自动回滚到旧版本
```

> ⚠️ **不要用 `AT+RESTORE` 测试回滚。** `AT+RESTORE` 是**出厂重置**
> —— 它会擦除整个 NVS（含设备身份、WiFi 凭据、全部配置）并重启，
> 不是"恢复旧固件"。要测试回滚，请用新镜像的正常 OTA 流程：
> 推送一个会在启动后 60 秒内崩溃的镜像，观察 bootloader 回退。

---

## 9. 故障恢复

### 9.1 安全模式

**触发条件**:
- 连续3次快速重启（<60秒内）
- 自动进入安全模式

**安全模式行为**:
- ✅ 跳过WiFi连接（避免在坏配置上反复重试）
- ✅ UART命令接口仍可用
- ❌ 不加载用户配置
- ⚠️ BLE 已从本固件移除（见 CHANGELOG），安全模式下同样不可用

**`AT+SAFEMODE` 的真实语义**（容易误解，务必读）:

`AT+SAFEMODE=1` 写的是一个**一次性标志**，效果发生在**下一次启动**，且启动时
会被自动清除（不需要手动关掉）：

```bash
# 让设备在下次重启时跳过 WiFi（例如当前 WiFi 配置把设备搞进了重启循环）
AT+SAFEMODE=1
# 该标志在下次启动被读取并立即清零 → 只影响一次启动

# 查询标志当前值（尚未被消费时）
AT+SAFEMODE?
# 返回: +SAFEMODE:1   或   +SAFEMODE:0

# 取消一个尚未被消费的强制标志
AT+SAFEMODE=0
```

**注意**：`AT+SAFEMODE` 既不能"退出"由崩溃循环触发的安全模式，也不会持久化。
崩溃循环触发的安全模式会在连续快速重启计数器复位后自然消失 —— 复位办法是让
设备连续稳定运行，或用 `AT+RESTORE` 清空 NVS（但那会一并抹掉设备身份）。

### 9.2 出厂重置

```bash
# ⚠️ 破坏性且不可逆：擦除整个 NVS 分区（WiFi凭据、设备身份、AT配置、
#    所有命名空间），然后重启。设备会在下次启动时重新生成新的设备身份。
#    没有返回值 —— 命令执行后设备直接重启。
AT+RESTORE
```

> 如果只想恢复 WiFi 配置、保留设备身份，请改用 `AT+CWJAP="SSID","PASS"`
> 覆盖凭据，不要用 `AT+RESTORE`。

### 9.3 串口恢复

```bash
# 如果WiFi不可用（ESP32 固件已无 BLE 通道），通过串口恢复

# 1. 连接串口
screen /dev/ttyUSB0 115200

# 2. 重新配置WiFi
AT+CWJAP="NewSSID","NewPassword"

# 3. 验证
AT+CWJAP?
```

---

## 10. 性能调优

### 10.1 内存优化

```bash
# 查看当前空闲堆（字节）
AT+HEAP
# 返回: +HEAP:2184320

# 查看开机以来的运行时长（注意单位是毫秒）
AT+UPTIME
# 返回: +UPTIME:86400000

# 等价的别名命令
AT+SYSRAM      # 同 AT+HEAP，返回 +SYSRAM:<bytes>
```

堆的**低水位**（开机以来最小空闲堆，判断内存泄漏的关键指标）不在 AT 侧，
要看 HTTP 状态：`curl http://<ip>/api/status | jq .heap_low_water_b`，
或打开浏览器面板的 "heap low-water" 一行。

如果低水位持续下降，考虑：
1. 禁用非必要功能（Lua 应用）
2. 减少 agent 历史记录大小
3. 降低并发任务数

### 10.2 CPU优化

```bash
# ESP32-S3: 启用双核调度
CONFIG_FREERTOS_UNICORE=n

# 关键任务绑定到Core 0
# agent线程 -> Core 0
# WiFi任务 -> Core 0
# 后台任务 -> Core 1
```

### 10.3 功耗优化

> **当前固件没有 `AT+SLEEP` 命令，也没有任何运行时电源管理 AT 接口。**
> ESP32-S3 上的 Light Sleep / Modem Sleep 需要相应的 sdkconfig 选项与
> 唤醒源配置，属于构建期工作，不是 AT 可切换的运行时开关。

现场降低功耗的现实手段：
1. 构建时关闭不用的功能（Lua 应用、Web 管理面板）以缩短活跃时间
2. 降低 agent 轮询/心跳频率（见 `main.rs` 的心跳间隔常量）
3. 需要真正的深度睡眠时，按需求单独开发电源管理并配合 GPIO/定时器唤醒

---

## 附录A: 生产检查清单

**硬件验证** ✓
- [ ] 供电电压稳定（±5%）
- [ ] 外部晶振工作正常
- [ ] 天线连接良好（WiFi；BLE 仅 nRF52840 平台有）
- [ ] 温度传感器校准（nRF52840）

**固件验证** ✓
- [ ] 版本号正确 —— `AT+GMR` 返回 `+GMR:mAgent v<x> / AT v0.2 / <chip>`，
      且 `<chip>` 与目标板一致
- [ ] Secure Boot已启用 —— `espefuse.py summary | grep SECURE_BOOT`
- [ ] Flash加密已启用 —— `espefuse.py summary | grep FLASH_CRYPT`
- [ ] OTA分区正确配置 —— 分区表含 `ota_0`/`ota_1`/`otadata` 三项

**功能测试** ✓
- [ ] WiFi连接成功 —— `AT+CWJAP?` 返回 `+CWJAP:"<ssid>",,0,0,<fmt>` 后跟 `OK`
- [ ] UART通信正常 —— 任意 `AT+GMR` 有回显
- [ ] HTTP接口可访问 —— `curl http://<ip>/api/status` 返回 JSON，
      且 `jq .free_heap_b` 非 0
- [ ] 传感器读取正确 —— 让 agent 执行 `read the temperature`，
      比较返回值与环境
- [ ] （仅 nRF52840）BLE广播可见

**安全测试** ✓
- [ ] WiFi密码已加密存储 —— `AT+WIFIPASSUPGRADE?` 返回 `+WIFIPASSUPGRADE:CURRENT`
      （`LEGACY` / `PLAINTEXT_LEGACY` 都表示仍是明文）
- [ ] TLS证书验证通过 —— 完整跑一次 TLS 请求（见 `docs/TIER6_VALIDATION.md`）
- [ ] AT命令访问控制（如需）
- [ ] 设备身份唯一 —— `AT+IDENT?` 返回 64 位十六进制公钥，逐台比对不重复

**压力测试** ✓
- [ ] 24小时稳定性测试 —— 结束后查 `heap_low_water_b` 未持续下降
- [ ] 温度循环测试（-20°C ~ 70°C）
- [ ] WiFi断线重连测试 —— 手动关闭 AP，观察 `[wifi-sup] LINK DOWN` →
      `[wifi-mgr] ✅ 重连成功！` → `[wifi-sup] LINK UP`
- [ ] OTA更新测试 —— `AT+OTA=<url>` 返回 `+OTA:STARTED`，且串口最终出现
      `[ota] OTA completed; device rebooting`

---

## 附录B: 联系支持

**技术支持**:
- Email: support@arkCyber.com
- GitHub Issues: https://github.com/arkCyber/mAgent/issues
- 文档: https://github.com/arkCyber/mAgent/tree/main/docs

**紧急联系**（24/7）:
- Phone: +86-xxx-xxxx-xxxx
- WeChat: arkCyber-Support

---

**部署指南版本**: 1.0
**最后更新**: 2026-09-17
**下次审查**: 2026-12-17
