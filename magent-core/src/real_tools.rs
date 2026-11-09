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
    /// Create a new simulator executor.
    ///
    /// The wrapped device has a week of nights already recorded, the way a
    /// device the user has been wearing would. [`AgentSimulator::new`]
    /// deliberately starts with no sleep history — a freshly-booted device has
    /// not been worn overnight, and `get_sleep_quality` reports that rather than
    /// inventing a night — but this executor is the CLI's tool backend, where
    /// the point is to exercise every tool the prompt advertises. With no
    /// history the advertised sleep tool can only ever answer with an error, so
    /// the harness seeds a plausible record here instead of making a fresh
    /// simulator claim it slept.
    pub fn new() -> Self {
        let mut simulator = AgentSimulator::new();
        // A week's worth of nights, improving slightly, so the trend and the
        // 7-night average `get_sleep_quality` reports are computed over real
        // records rather than a single night dressed up as a trend.
        for minutes in [430, 452, 468, 471, 489, 495, 502] {
            simulator.simulate_night(minutes);
        }
        Self { simulator }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `get_sleep_quality` is in the prompt the CLI hands the model, and this
    /// executor is the CLI's tool backend. If the backend's device has never
    /// slept, that advertised tool can only ever answer with an error — the
    /// prompt promises a capability the simulated run cannot demonstrate.
    #[test]
    fn the_cli_tool_backend_can_answer_the_advertised_sleep_tool() {
        let mut exec = SimulatorExecutor::new();
        let out = exec
            .execute("get_sleep_quality", "{}")
            .expect("the advertised sleep tool must not hard-fail on the CLI backend");
        assert!(out.contains("score"), "no score in {out:?}");
        assert!(out.contains("last 7 nights avg"), "no trend in {out:?}");
    }

    #[test]
    fn the_executor_reports_an_unknown_tool_as_an_err() {
        // The `ToolExecutor` contract is `Result<String, String>`; a tool the
        // simulator does not know must surface as `Err`, never an `Ok` with a
        // fabricated body.
        let mut exec = SimulatorExecutor::new();
        let err = exec
            .execute("no_such_tool", "{}")
            .expect_err("an unknown tool must be an error");
        assert!(!err.is_empty(), "the error string must describe the failure");
    }

    #[test]
    fn default_executor_and_accessors_are_usable() {
        let mut exec = SimulatorExecutor::default();
        // The mutable accessor lets a caller drive the simulator directly, and
        // `connect_ble` is a no-op success on the simulator.
        let _: &AgentSimulator = exec.simulator();
        let _: &mut AgentSimulator = exec.simulator_mut();
        exec.connect_ble();
    }
}
