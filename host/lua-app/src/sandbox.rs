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

use mlua::{Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Value, VmState};

/// Maximum Lua heap in bytes. Generous for a business app but far below the
/// budget of a memory-rich chip (ESP32-S3 ~8 MB PSRAM), so a runaway
/// allocation is contained.
pub const MEMORY_LIMIT: usize = 512 * 1024;

/// Instruction budget per script before the interpreter is stopped.
pub const INSTRUCTION_BUDGET: u64 = 2_000_000;

/// Runtime-error text raised when the instruction budget is exhausted.
///
/// Shared by the hook and the `pcall`/`xpcall` wrapper so the wrapper can
/// recognise a *budget* error and re-raise it instead of letting the script
/// catch it and keep running.
const BUDGET_MESSAGE: &str = "script instruction budget exceeded (infinite loop?)";

/// Whether a Lua error value is the instruction-budget error.
fn is_budget_error(value: &Value) -> bool {
    match value {
        Value::String(s) => s
            .to_str()
            .map(|s| s.contains(BUDGET_MESSAGE))
            .unwrap_or(false),
        // mlua surfaces a Rust-side `Error` returned from a hook (or a
        // callback) to Lua as a wrapped-error userdata, which `pcall`
        // hands back as `Value::Error` — not a string.
        Value::Error(err) => err.to_string().contains(BUDGET_MESSAGE),
        _ => false,
    }
}

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
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        LuaOptions::default(),
    )?;

    // The Lua *base* library cannot be deselected through `StdLib`
    // (mlua's construction calls `luaopen_base` unconditionally), and
    // in Lua 5.4 the base library itself defines the three chunk
    // loaders:
    //
    //   * `dofile` / `loadfile` open and execute a file through C
    //     `fopen` — bypassing the absent `io` / `os` libraries
    //     entirely;
    //   * `load` defaults to accepting *binary* chunks, which
    //     `run_script`'s `ChunkMode::Text` restriction does not cover
    //     (it only applies to the outer chunk), re-opening the
    //     "malicious bytecode can crash the interpreter" path.
    //
    // Remove all three. Business scripts have no legitimate need to
    // load code at runtime, and every script entry point goes through
    // `run_script`.
    for name in ["load", "loadfile", "dofile"] {
        lua.globals().set(name, mlua::Value::Nil)?;
    }
    Ok(lua)
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
    //
    // The debit is *saturating* and the exhausted state is sticky: the
    // previous `fetch_sub` wrapped a zero counter to `u64::MAX`, so a script
    // that caught the first budget error with `pcall` (pcall can catch a hook
    // error like any other runtime error) disabled the budget and could loop
    // indefinitely. Once the counter reaches zero it now stays zero, and every
    // later hook invocation raises again.
    let instructions = Rc::new(AtomicU64::new(INSTRUCTION_BUDGET));
    let hook_budget = Rc::clone(&instructions);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1024),
        move |_lua, _dbg| {
            let previous = hook_budget
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(1024))
                })
                .unwrap_or(0);
            if previous <= 1024 {
                return Err(mlua::Error::RuntimeError(BUDGET_MESSAGE.into()));
            }
            Ok(VmState::Continue)
        },
    );

    // The hook error is an ordinary Lua runtime error, so `pcall` /
    // `xpcall` can catch it. A script that wraps its body in `pcall`
    // therefore survives every budget trip and keeps running (the sticky
    // counter above at least prevents the old wrap-around, but does not by
    // itself end the script). Wrap both protected-call functions: when the
    // caught error is the budget error, re-raise it so it propagates out of
    // the script instead of being swallowed.
    for name in ["pcall", "xpcall"] {
        let Ok(original) = lua.globals().get::<Function>(name) else {
            continue;
        };
        let wrapped = lua.create_function(move |_, args: MultiValue| {
            let result: MultiValue = original.call(args)?;
            let failed = matches!(result.front(), Some(Value::Boolean(false)));
            if failed && result.iter().skip(1).any(is_budget_error) {
                return Err(mlua::Error::RuntimeError(BUDGET_MESSAGE.into()));
            }
            Ok(result)
        })?;
        lua.globals().set(name, wrapped)?;
    }

    Ok(Budget { instructions })
}
