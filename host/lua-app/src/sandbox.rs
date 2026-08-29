//! Lua sandboxing: restrict stdlibs, cap heap, cap instruction count.
//!
//! Aerospace-grade policy: an untrusted (or merely buggy) enterprise script
//! must not be able to read/write the filesystem, spawn processes, load
//! arbitrary modules, or hang the device. We enforce that three ways:
//!
//! 1. **Stdlib allow-list** — the VM is constructed with only `TABLE`,
//!    `STRING`, and `MATH`. (The Lua *base* lib — `print`, `type`, `pcall`,
//!    `pairs`, … — is always present in mlua.) `os`, `io`, `debug`, `package`,
//!    and `ffi` simply do not exist in the environment.
//! 2. **Memory cap** — `set_memory_limit` bounds the Lua heap.
//! 3. **Instruction budget** — an instruction hook stops any loop after a
//!    fixed number of instructions, so `while true do end` cannot wedge the
//!    host.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, VmState};

/// Maximum Lua heap in bytes. Generous for a business app but far below the
/// budget of a memory-rich chip (ESP32-S3 ~8 MB PSRAM), so a runaway
/// allocation is contained.
pub const MEMORY_LIMIT: usize = 512 * 1024;

/// Instruction budget per script before the interpreter is stopped.
pub const INSTRUCTION_BUDGET: u64 = 2_000_000;

/// Resettable per-script instruction budget handle.
///
/// The VM calls [`Budget::reset`] before each script so a long-lived VM does
/// not exhaust a *cumulative* budget across many scripts (which would
/// eventually make every script fail).
#[derive(Clone)]
pub struct Budget {
    instructions: Rc<AtomicU64>,
}

impl Budget {
    /// Reset the instruction counter to the full per-script budget.
    pub fn reset(&self) {
        self.instructions
            .store(INSTRUCTION_BUDGET, Ordering::Relaxed);
    }
}

/// Build a sandboxed [`Lua`] interpreter exposing only safe stdlibs.
///
/// Deliberately excludes `os`, `io`, `debug`, `package`, and `ffi`. This is
/// the single construction site for every VM in the crate, so the sandbox
/// cannot be bypassed accidentally.
pub fn new_sandbox() -> mlua::Result<Lua> {
    Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        LuaOptions::default(),
    )
}

/// Enforce the heap and instruction budgets on `lua`, returning the resettable
/// instruction [`Budget`] handle.
///
/// Any limit violation is surfaced as a [`crate::error::LuaHostError::Lua`]
/// runtime error (i.e. a `Result`), never a host panic.
pub fn enforce_budget(lua: &Lua) -> crate::Result<Budget> {
    // `set_memory_limit` returns the previous limit; we only care that the
    // new limit was applied, so discard it.
    let _ = lua
        .set_memory_limit(MEMORY_LIMIT)
        .map_err(crate::error::LuaHostError::from)?;

    // Instruction hook: every 1024 ops, debit the budget. When exhausted the
    // hook returns `Err`, which stops the script with a runtime error.
    let instructions = Rc::new(AtomicU64::new(INSTRUCTION_BUDGET));
    let hook_budget = Rc::clone(&instructions);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1024),
        move |_lua, _dbg| {
            let remaining = hook_budget.fetch_sub(1024, Ordering::Relaxed);
            if remaining <= 1024 {
                return Err(mlua::Error::RuntimeError(
                    "script instruction budget exceeded (infinite loop?)".into(),
                ));
            }
            Ok(VmState::Continue)
        },
    );

    Ok(Budget { instructions })
}
