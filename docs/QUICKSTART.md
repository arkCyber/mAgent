# mAgent 快速开始指南

**目标**: 10分钟内运行你的第一个AI智能体

---

## 前置条件

### 最低要求
- **操作系统**: Linux, macOS, 或 Windows (WSL2)
- **Rust**: 1.87+ (`rustup update` 确保最新版本)
- **内存**: 4GB RAM
- **磁盘**: 2GB 可用空间

### 可选（用于硬件部署）
- **nRF52840 开发板** - 用于BLE/低功耗场景
- **ESP32-C61 或 ESP32-S3 开发板** - 用于WiFi/云端LLM场景
- **USB 串口线** - 用于固件烧录

---

## 方案1: 主机模拟器（最快）

**适用场景**: 快速体验AI智能体功能，无需硬件

### 步骤1: 克隆仓库

```bash
git clone https://github.com/arkCyber/mAgent.git
cd mAgent
```

### 步骤2: 安装Ollama（本地LLM）

```bash
# macOS
brew install ollama

# Linux
curl -fsSL https://ollama.com/install.sh | sh

# 启动服务
ollama serve

# 拉取模型（新终端）
ollama pull llama3.2:1b
```

### 步骤3: 运行CLI工具

```bash
# 构建CLI
cargo build -p magent --release

# 运行你的第一个智能体任务
./target/release/magent run "读取温度传感器"

# 使用DeepSeek云端LLM（如果你有API密钥）
DEEPSEEK_API_KEY=sk-xxx ./target/release/magent run --provider deepseek "查询上海天气"
```

**预期输出**（`--mock` 下逐字如此；接真实后端时只有 `[Backend]` / `[LLM]` 两行不同）:
```
[Config] applied provider=ollama, ollama_url=http://localhost:11434, …
[Backend] SimulatorExecutor (sensors/BLE/flash/GPIO); budget = 10 iter / 8 tools / …
[Mode] mock (simulated reasoning)
[Task] read the temperature sensor
[Thinking] iteration 1 (tool_calls=0)
[LLM] {"tool": "read_sensor", "args": {"sensor": "temperature"}}
[Action] read_sensor {"sensor":"temperature"}
[Tool] read_sensor: Temperature: 24.2°C
[Thinking] iteration 2 (tool_calls=1)
[LLM] {"result": "Temperature sensor reading completed"}
[Result] Temperature sensor reading completed
```

---

## 方案2: ESP32-S3 硬件部署（推荐）

**适用场景**: 真实硬件项目，需要WiFi/云端LLM

### 步骤1: 安装ESP工具链

```bash
# 安装espup和esp-rs工具链
cargo install espup
espup install

# 安装espflash
cargo install espflash
```

### 步骤2: 构建固件

```bash
cd firmware/esp32-app

# 构建ESP32-S3固件
./build-s3.sh
```

### 步骤3: 烧录固件

```bash
# 连接ESP32-S3开发板到USB端口
# 查找端口（macOS: /dev/cu.usbserial-*, Linux: /dev/ttyUSB*）

# 烧录
./flash-s3.sh --port /dev/cu.usbserial-XXXXX

# 监控串口
espflash monitor /dev/cu.usbserial-XXXXX
```

### 步骤4: 配置WiFi（通过AT命令）

```bash
# 在串口终端中输入（或通过另一终端发送）：

# 设置WiFi凭据
AT+CWJAP="你的WiFi名称","你的WiFi密码"

# 验证连接
AT+CWJAP?

# 查看设备信息
AT+GMR
```

### 步骤5: 通过UART发送智能体任务

```bash
# 方法1: 直接写入串口
printf 'read the temperature\n' > /dev/cu.usbserial-XXXXX

# 方法2: 使用测试脚本
python tools/s3_full_test.py --port /dev/cu.usbserial-XXXXX
```

**预期输出**（标签与固件实际打印的一致，见
`main.rs` / `local_tools.rs`；IP 与温度因环境而异）:
```
[wifi-sup] initial state: connected=true ip=192.168.1.100 rssi=-55 dBm
[agent] thread starting
[agent] MiniAgent ready
[agent] result(read the temperature): temperature=34.4 C (err=0)
RESULT[read the temperature]: temperature=34.4 C (err=0)
```

### 步骤6: 访问HTTP管理面板

```bash
# 浏览器打开（替换为你的设备IP）
open http://192.168.1.100

# 或使用curl查看状态
curl http://192.168.1.100/api/status | jq
```

---

## 方案3: nRF52840（BLE场景）

**适用场景**: 智能手表/可穿戴设备，需要BLE通信

### 步骤1: 安装ARM工具链

```bash
# 安装ARM目标
rustup target add thumbv7em-none-eabihf

# 安装probe-rs（用于烧录）
cargo install probe-rs --features cli
```

### 步骤2: 构建固件

```bash
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
```

### 步骤3: 烧录到nRF52840 DK

```bash
# 连接nRF52840 DK到USB
probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

### 步骤4: BLE配置（使用桌面管理器）

```bash
# 构建Tauri桌面应用
cd host/magent-man
npm install
npm run tauri build

# 运行
./src-tauri/target/release/magent-man
```

---

## 常见任务示例

### 传感器读取

```bash
# 温度
magent run "read temperature sensor"

# 多传感器监控
magent run "monitor temperature, humidity, and pressure for 10 seconds"
```

### GPIO控制

```bash
# 开灯
magent run "turn on the LED"

