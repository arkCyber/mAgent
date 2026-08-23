# mAgent API Reference

## Core API

### MiniAgent

The main agent struct that manages the ReAct loop and tool execution.

#### Methods

##### `new(config: AgentConfig) -> Result<Self>`

Create a new agent with the given configuration.

**Parameters:**
- `config`: Agent configuration

**Returns:**
- `Result<MiniAgent>`: Agent instance or error

**Example:**
```rust
let config = AgentConfig::new()
    .unwrap()
    .with_max_iterations(50)
    .unwrap();
let agent = MiniAgent::new(config)?;
```

##### `run(&mut self, task: &str) -> Result<String>`

Execute a task using the ReAct loop.

**Parameters:**
- `task`: Task description

**Returns:**
- `Result<String>`: Task result or error

**Example:**
```rust
let result = agent.run("Read temperature sensor").await?;
```

##### `reset(&mut self)`

Reset agent state for a new task.

**Example:**
```rust
agent.reset();
```

##### `state(&self) -> AgentState`

Get current agent state.

**Returns:**
- `AgentState`: Current state (Thinking, Executing, Observing, Finished)

##### `budget(&self) -> &BudgetEnforcer`

Get reference to budget enforcer.

##### `watchdog(&self) -> &Watchdog`

Get reference to watchdog.

##### `skills(&mut self) -> &mut SkillsManager`

Get mutable reference to skills manager.

##### `tools(&mut self) -> &mut ToolRegistry`

Get mutable reference to tools registry.

---

## Safety API

### BudgetEnforcer

Enforces iteration, memory, and time budgets.

#### Methods

##### `new(iteration_limit: usize, memory_limit: usize, time_limit_ms: u32) -> Self`

Create a new budget enforcer with specified limits.

##### `consume_iteration(&self) -> Result<()>`

Consume one iteration from the budget.

##### `consume_memory(&self, bytes: usize) -> Result<()>`

Consume memory from the budget.

##### `release_memory(&self, bytes: usize)`

Release memory back to the budget.

##### `reset_iteration(&self)`

Reset iteration budget to zero.

##### `reset_memory(&self)`

Reset memory budget to zero.

##### `iteration_usage(&self) -> usize`

Get current iteration usage.

##### `memory_usage(&self) -> usize`

Get current memory usage.

---

### Watchdog

Hardware watchdog timer interface.

#### Methods

##### `new(timeout_ms: u32) -> Self`

Create a new watchdog with specified timeout.

##### `feed(&self)`

Feed the watchdog (reset timer).

##### `needs_feed(&self) -> bool`

Check if watchdog needs feeding.

##### `timeout_ms(&self) -> u32`

Get timeout in milliseconds.

---

### StackMonitor

Monitor stack depth to prevent overflow.

#### Methods

##### `new(stack_base: usize, stack_limit: usize) -> Self`

Create a new stack monitor.

##### `check_depth(&self, current_sp: usize) -> Result<()>`

Check current stack depth.

##### `current_depth(&self) -> usize`

Get current stack depth.

---

### FaultDetector

Detect and classify system faults.

#### Methods

##### `new(error_threshold: usize) -> Self`

Create a new fault detector.

##### `report_error(&self, error: &AgentError) -> Result<()>`

Report an error to the fault detector.

##### `reset(&self)`

Reset error count.

##### `error_count(&self) -> usize`

Get current error count.

---

## Skills API

### SkillsManager

Manage skills storage and retrieval.

#### Methods

##### `new(max_skills: usize) -> Self`

Create a new skills manager.

##### `add(&mut self, skill: Skill) -> Result<()>`

Add a skill to the manager.

##### `search(&self, keyword: &str) -> Vec<&Skill, MAX_SKILLS>`

Search for skills by keyword.

##### `get(&self, name: &str) -> Option<&Skill>`

Get a skill by name.

##### `all(&self) -> &[Skill]`

Get all skills.

##### `remove(&mut self, name: &str) -> Result<()>`

Remove a skill by name.

##### `clear(&mut self)`

Clear all skills.

##### `count(&self) -> usize`

Get skill count.

---

### Skill

Represents a single skill.

#### Methods

##### `new(name: &str, description: &str, category: &str, content: &str) -> Result<Self>`

Create a new skill.

##### `validate(&self) -> Result<()>`

