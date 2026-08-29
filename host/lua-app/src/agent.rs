//! `agent.reason(context, prompt)` binding onto the embedded `MiniAgent`.
//!
//! `MiniAgent` is `&mut`-stateful and `async`. To expose it as a synchronous
//! Lua callback we park it behind a [`Mutex`] and drive each call with
//! [`block_on`]. The Lua script therefore never blocks the host event loop on
//! a real device if this binding runs on a dedicated worker thread.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use magent_core::MiniAgent;

use crate::error::{LuaHostError, Result};

/// A shared handle to a [`MiniAgent`] usable from Lua callbacks.
///
/// The mutex guards the `&mut`-stateful agent. A poisoned mutex is reported as
/// an error, never a panic.
pub type SharedAgent = Arc<Mutex<MiniAgent>>;

/// Run the embedded agent on `context` + `prompt` and return its answer.
///
/// The two strings are joined into a single task and capped to the agent's
/// own `MAX_BUFFER_SIZE`. The returned `heapless` answer is converted to an
/// owned [`String`].
pub fn reason(agent: &SharedAgent, context: &str, prompt: &str) -> Result<String> {
    let mut task = String::with_capacity(context.len() + prompt.len() + 1);
    task.push_str(context);
    if !context.is_empty() && !prompt.is_empty() {
        task.push(' ');
    }
    task.push_str(prompt);

    // `run` is async; we drive it synchronously. On a real device this must
    // run on the Lua worker thread, not an interrupt or the RTOS tick.
    let mut guard = agent
        .lock()
        .map_err(|_| LuaHostError::Agent("agent mutex poisoned".to_string()))?;
    let out = block_on(guard.run(&task)).map_err(|e| LuaHostError::Agent(e.to_string()))?;
    Ok(out.to_string())
}
