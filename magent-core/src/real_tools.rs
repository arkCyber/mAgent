//! Real tool executor for testing (std only)
//!
//! Uses the simulated hardware from simulator.rs to provide
//! real tool execution for tests and the simulator build.

#![cfg(feature = "std")]

use crate::agent_runner::ToolExecutor;
use crate::error::Result;
use crate::simulator::AgentSimulator;
use std::string::String;

/// Test executor that uses the full simulator
pub struct SimulatorExecutor {
    simulator: AgentSimulator,
}

impl SimulatorExecutor {
    /// Create a new simulator executor
    pub fn new() -> Self {
        Self {
            simulator: AgentSimulator::new(),
        }
    }

    /// Get mutable reference to simulator
    pub fn simulator_mut(&mut self) -> &mut AgentSimulator {
        &mut self.simulator
    }

    /// Get read-only reference to simulator
    pub fn simulator(&self) -> &AgentSimulator {
        &self.simulator
    }

    /// Connect BLE for the simulator
    pub fn connect_ble(&mut self) {
        let _: Result<()> = self.simulator.ble.connect();
    }
}

impl Default for SimulatorExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor for SimulatorExecutor {
    fn execute(&mut self, tool: &str, args: &str) -> std::result::Result<String, String> {
        self.simulator
            .execute_tool(tool, args)
            .map_err(|e| format!("{:?}", e))
    }
}
