# mAgent Usage Examples

## Quick Start

This guide provides practical examples for using the mAgent embedded AI agent library.

## Basic Agent Setup

```rust
use magent_core::{MiniAgent, AgentConfig};

// Create agent configuration
let config = AgentConfig::default();

// Initialize agent
let mut agent = MiniAgent::new(config);

// Run agent with a task
let result = agent.run("What is the current temperature?").await?;
```

## Sensor Integration

### Reading Temperature

```rust
use magent_core::hardware::TemperatureSensor;

// Create temperature sensor
let mut sensor = TemperatureSensor::new(0x48);

// Initialize sensor
sensor.init()?;

// Read temperature
let temp = sensor.read_temperature()?;
println!("Temperature: {:.1}°C", temp);
```

### Reading Accelerometer Data

```rust
use magent_core::hardware::Accelerometer;

// Create accelerometer
let mut sensor = Accelerometer::new(5);

// Initialize sensor
sensor.init()?;

// Read acceleration
let (x, y, z) = sensor.read_acceleration()?;
println!("Acceleration: X={:.2}g, Y={:.2}g, Z={:.2}g", x, y, z);
```

## GPIO Control

```rust
use magent_core::hardware::{GpioPin, GpioDirection, GpioState};

// Create GPIO pin as output
let mut led = GpioPin::new(10, GpioDirection::Output);

// Turn on LED
led.set(GpioState::High)?;

// Turn off LED
led.set(GpioState::Low)?;

// Toggle LED
led.toggle()?;
```

## Flash Storage

### KV Store Operations

```rust
use magent_core::storage::{FlashStorage, KvStore};

// Create flash storage
let flash = FlashStorage::new(base_address);

// Create KV store
let mut kv_store = KvStore::new(flash, 0x1000);

// Store configuration
kv_store.set("config", &[1, 2, 3, 4, 5])?;

// Retrieve configuration
if let Some(data) = kv_store.get("config")? {
    println!("Config data: {:?}", data);
}

// Delete configuration
kv_store.delete("config")?;

// Get statistics
let stats = kv_store.get_stats()?;
println!("Valid entries: {}", stats.valid_entries);
```

### Garbage Collection

```rust
// Run garbage collection
let collected = kv_store.garbage_collect()?;
println!("Collected {} deleted entries", collected);
```

## Tool Execution

### Using Tool Registry

```rust
use magent_core::tools::ToolRegistry;

// Create tool registry
let mut tools = ToolRegistry::new();

// Execute sensor read
let result = tools.execute("read_sensor", "temperature").await?;
println!("Tool result: {}", result.data);

// Execute GPIO write
let result = tools.execute("write_gpio", "pin=5,state=high").await?;
println!("Tool result: {}", result.data);
```

## Error Recovery

### Using Recovery Manager

```rust
use magent_core::recovery::{RecoveryManager, RecoveryStrategy};

// Create recovery manager
let recovery = RecoveryManager::with_defaults();

// Get strategy for error
let strategy = recovery.get_strategy(&error);
match strategy {
    RecoveryStrategy::Retry => println!("Will retry operation"),
    RecoveryStrategy::Fallback => println!("Will use fallback value"),
    RecoveryStrategy::Abort => println!("Will abort operation"),
    _ => {}
}

// Execute with retry logic
let result = recovery.execute_with_retry(|| {
    // Your operation here
    Ok(())
}).await?;
```

## Monitoring

### Logging

```rust
use magent_core::monitoring::{MonitoringManager, LogLevel};

// Create monitoring manager
let mut monitor = MonitoringManager::new();

// Log messages
monitor.log(LogLevel::Info, "Agent started")?;
monitor.log(LogLevel::Warning, "Memory usage high")?;
monitor.log(LogLevel::Error, "Sensor timeout")?;

// Get logs
for log in monitor.get_logs() {
    println!("{:?}: {}", log.level, log.message);
}
```

### Performance Metrics

```rust
// Record operation start
monitor.operation_start();

// Perform operation
let start = embassy_time::Instant::now();
// ... operation ...
let duration = start.elapsed().as_micros() as u32;

// Record operation success
monitor.operation_success(duration);

// Get metrics
let metrics = monitor.get_metrics();
println!("Success rate: {:.1}%", 
    (metrics.successful_operations as f32 / metrics.total_operations as f32) * 100.0);
```

