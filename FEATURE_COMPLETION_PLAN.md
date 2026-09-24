# mAgent 功能补全计划 - 无BLE优化版

**日期**: 2026-09-17
**策略**: 不使用BLE，专注核心功能完善

---

## 🎯 核心功能补全优先级

### ✅ P0 - 已完成
- [x] WiFi STA连接
- [x] UART命令接口
- [x] AT命令解析器
- [x] DeepSeek LLM集成
- [x] HTTP管理面板
- [x] 本地工具（GPIO/温度）
- [x] NVS存储
- [x] 看门狗

### 🔄 P1 - 本周完成（无BLE后优先）

#### 1. WiFi功能增强
- [ ] **WiFi自动重连**
  ```rust
  // firmware/esp32-app/src/wifi_manager.rs (新建)
  - 断线检测
  - 指数退避重连
  - 信号强度监控
  - 连接质量报告
  ```

- [ ] **WiFi配置AP模式**
  ```rust
  // 首次配置用AP模式
  - SSID: mAgent-Setup-XXXX
  - Web配置界面
  - 完成后切换到STA
  ```

#### 2. TLS稳定性修复
- [ ] **HTTPS支持**
  ```rust
  // 使用释放的BLE内存优化SSL
  - 增加SSL缓冲区到16KB
  - 启用PSRAM存储TLS上下文
  - 证书验证
  - 错误处理改进
  ```

#### 3. 本地工具扩展
- [ ] **GPIO增强**
  ```rust
  // magent-core/src/local_tools.rs
  - PWM输出（LED调光）
  - 中断输入（按钮/传感器）
  - GPIO组控制
  - 状态持久化
  ```

- [ ] **I2C传感器支持**
  ```rust
  // 常见传感器
  - BME280（温湿度气压）
  - MPU6050（加速度陀螺仪）
  - VEML7700（光照）
  - AHT20（温湿度）
  ```

- [ ] **ADC读取**
  ```rust
  - 电池电压监控
  - 模拟传感器
  - 校准功能
  ```

#### 4. HTTP API扩展
- [ ] **RESTful控制**
  ```rust
  // firmware/esp32-app/src/http_api.rs (新建)
  GET  /api/status        - 系统状态
  POST /api/command       - 执行命令
  GET  /api/config        - 读取配置
  POST /api/config        - 更新配置
  GET  /api/tools         - 工具列表
  POST /api/tools/:name   - 执行工具
  GET  /api/logs          - 日志流
  ```

- [ ] **WebSocket实时通信**
  ```rust
  WS   /ws/agent         - Agent事件流
  WS   /ws/logs          - 日志流
  WS   /ws/metrics       - 性能指标
  ```

### 🟢 P2 - 本月完成

#### 5. OTA增强
- [ ] **OTA UI**
  - HTTP管理面板中的OTA页面
  - 上传固件文件
  - 进度显示
  - 版本管理

- [ ] **OTA安全**
  - 固件签名验证
  - 加密传输
  - 回滚测试

#### 6. 数据持久化
- [ ] **配置管理**
  ```rust
  // magent-core/src/config_store.rs (新建)
  - JSON配置文件
  - 配置版本控制
  - 导入/导出
  - 配置校验
  ```

- [ ] **日志系统**
  ```rust
  - 循环缓冲区日志
  - 日志级别过滤
  - 远程日志上传
  - 日志分析
  ```

#### 7. 性能监控
- [ ] **系统指标**
  ```rust
  // firmware/esp32-app/src/metrics.rs (新建)
  - CPU使用率
  - 内存使用
  - WiFi信号质量
  - 任务运行时间
  - 错误计数
  ```

- [ ] **健康检查**
  ```rust
  - 周期性自检
  - 异常检测
  - 自动恢复
  ```

### 🔵 P3 - 未来增强

#### 8. MQTT支持
- [ ] **MQTT客户端**
  ```rust
  - 连接到MQTT broker
  - 订阅/发布
  - QoS支持
  - 遗嘱消息
  ```

#### 9. 时间同步
- [ ] **NTP客户端**
  ```rust
  - 自动时间同步
  - 时区支持
  - RTC备份
  ```

#### 10. 更多LLM后端
- [ ] **多LLM支持**
  ```rust
  - OpenAI API
  - Claude API
  - 本地Ollama
  - 切换机制
  ```

---

## 📝 详细实现计划

### 1. WiFi自动重连（本周一）

**文件**: `firmware/esp32-app/src/wifi_manager.rs`

```rust
use esp_idf_svc::wifi::{EspWifi, WifiEvent};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct WifiManager {
    wifi: Arc<Mutex<EspWifi>>,
    ssid: String,
    password: String,
    retry_count: u32,
    max_retries: u32,
    backoff: Duration,
}

impl WifiManager {
    pub fn new(wifi: EspWifi, ssid: String, password: String) -> Self {
        Self {
            wifi: Arc::new(Mutex::new(wifi)),
            ssid,
            password,
            retry_count: 0,
            max_retries: 10,
            backoff: Duration::from_secs(1),
        }
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        // 连接逻辑
        // 1. 检查当前状态
        // 2. 如果未连接，尝试连接
        // 3. 失败时指数退避
        // 4. 达到最大次数后进入安全模式
    }

    pub fn monitor(&mut self) {
        // 后台监控线程
        // 1. 定期检查连接状态
        // 2. 检测到断线自动重连
        // 3. 记录信号强度
        // 4. 触发事件回调
    }

    pub fn get_signal_strength(&self) -> i8 {
        // 获取RSSI
    }

    pub fn get_connection_quality(&self) -> ConnectionQuality {
        // 综合评估连接质量
    }
}
```

