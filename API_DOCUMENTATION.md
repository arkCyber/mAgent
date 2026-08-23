# mAgent API Documentation

## Core Modules

### Agent Module

#### `MiniAgent`

The main agent implementation using the ReAct pattern.

```rust
pub struct MiniAgent {
    config: AgentConfig,
    state: AgentState,
    budget: BudgetEnforcer,
    watchdog: Watchdog,
    skills: SkillsManager,
    tools: ToolRegistry,
    conversation: Vec<Message, 10>,
    current_task: String<MAX_BUFFER_SIZE>,
}
```

**Methods:**
- `new(config: AgentConfig) -> Self` - Create new agent
- `run(&mut self, task: &str) -> Result<String<MAX_BUFFER_SIZE>>` - Run agent with task
- `think(&mut self) -> Result<LlmResponse>` - Think phase
- `execute_tool(&mut self, tool_call: &ToolCall) -> Result<ToolResult>` - Execute tool
- `observe(&mut self, result: &ToolResult) -> Result<()>` - Observe result

#### `AgentState`

Agent state machine states.

```rust
pub enum AgentState {
    Thinking,
    Executing,
    Observing,
    Finished,
}
```

### Tools Module

#### `ToolRegistry`

Registry for available tools.

```rust
pub struct ToolRegistry {
    tools: Vec<Tool, 8>,
}
```

**Methods:**
- `new() -> Self` - Create new registry
- `register(&mut self, tool: Tool) -> Result<()>` - Register tool
- `execute(&self, tool_name: &str, args: &str) -> Result<ToolResult>` - Execute tool

#### `Tool`

Tool definition.

```rust
pub struct Tool {
    pub name: String<32>,
    pub description: String<128>,
    pub tool_type: ToolType,
}
```

### Storage Module

#### `FlashStorage`

Flash storage abstraction.

```rust
pub struct FlashStorage<F> {
    flash: F,
    sector_size: usize,
}
```

**Methods:**
- `new(base_address: u32) -> Self` - Create flash storage
- `read(&self, address: u32, buf: &mut [u8]) -> Result<()>` - Read from flash
- `write(&self, address: u32, data: &[u8]) -> Result<()> - Write to flash
- `erase(&self, sector: u32) -> Result<()>` - Erase sector
- `sector_size(&self) -> usize` - Get sector size

#### `KvStore`

Key-value store on flash.

```rust
pub struct KvStore<F> {
    storage: FlashStorage<F>,
    base_address: u32,
}
```

**Methods:**
- `new(storage: FlashStorage<F>, base_address: u32) -> Self` - Create KV store
- `get(&mut self, key: &str) -> Result<Option<Vec<u8, 256>>>` - Get value
- `set(&mut self, key: &str, value: &[u8]) -> Result<()>` - Set value
- `delete(&mut self, key: &str) -> Result<()>` - Delete key
- `garbage_collect(&mut self) -> Result<usize>` - Run garbage collection
- `get_stats(&mut self) -> Result<KvStoreStats>` - Get statistics

### Hardware Module

#### `I2cSensor`

I2C sensor interface.

```rust
pub struct I2cSensor {
    address: u8,
    initialized: bool,
}
```

**Methods:**
- `new(address: u8) -> Self` - Create I2C sensor
- `init(&mut self) -> Result<()>` - Initialize sensor
- `read(&self, register: u8) -> Result<Vec<u8, 8>>` - Read from register
- `write(&self, register: u8, value: u8) -> Result<()> - Write to register

#### `SpiSensor`

SPI sensor interface.

```rust
pub struct SpiSensor {
    cs_pin: u8,
    initialized: bool,
}
```