### Health Checks

```rust
// Add health check
monitor.add_health_check("sensor", HealthStatus::Healthy, "Sensor OK")?;
monitor.add_health_check("flash", HealthStatus::Degraded, "Flash wear high")?;

// Get overall health
let health = monitor.get_health_status();
println!("System health: {:?}", health);

// Get individual checks
for check in monitor.get_health_checks() {
    println!("{}: {:?}", check.component, check.status);
}
```

## Safety Mechanisms

### Budget Enforcement

```rust
use magent_core::safety::{BudgetEnforcer, Watchdog};

// Create budget enforcer
let budget = BudgetEnforcer::new(50, 50 * 1024, 10000);

// Create watchdog
let watchdog = Watchdog::with_defaults();

// Check budget before operation
if !budget.check_iteration() {
    return Err(AgentError::IterationBudgetExhausted {
        used: budget.get_iteration_count(),
        limit: 50,
    });
}

// Feed watchdog
watchdog.feed();
```

## Complete Example

```rust
use magent_core::*;
use magent_core::hardware::*;

async fn run_agent() -> Result<()> {
    // Initialize monitoring
    let mut monitor = MonitoringManager::new();
    monitor.log(LogLevel::Info, "Starting agent")?;

    // Initialize sensors
    let mut temp_sensor = TemperatureSensor::new(0x48);
    temp_sensor.init()?;
    
    let mut accel = Accelerometer::new(5);
    accel.init()?;

    // Initialize GPIO
    let mut led = GpioPin::new(10, GpioDirection::Output);

    // Initialize storage
    let flash = FlashStorage::new(0x1000);
    let mut kv_store = KvStore::new(flash, 0x2000);

    // Create agent
    let config = AgentConfig::default();
    let mut agent = MiniAgent::new(config);

    // Run agent
    monitor.operation_start();
    let result = agent.run("Monitor environment").await?;
    monitor.operation_success(1000);

    // Read sensors
    let temp = temp_sensor.read_temperature()?;
    let (x, y, z) = accel.read_acceleration()?;

    // Store data
    let mut data = Vec::new();
    let _ = data.push(temp as u8);
    kv_store.set("temperature", &data)?;

    // Control LED based on temperature
    if temp > 30.0 {
        led.set(GpioState::High)?;
    } else {
        led.set(GpioState::Low)?;
    }

    monitor.log(LogLevel::Info, "Agent completed successfully")?;
    Ok(())
}
```

## Advanced Patterns

### Sensor Fusion

```rust
// Combine multiple sensor readings
let temp = temp_sensor.read_temperature()?;
let humidity = humidity_sensor.read_humidity()?;
let pressure = pressure_sensor.read_pressure()?;

// Calculate heat index
let heat_index = calculate_heat_index(temp, humidity);
```

### Batch Operations

```rust
// Perform multiple operations efficiently
let operations = vec![
    ("read_sensor", "temperature"),
    ("read_sensor", "accelerometer"),
    ("flash_read", "address=0,length=256"),
];

for (tool, args) in operations {
    let result = tools.execute(tool, args).await?;
    println!("{}: {}", tool, result.data);
}
```

### Error Handling

```rust
// Comprehensive error handling
match agent.run(task).await {
    Ok(result) => {
        monitor.log(LogLevel::Info, "Task completed")?;
        Ok(result)
    }
    Err(AgentError::IterationBudgetExhausted { .. }) => {
        monitor.log(LogLevel::Warning, "Budget exhausted")?;
        // Handle budget exhaustion
        Err(error)
    }
    Err(AgentError::SensorReadFailed { .. }) => {
        monitor.log(LogLevel::Error, "Sensor failed")?;
        // Use fallback value
        Ok(fallback_value)
    }
    Err(error) => {
        monitor.log(LogLevel::Error, "Unknown error")?;
        Err(error)
    }
}
```

## Tips and Best Practices

1. **Always initialize sensors before use**
2. **Use monitoring to track performance**
3. **Implement proper error recovery**
4. **Respect memory and iteration budgets**
5. **Feed watchdog regularly**
6. **Use KV store for persistent configuration**
7. **Validate all inputs**
8. **Handle sensor timeouts gracefully**
9. **Use appropriate log levels**
10. **Test with simulation before real hardware**