Validate skill data.

##### `increment_usage(&mut self)`

Increment usage count.

##### `update_success_rate(&mut self, success: bool)`

Update success rate.

##### `to_injection_string(&self) -> String<MAX_SKILL_CONTENT>`

Convert skill to injection string for LLM.

---

## Tools API

### ToolRegistry

Register and execute tools.

#### Methods

##### `new() -> Self`

Create a new tool registry.

##### `register(&mut self, tool: Tool) -> Result<()>`

Register a tool.

##### `execute(&self, call: &ToolCall) -> Result<ToolResult>`

Execute a tool call.

---

### Tool

Represents a tool definition.

#### Fields

- `name: String<32>`: Tool name
- `description: String<128>`: Tool description
- `tool_type: ToolType`: Tool type

---

### ToolType

Tool type enumeration.

#### Variants

- `ReadSensor`: Read sensor value
- `WriteGpio`: Write GPIO pin
- `FlashRead`: Read from flash
- `FlashWrite`: Write to flash
- `BleSend`: Send via BLE

---

### ToolCall

Represents a tool call from LLM.

#### Fields

- `name: String<32>`: Tool name
- `arguments: String<128>`: Tool arguments (JSON)

---

### ToolResult

Represents a tool execution result.

#### Fields

- `tool_name: String<32>`: Tool name
- `data: String<256>`: Result data
- `success: bool`: Success flag
- `error: Option<String<64>>`: Error message if failed

---

## Storage API

### FlashStorage<F>

Flash storage wrapper with wear leveling.

#### Methods

##### `new(flash: F) -> Self`

Create a new flash storage.

##### `read(&mut self, offset: u32, buf: &mut [u8]) -> Result<()>`

Read data from flash.

##### `write(&mut self, offset: u32, data: &[u8]) -> Result<()>`

Write data to flash.

##### `erase(&mut self, sector: u32) -> Result<()>`

Erase a flash sector.

##### `sector_size(&self) -> usize`

Get sector size.

##### `page_size(&self) -> usize`

Get page size.

---

### KvStore<F>

Simple key-value store in flash.

#### Methods

##### `new(storage: FlashStorage<F>, base_address: u32) -> Self`

Create a new KV store.

##### `get(&mut self, key: &str) -> Result<Option<Vec<u8, 256>>>`

Get a value by key.

##### `set(&mut self, key: &str, value: &[u8]) -> Result<()>`

Set a value by key.

##### `delete(&mut self, key: &str) -> Result<()>`

Delete a key.

---

## Communication API

### BleClient

BLE client for cloud communication.

#### Methods

##### `new(timeout_ms: u32) -> Self`

Create a new BLE client.

##### `connect(&mut self) -> Result<()>`

Connect to gateway.

##### `disconnect(&mut self) -> Result<()>`

Disconnect from gateway.

##### `is_connected(&self) -> bool`

Check if connected.

##### `send_request(&self, prompt: &str) -> Result<String>`

Send request to cloud LLM API.

##### `send_tool_result(&self, result: &ToolResult) -> Result<()>`

Send tool result to cloud.

##### `receive_response(&self) -> Result<LlmResponse>`

Receive response from cloud.

---

### BleMessage

BLE message for communication.

#### Methods

##### `new(message_type: MessageType, message_id: u32, payload: &str) -> Result<Self>`

Create a new BLE message.

##### `to_bytes(&self) -> Result<Vec<u8, 512>>`

Serialize to bytes.

##### `from_bytes(bytes: &[u8]) -> Result<Self>`

Deserialize from bytes.

---

### MessageType

Message type enumeration.

#### Variants

- `LlmRequest`: LLM request
- `LlmResponse`: LLM response
- `ToolCall`: Tool call
- `ToolResult`: Tool result
- `Heartbeat`: Heartbeat
- `Error`: Error

---

## Configuration API

### AgentConfig

Agent configuration.

#### Methods

##### `new() -> Self`

Create a new configuration with defaults.

##### `validate(&self) -> Result<()>`

Validate configuration.

##### `with_name(self, name: &str) -> Result<Self>`

Set agent name.

##### `with_max_iterations(self, max: u16) -> Result<Self>`

Set max iterations.

##### `with_max_memory(self, max: u32) -> Result<Self>`

Set max memory.