### 2. TLS优化（本周二）

**文件**: `firmware/esp32-app/src/tls_config.rs`

```rust
use esp_idf_svc::http::client::Configuration;

pub fn create_tls_config() -> Configuration {
    Configuration {
        // 使用释放的BLE内存
        buffer_size: Some(16384),  // 16KB缓冲区
        buffer_size_tx: Some(4096),

        // PSRAM存储
        use_global_ca_store: true,

        // 证书验证
        crt_bundle_attach: Some(esp_crt_bundle_attach),

        // 超时设置
        timeout: Some(Duration::from_secs(30)),

        ..Default::default()
    }
}
```

### 3. I2C传感器（本周三）

**文件**: `magent-core/src/i2c_sensors.rs`

```rust
pub struct I2CSensorManager {
    i2c: I2C,
    sensors: Vec<Box<dyn I2CSensor>>,
}

pub trait I2CSensor {
    fn name(&self) -> &str;
    fn address(&self) -> u8;
    fn init(&mut self) -> Result<()>;
    fn read(&mut self) -> Result<SensorData>;
}

pub struct BME280Sensor {
    // BME280实现
}

impl I2CSensor for BME280Sensor {
    fn read(&mut self) -> Result<SensorData> {
        // 读取温湿度气压
    }
}
```

### 4. HTTP API扩展（本周四）

**文件**: `firmware/esp32-app/src/http_api.rs`

```rust
use esp_idf_svc::http::server::{EspHttpServer, Request, Response};

pub fn setup_api_routes(server: &mut EspHttpServer) -> Result<()> {
    // GET /api/status
    server.fn_handler("/api/status", Method::Get, |req| {
        let status = get_system_status();
        Response::new(200)
            .body(serde_json::to_string(&status)?)
            .header("Content-Type", "application/json")
    })?;

    // POST /api/command
    server.fn_handler("/api/command", Method::Post, |req| {
        let cmd: Command = serde_json::from_slice(req.body())?;
        let result = execute_command(cmd)?;
        Response::new(200)
            .body(serde_json::to_string(&result)?)
    })?;

    // WebSocket /ws/agent
    server.ws_handler("/ws/agent", |ws| {
        handle_agent_ws(ws)
    })?;

    Ok(())
}
```

### 5. 配置管理（本周五）

**文件**: `magent-core/src/config_store.rs`

```rust
#[derive(Serialize, Deserialize)]
pub struct AgentConfig {
    pub version: u32,
    pub wifi: WifiConfig,
    pub llm: LLMConfig,
    pub tools: Vec<ToolConfig>,
    pub system: SystemConfig,
}

pub struct ConfigStore {
    nvs: NVS,
    cache: AgentConfig,
}

impl ConfigStore {
    pub fn load(&mut self) -> Result<AgentConfig> {
        // 从NVS加载
        // 验证配置
        // 缓存到RAM
    }

    pub fn save(&mut self, config: &AgentConfig) -> Result<()> {
        // 验证配置
        // 写入NVS
        // 更新缓存
    }

    pub fn export_json(&self) -> Result<String> {
        // 导出为JSON
    }

    pub fn import_json(&mut self, json: &str) -> Result<()> {
        // 从JSON导入
        // 验证
        // 保存
    }
}
```

---

## 🧪 测试计划

### 单元测试
```bash
# 核心功能测试
cargo test -p magent-core

# 固件模块测试（需要mock）
cargo test -p magent-esp32-app --lib
```

### 集成测试
```bash
# WiFi重连测试
# 1. 断开路由器
# 2. 验证自动重连
# 3. 检查指数退避

# TLS测试
curl https://api.deepseek.com/health

# HTTP API测试
curl http://192.168.1.100/api/status
curl -X POST http://192.168.1.100/api/command \
  -H "Content-Type: application/json" \
  -d '{"cmd": "read temperature"}'
```

### 硬件测试
```
1. 长期稳定性（48小时）
2. WiFi漫游测试
3. 内存泄漏检查
4. OTA完整流程
5. 传感器精度验证
```

---

## 📊 完成度跟踪

```
进度: ██████░░░░░░░░░░░░░░ 30%

✅ 已完成:
  - BLE移除和优化
  - 文档更新
  - 配置文件

🔄 进行中:
  - WiFi自动重连
  - TLS优化

📅 计划中:
  - I2C传感器
  - HTTP API扩展
  - 配置管理
```

---

需要我开始实现这些功能吗？我可以从WiFi自动重连、TLS优化或任何你认为最重要的功能开始！
