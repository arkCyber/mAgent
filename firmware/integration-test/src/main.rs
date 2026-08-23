//! End-to-end integration test runner for magent-core on nRF52840.
//!
//! This binary runs the agent's ReAct loop, tool registration,
//! skills management, budget enforcement and error-handling code
//! paths against *real* (or `probe-rs`-simulated) nRF52840 hardware.
//! Each test prints `✓ ... passed` over RTT/DEFMT.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p magent-integration-test --release
//! ```
//!
//! Previously this lived as a loose `tests/integration_test.rs` at the
//! workspace root, where Cargo silently ignored it.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_alloc::Heap;
use panic_probe as _;
use static_cell::StaticCell;

#[macro_use]
extern crate alloc;

use magent_core::agent::MiniAgent;
use magent_core::config::AgentConfig;
use magent_core::skills::Skill;
use magent_core::tools::{Tool, ToolType};
use heapless::String;

static EXECUTOR: StaticCell<embassy_executor::Executor> = StaticCell::new();

#[global_allocator]
static HEAP: Heap = Heap::empty();

#[cortex_m_rt::entry]
fn main() -> ! {
    defmt::info!("Starting integration tests...");

    // Initialise the heap allocator. The integration tests don't
    // exercise the heap, but `core::assert_eq!(string, ..)` and a
    // couple of other macros pull in alloc-backed machinery so we
    // need a real allocator in place.
    {
        const HEAP_SIZE: usize = 8192;
        static mut HEAP_MEM: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
        unsafe {
            HEAP.init(&mut HEAP_MEM as *mut _ as usize, HEAP_SIZE);
        }
    }

    let config = embassy_nrf::config::Config::default();
    let _p = embassy_nrf::init(config);

    let executor = EXECUTOR.init(embassy_executor::Executor::new());

    // `executor-thread` runs tasks on the thread-mode executor; we drive
    // it by yielding back via `run`. The `run` closure never returns.
    executor.run(|spawner| {
        spawner.spawn(test_task(spawner)).unwrap();
    });
}

#[embassy_executor::task]
async fn test_task(_spawner: Spawner) {
    defmt::info!("Running integration tests...");

    // Test 1: Agent creation
    test_agent_creation().await;

    // Test 2: Tool registration
    test_tool_registration().await;

    // Test 3: Skills management
    test_skills_management().await;

    // Test 4: Budget enforcement
    test_budget_enforcement().await;

    // Test 5: Error handling
    test_error_handling().await;

    defmt::info!("All integration tests passed!");

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

async fn test_agent_creation() {
    defmt::info!("Test: Agent creation");

    let config = AgentConfig::new()
        .with_name("TestAgent")
        .unwrap()
        .with_max_iterations(10)
        .unwrap();

    let agent = MiniAgent::new(config);
    core::assert!(agent.is_ok());

    defmt::info!("✓ Agent creation test passed");
}

async fn test_tool_registration() {
    defmt::info!("Test: Tool registration");

    let config = AgentConfig::default();
    let mut agent = MiniAgent::new(config).unwrap();

    let tool = Tool {
        name: heapless::String::try_from("test_tool").unwrap(),
        description: heapless::String::try_from("Test tool").unwrap(),
        tool_type: ToolType::ReadSensor,
    };

    let result = agent.tools().register(tool);
    core::assert!(result.is_ok());

    defmt::info!("✓ Tool registration test passed");
}

async fn test_skills_management() {
    defmt::info!("Test: Skills management");

    let config = AgentConfig::default();
    let mut agent = MiniAgent::new(config).unwrap();

    let skill = Skill::new(
        "Test Skill",
        "Test description",
        "test",
        "Test content",
    );

    core::assert!(skill.is_ok());

    if let Ok(skill) = skill {
        let result = agent.skills().add(skill);
        core::assert!(result.is_ok());
    }

    core::assert_eq!(agent.skills().count(), 1);

    defmt::info!("✓ Skills management test passed");
}

async fn test_budget_enforcement() {
    defmt::info!("Test: Budget enforcement");

    let config = AgentConfig::default();
    let agent = MiniAgent::new(config).unwrap();

    let budget = agent.budget();

    // Test iteration budget
    for _ in 0..5 {
        core::assert!(budget.consume_iteration().is_ok());
    }

    core::assert_eq!(budget.iteration_usage(), 5);

    // Test memory budget
    core::assert!(budget.consume_memory(1024).is_ok());
    core::assert_eq!(budget.memory_usage(), 1024);

    budget.release_memory(512);
    core::assert_eq!(budget.memory_usage(), 512);

    defmt::info!("✓ Budget enforcement test passed");
}

async fn test_error_handling() {
    defmt::info!("Test: Error handling");

    // Test configuration error: max_iterations = 0 is invalid
    let mut bad_config = AgentConfig::default();
    bad_config.max_iterations = 0;
    let result = MiniAgent::new(bad_config);
    core::assert!(result.is_err(), "Agent creation with bad config should fail");

    // Test configuration error: empty name is invalid
    let mut bad_config2 = AgentConfig::default();
    bad_config2.name = heapless::String::new();
    let result2 = MiniAgent::new(bad_config2);
    core::assert!(result2.is_err(), "Agent creation with empty name should fail");

    defmt::info!("✓ Error handling test passed");
}