##### `to_bytes(&self) -> Result<Vec<u8, 256>>`

Serialize to bytes.

##### `from_bytes(bytes: &[u8]) -> Result<Self>`

Deserialize from bytes.

---

## Error API

### AgentError

Comprehensive error type.

#### Methods

##### `category(&self) -> ErrorCategory`

Get error category.

##### `recovery_strategy(&self) -> RecoveryStrategy`

Get recommended recovery strategy.

##### `is_fatal(&self) -> bool`

Check if error is fatal.

---

### ErrorCategory

Error category enumeration.

#### Variants

- `Memory`: Memory-related errors
- `Network`: Network/communication errors
- `Storage`: Storage/flash errors
- `Hardware`: Sensor/hardware errors
- `Validation`: Input validation errors
- `Budget`: Budget exhaustion errors
- `Timeout`: Timeout errors
- `Unknown`: Unknown errors

---

### RecoveryStrategy

Recovery strategy enumeration.

#### Variants

- `RetryImmediate`: Retry immediately
- `RetryBackoff`: Retry with exponential backoff
- `Skip`: Skip and continue
- `Degrade`: Graceful degradation
- `Fatal`: Fatal error, requires reset

---

## Constants

### Memory Limits

- `MAX_MEMORY_BUDGET: usize = 50 * 1024` (50KB)
- `MAX_BUFFER_SIZE: usize = 2048` (2KB)
- `MAX_CONCURRENT_TOOLS: usize = 3`

### Execution Limits

- `MAX_ITERATION_BUDGET: usize = 50`
- `WATCHDOG_TIMEOUT_SECS: u64 = 10`

### Stack Sizes

- `AGENT_STACK_SIZE: usize = 8192` (8KB)
- `COMM_STACK_SIZE: usize = 4096` (4KB)
- `STORAGE_STACK_SIZE: usize = 2048` (2KB)

### Storage Limits

- `MAX_SKILLS: usize = 10`
- `MAX_TOOLS: usize = 16`
- `MAX_MESSAGE_SIZE: usize = 512`

---

## Example Usage

### Basic Agent Usage

```rust
use magent_core::agent::MiniAgent;
use magent_core::config::AgentConfig;

#[embassy_executor::main]
async fn main(spawner: Spawner) -> ! {
    // Create configuration
    let config = AgentConfig::new()
        .unwrap()
        .with_max_iterations(50)
        .unwrap();

    // Create agent
    let mut agent = MiniAgent::new(config)?;

    // Run task
    let result = agent.run("Read temperature sensor").await?;
    
    info!("Result: {}", result);
    
    loop {
        agent.watchdog().feed();
        Timer::after(Duration::from_secs(1)).await;
    }
}
```

### Skill Management

```rust
use magent_core::skills::{Skill, SkillsManager};

let mut skills = SkillsManager::new(10);

let skill = Skill::new(
    "Temperature Monitor",
    "Monitor temperature sensor",
    "sensor",
    "Steps:\n1. Read sensor\n2. Check value\n3. Alert if needed",
)?;

skills.add(skill)?;

let results = skills.search("temperature");
```

### Tool Execution

```rust
use magent_core::tools::{ToolRegistry, ToolCall, ToolType};

let mut registry = ToolRegistry::new();

let call = ToolCall {
    name: String::from("read_sensor"),
    arguments: String::from(r#"{"sensor":"temperature"}"#),
};

let result = registry.execute(&call).await?;
```

### Budget Enforcement

```rust
use magent_core::safety::BudgetEnforcer;

let budget = BudgetEnforcer::with_defaults();

// Consume resources
budget.consume_iteration()?;
budget.consume_memory(1024)?;

// Check usage
let iter_used = budget.iteration_usage();
let mem_used = budget.memory_usage();

// Release resources
budget.release_memory(512);
```

### Error Handling

```rust
use magent_core::error::{AgentError, Result};

async fn safe_operation() -> Result<String> {
    // Operation that may fail
    match some_operation() {
        Ok(result) => Ok(result),
        Err(e) => {
            // Check error category
            match e.category() {
                ErrorCategory::Network => {
                    // Retry with backoff
                    retry_operation().await
                }
                ErrorCategory::Memory => {
                    // Fatal error
                    Err(e)
                }
                _ => Err(e),
            }
        }
    }
}
```
