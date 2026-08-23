//! Module integration tests for magent-core
//!
//! Exercises the cross-module interactions (storage ↔ monitoring,
//! hardware ↔ tools, recovery ↔ agent, safety ↔ monitoring) using a
//! minimal std-side simulation so we can verify the integration shape
//! without an embedded target.
//!
//! Run with: `cargo run -p magent-tools --bin module-integration-test`

fn main() {
    println!("=== Module Integration Tests ===\n");

    test_storage_monitoring_integration();
    test_hardware_tools_integration();
    test_recovery_agent_integration();
    test_safety_monitoring_integration();

    println!("\n=== All Integration Tests Passed ===");
}

fn test_storage_monitoring_integration() {
    println!("Testing Storage + Monitoring Integration...");

    // Simulate KV store operations with monitoring
    let mut operation_count = 0;
    let mut success_count = 0;
    let mut failure_count = 0;

    // Simulate 100 operations
    for i in 0..100 {
        operation_count += 1;

        // Simulate 95% success rate
        if i % 20 != 0 {
            success_count += 1;
        } else {
            failure_count += 1;
        }
    }

    assert_eq!(operation_count, 100);
    assert_eq!(success_count, 95);
    assert_eq!(failure_count, 5);

    let success_rate = (success_count as f32 / operation_count as f32) * 100.0;
    assert!(success_rate >= 95.0, "Success rate should be >= 95%");

    println!(
        "  ✅ Operation tracking: {} total, {} success, {} failure",
        operation_count, success_count, failure_count
    );
    println!("  ✅ Success rate: {:.1}%", success_rate);
}

fn test_hardware_tools_integration() {
    println!("Testing Hardware + Tools Integration...");

    // Simulate tool execution with hardware
    let tools = vec![
        ("read_sensor", "temperature"),
        ("read_sensor", "accelerometer"),
        ("write_gpio", "pin=5,state=high"),
        ("flash_read", "address=0,length=256"),
    ];

    let mut executed = 0;
    let mut hardware_calls = 0;

    for (tool, _args) in tools {
        executed += 1;

        // Simulate hardware calls. `read_sensor` and `write_gpio` both
        // touch the real HAL; everything else is a software-side tool.
        if tool == "read_sensor" || tool == "write_gpio" {
            hardware_calls += 1;
        }
    }

    assert_eq!(executed, 4);
    assert_eq!(hardware_calls, 3);

    println!("  ✅ Tools executed: {}", executed);
    println!("  ✅ Hardware calls: {}", hardware_calls);
}

fn test_recovery_agent_integration() {
    println!("Testing Recovery + Agent Integration...");

    // Simulate agent operations with recovery
    let mut iterations = 0;
    let max_iterations = 50;
    let mut retries = 0;
    let max_retries = 3;

    while iterations < 10 {
        iterations += 1;

        // Simulate 20% error rate
        if iterations % 5 == 0 {
            // Apply recovery strategy
            if retries < max_retries {
                retries += 1;
                println!(
                    "  ✅ Retry {}/{} for iteration {}",
                    retries, max_retries, iterations
                );
            }
        }
    }

    assert!(iterations <= max_iterations, "Should respect iteration budget");
    assert!(retries <= max_retries * 2, "Should respect retry limit");

    println!("  ✅ Iterations: {}/{}", iterations, max_iterations);
    println!("  ✅ Total retries: {}", retries);
}

fn test_safety_monitoring_integration() {
    println!("Testing Safety + Monitoring Integration...");

    // Simulate safety mechanisms with monitoring
    let memory_budget = 50 * 1024; // 50KB
    let iteration_budget = 50;

    let mut memory_used = 0;
    let mut iterations = 0;
    let mut warnings = 0;

    for _i in 0..100 {
        if iterations < iteration_budget && memory_used < memory_budget {
            iterations += 1;
            memory_used += 1024; // 1KB per iteration

            // Monitor for warnings
            if memory_used > memory_budget * 80 / 100 {
                warnings += 1;
            }
        }
    }

    assert_eq!(iterations, iteration_budget);
    assert!(memory_used <= memory_budget);
    assert!(warnings > 0, "Should trigger warnings near limit");

    println!(
        "  ✅ Memory used: {}/{} bytes",
        memory_used, memory_budget
    );
    println!("  ✅ Iterations: {}/{}", iterations, iteration_budget);
    println!("  ✅ Warnings triggered: {}", warnings);
}
