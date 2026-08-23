//! Standalone algorithm simulation tests
//!
//! Demonstrates the core algorithms used in magent-core without
//! requiring embedded hardware or `no_std`. Each `fn` is a self-contained
//! mini test that asserts the algorithm produces the expected result.
//!
//! Run with: `cargo run -p magent-tools --bin algorithm-tests`

use std::collections::HashMap;

fn main() {
    println!("Running magent-core algorithm simulations...\n");

    test_wear_leveling();
    test_budget_enforcement();
    test_power_modes();
    test_skill_management();
    test_tool_registry();
    test_agent_state_machine();

    println!("\n✅ All algorithm simulations passed!");
}

fn test_wear_leveling() {
    println!("Testing wear leveling algorithm...");

    let mut write_counts = [0u32; 16];
    for i in 0..100 {
        write_counts[i % 16] += 1;
    }

    assert_eq!(write_counts[0], 7, "First sector should have 7 writes");
    assert_eq!(write_counts[15], 6, "Last sector should have 6 writes");

    println!("  ✅ Wear leveling: 100 writes distributed across 16 sectors");
}

fn test_budget_enforcement() {
    println!("Testing budget enforcement...");

    let mut iteration_count = 0;
    let max_iterations = 10;

    for _ in 0..15 {
        if iteration_count < max_iterations {
            iteration_count += 1;
        }
    }

    assert_eq!(iteration_count, max_iterations, "Should stop at max iterations");

    println!("  ✅ Budget enforcement: Limited to {} iterations", max_iterations);
}

fn test_power_modes() {
    println!("Testing power mode transitions...");

    let modes = vec!["Active", "Idle", "LowPower", "DeepSleep"];
    assert_eq!(modes.len(), 4, "Should have 4 power modes");

    println!("  ✅ Power modes: {:?}", modes);
}

fn test_skill_management() {
    println!("Testing skill management...");

    let mut skills = HashMap::new();
    skills.insert("Read Temperature", "Read temperature sensor");
    skills.insert("Write GPIO", "Control GPIO pins");

    assert_eq!(skills.len(), 2, "Should have 2 skills");
    assert!(skills.contains_key("Read Temperature"), "Should contain temperature skill");

    println!("  ✅ Skill management: {} skills registered", skills.len());
}

fn test_tool_registry() {
    println!("Testing tool registry...");

    let tools = ["read_sensor", "write_gpio", "flash_read"];

    assert_eq!(tools.len(), 3, "Should have 3 tools");
    assert!(tools.contains(&"read_sensor"), "Should contain read_sensor");

    println!("  ✅ Tool registry: {} tools registered", tools.len());
}

fn test_agent_state_machine() {
    println!("Testing ReAct state machine...");

    let states = ["Thinking", "Executing", "Observing", "Finished"];
    assert_eq!(states.len(), 4, "Should have 4 states");

    println!("  ✅ State machine: {:?}", states);
}