# 设置GPIO引脚
magent run "set GPIO pin 13 to high"
```

### BLE通信

```bash
# 发送BLE通知
magent run "send BLE notification: Hello from mAgent"
```

### Web3功能

> Web3 是**默认关闭的 Cargo feature**，不是运行时开关。必须先带 feature 构建，
> 否则 `magent web3 …` 只会回 `error: unknown flag: web3`。

```bash
# 带 web3 支持构建（一次性）
cargo build -p magent --features web3,blockchain

# vault 是加密的：写盘时必须提供口令，否则 new/sign 会报
#   vault decrypt failed: passphrase env var $MAGENT_WEB3_PASSPHRASE is not set
# 默认变量名是 MAGENT_WEB3_PASSPHRASE，可用 --passphrase-env 换名。
export MAGENT_WEB3_PASSPHRASE='choose-a-strong-passphrase'

# 生成身份（<NAME> 是身份名，不是 --output 路径）
./target/debug/magent web3 new alice

# 列出 / 查看身份（只读，不需要口令）
./target/debug/magent web3 list
./target/debug/magent web3 identity alice

# 签名（payload 从文件读，原文不做位置参数；需要口令）
echo -n "Hello, Web3!" > msg.bin
./target/debug/magent web3 sign alice --payload msg.bin

# 区块链工具需要 blockchain feature（同样是构建期开关）
./target/debug/magent run "get my ETH balance"
```

**关于 `magent web3 did`**：不访问 vault，因此无需口令，例如
`./target/debug/magent web3 did --from-seed <64位hex>`。

### 云端LLM对话

```bash
# DeepSeek推理
DEEPSEEK_API_KEY=sk-xxx magent run --provider deepseek \
  "分析这组传感器数据并给出建议：temp=35°C, humidity=80%"

# 使用网络搜索工具
magent run --provider deepseek "搜索Rust 2026新特性，并总结最重要的3个"
```

---

## 故障排除

### 问题1: Ollama连接失败

**症状**: `Error: Failed to connect to Ollama`

**解决方案**:
```bash
# 检查Ollama服务是否运行
curl http://localhost:11434/api/tags

# 如果没有响应，启动服务
ollama serve

# 如果端口冲突，指定自定义端口
magent run --ollama http://localhost:11435 "你的任务"
```

### 问题2: 固件烧录失败

**症状**: `Error: Failed to flash firmware`

**解决方案**:
```bash
# ESP32: 检查USB驱动
# macOS需要CP2102驱动: https://www.silabs.com/developers/usb-to-uart-bridge-vcp-drivers

# 检查端口权限（Linux）
sudo usermod -a -G dialout $USER
# 重新登录生效

# 进入下载模式
# ESP32: 按住BOOT按钮，按RST按钮，释放RST，释放BOOT
# nRF52840: 无需特殊步骤，probe-rs自动处理
```

### 问题3: WiFi连接失败（ESP32）

**症状**: `WiFi connection timeout`

**解决方案**:
```bash
# 检查WiFi凭据
AT+CWJAP?

# 重新设置（注意引号）
AT+CWJAP="SSID","password"

# 检查WiFi频段（仅支持2.4GHz）
# 如果路由器是5GHz，请切换到2.4GHz

# 进入安全模式（跳过WiFi）
# 连续快速重启3次会触发安全模式
```

### 问题4: 编译错误

**症状**: `error: invalid Mach-O section specifier`

**解决方案**:
```bash
# 不要在macOS上直接运行cargo test --workspace
# 使用CI推荐的命令：
cargo test --workspace \
  --exclude magent-nrf52-app \
  --exclude magent-esp32-app \
  --exclude magent-integration-test

# 或者只测试特定包
cargo test -p magent-core
cargo test -p magent --lib
```

---

## 下一步

### 学习更多
- **平台选择**: [docs/PLATFORM_COMPARISON.md](PLATFORM_COMPARISON.md)
- **AT命令参考**: [docs/AT_COMMAND_REFERENCE.md](AT_COMMAND_REFERENCE.md)
- **LLM后端配置**: [docs/LLM_BACKENDS.md](LLM_BACKENDS.md)
- **Web3功能**: [README.md#web3-identity](../README.md)

### 示例项目
- `examples/wearable-pipeline/` - 可穿戴设备数据管道
- `examples/mqtt-roundtrip/` - MQTT消息路由
- `examples/email-mqtt-bridge/` - 邮件与MQTT桥接

### 社区支持
- **GitHub Issues**: https://github.com/arkCyber/mAgent/issues
- **讨论区**: https://github.com/arkCyber/mAgent/discussions
- **贡献指南**: [CONTRIBUTING.md](../CONTRIBUTING.md)

---

## 性能提示

### 优化编译时间
```bash
# 使用sccache缓存编译结果
cargo install sccache
export RUSTC_WRAPPER=sccache

# 使用mold链接器（Linux）
sudo apt install mold
export RUSTFLAGS="-C link-arg=-fuse-ld=mold"
```

### 减少固件大小
```bash
# 使用release profile（已优化为size）
cargo build --release

# 查看固件大小
cargo size --release -- -A

# 进一步优化（在Cargo.toml中）
[profile.release]
opt-level = "z"     # 优化大小
lto = true          # 链接时优化
codegen-units = 1   # 单编译单元
strip = true        # 移除符号
```

### 电池优化
```bash
# 启用低功耗模式
magent run "enter low power mode"

# 配置电源管理（在config.json中）
{
  "power": {
    "idle_timeout_ms": 5000,
    "deep_sleep_enabled": true
  }
}
```

---

**快速开始完成！** 🎉

你现在应该能够运行mAgent智能体了。如果遇到问题，请查看[故障排除](#故障排除)或提交[GitHub Issue](https://github.com/arkCyber/mAgent/issues)。
