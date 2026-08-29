//! Host-only **mock LLM backend** so `agent.reason()` returns a canned action
//! and the full "agent decides → hardware acts" path can be tested / demoed on
//! a desktop without a network.
//!
//! The real firmware uses a DeepSeek HTTP backend; this crate provides the mock
//! so the host test suite can prove the decision → action → hardware pipeline
//! end to end.

use std::sync::{Arc, Mutex};

use magent_core::agent::LlmBackend;
use magent_core::error::AgentError;
use magent_core::MiniAgent;

use crate::agent::SharedAgent;
use crate::error::{LuaHostError, Result};

/// A [`LlmBackend`] that always returns one fixed answer.
pub struct MockLlmBackend {
    action: String,
}

impl MockLlmBackend {
    /// Build a mock that answers every request with `action`.
    pub fn new(action: &str) -> Self {
        Self {
            action: action.to_owned(),
        }
    }
}

impl LlmBackend for MockLlmBackend {
    fn complete(&mut self, _system: &str, _user: &str) -> core::result::Result<String, AgentError> {
        // Plain text (not JSON tool-call) → `MiniAgent` treats it as a final
        // answer, so `agent.reason` returns it verbatim.
        Ok(self.action.clone())
    }
}

/// Build a [`SharedAgent`] whose `agent.reason()` always returns `action`.
///
/// The backend is leaked to `'static` (as the firmware does for its DeepSeek
/// client) because `MiniAgent` holds it by `&'static mut`.
pub fn install_mock_agent(action: &str) -> Result<SharedAgent> {
    let backend: &'static mut dyn LlmBackend = Box::leak(Box::new(MockLlmBackend::new(action)));
    let mut agent = MiniAgent::with_defaults()
        .map_err(|e| LuaHostError::Agent(format!("mock agent init: {e}")))?;
    agent.set_llm_backend(backend);
    // TRACE: REQ-LUA-SANDBOX — single-threaded VM design (see examples/demo.rs).
    #[allow(clippy::arc_with_non_send_sync)]
    Ok(Arc::new(Mutex::new(agent)))
}