**Methods:**
- `new(cs_pin: u8) -> Self` - Create SPI sensor
- `init(&mut self) -> Result<()>` - Initialize sensor
- `read(&self, register: u8) -> Result<Vec<u8, 8>>` - Read from register
- `write(&self, register: u8, value: u8) -> Result<()> - Write to register

#### `GpioPin`

GPIO pin interface.

```rust
pub struct GpioPin {
    pin: u8,
    direction: GpioDirection,
    state: GpioState,
}
```

**Methods:**
- `new(pin: u8, direction: GpioDirection) -> Self` - Create GPIO pin
- `set(&mut self, state: GpioState) -> Result<()>` - Set pin state
- `read(&self) -> Result<GpioState>` - Read pin state
- `toggle(&mut self) -> Result<()>` - Toggle pin state

### Monitoring Module

#### `MonitoringManager`

Monitoring and logging system.

```rust
pub struct MonitoringManager {
    logs: Vec<LogEntry, 64>,
    metrics: PerformanceMetrics,
    health_checks: Vec<HealthCheck, 16>,
}
```

**Methods:**
- `new() -> Self` - Create monitoring manager
- `log(&mut self, level: LogLevel, message: &str) -> Result<()>` - Log message
- `get_logs(&self) -> &[LogEntry]` - Get logs
- `operation_start(&mut self)` - Record operation start
- `operation_success(&mut self, execution_time_us: u32)` - Record success
- `operation_failure(&mut self)` - Record failure
- `get_metrics(&self) -> &PerformanceMetrics` - Get metrics
- `add_health_check(&mut self, component: &str, status: HealthStatus, message: &str) -> Result<()>` - Add health check
- `get_health_status(&self) -> HealthStatus` - Get overall health
- `get_health_checks(&self) -> &[HealthCheck]` - Get health checks

### Recovery Module

#### `RecoveryManager`

Error recovery management.

```rust
pub struct RecoveryManager {
    max_retries: u8,
    backoff_base_ms: u32,
}
```

**Methods:**
- `new(max_retries: u8, backoff_base_ms: u32) -> Self` - Create recovery manager
- `get_strategy(&self, error: &AgentError) -> RecoveryStrategy` - Get recovery strategy
- `calculate_backoff(&self, retry_count: u8) -> u32` - Calculate backoff delay
- `should_retry(&self, error: &AgentError, retry_count: u8) -> bool` - Check if should retry
- `execute_with_retry<F, Fut, T>(&self, operation: F) -> Result<T>` - Execute with retry

### Safety Module

#### `BudgetEnforcer`

Resource budget enforcement.

```rust
pub struct BudgetEnforcer {
    iteration_limit: usize,
    memory_limit: usize,
    time_limit_ms: u32,
    iteration_count: usize,
    memory_used: usize,
    start_time: u32,
}
```

**Methods:**
- `new(iteration_limit: usize, memory_limit: usize, time_limit_ms: u32) -> Self` - Create enforcer
- `check_iteration(&mut self) -> bool` - Check iteration budget
- `check_memory(&mut self, additional: usize) -> bool` - Check memory budget
- `check_time(&self) -> bool` - Check time budget
- `get_iteration_count(&self) -> usize` - Get iteration count

#### `Watchdog`

Watchdog timer.

```rust
pub struct Watchdog {
    timeout_ms: u32,
    last_feed: u32,
}
```

**Methods:**
- `new(timeout_ms: u32) -> Self` - Create watchdog
- `feed(&mut self)` - Feed watchdog
- `check(&self) -> bool` - Check if watchdog triggered

## Error Types

### `AgentError`

Main error type for all operations.

```rust
pub enum AgentError {
    MemoryAllocationFailed { requested: usize, available: usize },
    StackOverflow { used: usize, limit: usize },
    NetworkConnectionFailed { reason: NetworkError },
    NetworkTimeout { operation: &'static str, duration_ms: u32 },
    StorageWriteFailed { address: u32, reason: StorageError },
    StorageReadFailed { address: u32, reason: StorageError },
    SensorReadFailed { sensor: &'static str, reason: SensorError },
    GpioOperationFailed { pin: u8, operation: GpioOperation },
    InputValidationFailed { field: &'static str, reason: ValidationError },
    IterationBudgetExhausted { used: usize, limit: usize },
    MemoryBudgetExhausted { used: usize, limit: usize },
    OperationTimeout { operation: &'static str, timeout_ms: u32 },
    InvalidStateTransition { from: &'static str, to: &'static str },
    ConfigurationError { field: &'static str, reason: ConfigError },
    Unknown { code: u32 },
}
```

## Constants

```rust
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_MEMORY_BUDGET: usize = 50 * 1024; // 50KB
pub const MAX_ITERATION_BUDGET: usize = 50;
pub const MAX_BUFFER_SIZE: usize = 2048;
pub const MAX_CONCURRENT_TOOLS: usize = 3;
pub const WATCHDOG_TIMEOUT_SECS: u64 = 10;
pub const AGENT_STACK_SIZE: usize = 8192; // 8KB
pub const COMM_STACK_SIZE: usize = 4096; // 4KB
pub const STORAGE_STACK_SIZE: usize = 2048; // 2KB
```

## Type Aliases

```rust
pub type Result<T> = core::result::Result<T, AgentError>;
```

## Re-exports

```rust
pub use agent::{MiniAgent, AgentState};
pub use config::AgentConfig;
pub use error::{AgentError, Result};
pub use power::{PowerManager, PowerMode, BatteryStatus};
pub use safety::{BudgetEnforcer, Watchdog};
pub use security::{SecurityManager, EncryptionMode, SecurityLevel};
pub use skills::{Skill, SkillsManager};
pub use tools::{Tool, ToolRegistry, ToolType};
pub use wear_leveling::{WearLeveler, WearLevelingStrategy};
pub use storage::{FlashStorage, KvStore};
pub use communication::BleClient;
pub use hardware::{I2cSensor, SpiSensor, GpioPin, GpioDirection, GpioState};
pub use hardware::{TemperatureSensor, Accelerometer, HumiditySensor, PressureSensor};
```
