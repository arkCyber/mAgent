//! Engine-agnostic abstraction over the Lua VM backends (`mlua` and `piccolo`).
//!
//! [`AppRuntime`](crate::runtime::AppRuntime) is generic over [`LuaEngine`], so
//! the full runtime (watchdog / health / reload / graceful stop) works with
//! either backend. `mlua` is the default; `piccolo` (pure Rust, Xtensa-capable)
//! is the ESP32-S3 engine.

use crate::error::{LuaHostError, Result};

#[cfg(feature = "mlua")]
use crate::vm::LuaVm;

/// The minimal surface an event-loop runtime needs from a Lua VM.
pub trait LuaEngine {
    /// Short identifier for the engine, e.g. `"mlua"` or `"piccolo"`.
    fn engine_name(&self) -> &'static str;

    /// Execute a Lua chunk. Errors are returned, never a panic.
    fn run_script(&mut self, script: &str) -> Result<()>;
    /// Whether a named global callable (function) is defined.
    fn has(&mut self, fname: &str) -> bool;
    /// Invoke a named global function with string args; return its string.
    fn call(&mut self, fname: &str, args: &[String]) -> Result<String>;
    /// Rebuild the interpreter from a clean slate (for hot-reload).
    fn reload_state(&mut self) -> Result<()>;
}

/// Capture an engine's identity for diagnostics (test names, log lines, etc.).
/// Convenience wrapper over [`LuaEngine::engine_name`] so callers don't need to
/// import the trait just to print the engine label.
pub fn engine_name<E: LuaEngine>(engine: &E) -> &'static str {
    engine.engine_name()
}

/// Assert that `actual` equals `expected`, using `engine` for a clear error
/// message when they differ.  When they differ, the assertion shows both
/// values labelled by engine name so it's immediately obvious which engine
/// deviated.
///
/// ```ignore
/// assert_engine_output(&mut engine, "on_tick", &["0"], "ok")?;
/// ```
pub fn assert_engine_output<E: LuaEngine>(
    engine: &mut E,
    fname: &str,
    args: &[&str],
    expected: &str,
) -> Result<()> {
    let owned_args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let actual = engine.call(fname, &owned_args)?;
    if actual != expected {
        return Err(LuaHostError::Lua(format!(
            "[{}] {}{:?} produced \"{actual}\" but expected \"{expected}\"",
            engine.engine_name(),
            fname,
            args,
        )));
    }
    Ok(())
}

#[cfg(feature = "mlua")]
impl LuaEngine for LuaVm {
    fn engine_name(&self) -> &'static str { "mlua" }
    fn run_script(&mut self, script: &str) -> Result<()> {
        LuaVm::run_script(self, script)
    }
    fn has(&mut self, fname: &str) -> bool {
        LuaVm::has(self, fname)
    }
    fn call(&mut self, fname: &str, args: &[String]) -> Result<String> {
        LuaVm::call(self, fname, args)
    }
    fn reload_state(&mut self) -> Result<()> {
        LuaVm::reload_state(self)
    }
}
