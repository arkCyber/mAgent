//! A `piccolo`-backed Lua VM exposing a `LuaVm`-compatible surface, so the
//! ESP32-S3 (which cannot build `mlua`'s vendored Lua C) can run the Lua app.
//! `piccolo` is pure Rust — no C — so it compiles for any target.
//!
//! Built only when the `piccolo` feature is on (and `mlua` default is off):
//! `cargo test -p magent-lua --features piccolo --no-default-features`

//! ## Known differences from `mlua` / standard Lua 5.4
//!
//! `piccolo` 0.3.x ships a deliberately minimal stdlib. The following are
//! **not available** and must not be used in scripts that target this engine:
//!
//! | Category | Missing API |
//! |----------|------------|
//! | `string` | `find`, `gmatch`, `gsub`, `rep`, `reverse` |
//! | `table`  | `move`, `unpack` |
//! | `base`   | `collectgarbage`, `error` (no error propagation), `select` |
//! | `math`   | `atan2`, `cosh`, `sinh`, `tanh`, `log`, `log10`, `exp`, `pow` |
//! | `os`     | entirely absent |
//! | `io`     | entirely absent |
//!
//! The following are **shimmed** in this file to provide cross-engine compatibility:
//! - `tonumber(s [, base])` — global, handles decimal and radix 2–36
//! - `string.format(fmt, ...)` — handles `%s`, `%d`, `%i`, `%f`, `%.<n>f`, `%%`
//! - `string.match(s, pat [, init])` — supports `%d`, `%w`, `%s`, `%a`, and uppercase negations, quantifiers `+` `*` `?`, `(...)` captures, and anchors `^` `$`
//!
//! ### varargs in Rust callbacks (known piccolo 0.3.x limitation)
//! `stack.from_front::<Value>()` silently drops values when called from within
//! a Lua function execution context. All varargs must be read via `stack.get(i)`
//! (peek, no pop), then explicitly drained with `stack.pop_front()`.

#![cfg(feature = "piccolo")]

use piccolo::{
    Callback, CallbackReturn, Closure, Error, Executor, FromMultiValue, Fuel, Function,
    FunctionPrototype, Lua, RuntimeError, StashedExecutor, String as LuaString, Table, Value,
    Variadic,
};

use crate::agent::{reason as agent_reason, SharedAgent};
use crate::engine::LuaEngine;
use crate::error::{LuaHostError, Result};
use crate::hardware::SharedHardware;
use crate::nvram;

/// Read one Lua string argument off the stack and decode it as UTF-8.
fn pop_str<'gc>(ctx: piccolo::Context<'gc>, stack: &mut piccolo::Stack<'gc, '_>) -> Result<String> {
    let s: piccolo::String<'gc> = stack
        .from_front(ctx)
        .map_err(|_| LuaHostError::Lua("expected string argument".into()))?;
    // Copy the bytes out immediately so no arena borrow escapes this frame.
    let bytes: Vec<u8> = s.as_bytes().to_vec();
    String::from_utf8(bytes).map_err(|e| LuaHostError::Lua(format!("bad utf8: {e}")))
}

/// Read one Lua integer argument off the stack.
fn pop_i64<'gc>(ctx: piccolo::Context<'gc>, stack: &mut piccolo::Stack<'gc, '_>) -> Result<i64> {
    stack
        .from_front(ctx)
        .map_err(|_| LuaHostError::Lua("expected integer argument".into()))
}

/// A `piccolo`-backed sandboxed Lua VM.
pub struct PiccoloVm {
    lua: Lua,
    /// Seeds kept so the VM can be rebuilt from a clean slate on hot-reload.
    hardware: SharedHardware,
    agent: SharedAgent,
}

impl PiccoloVm {
    /// Register `hardware.*` + `agent.reason` into a fresh `piccolo` core VM.
    fn build_lua(hw: SharedHardware, ag: SharedAgent) -> Lua {
        // `Lua::full()` loads the core stdlib + the io stdlib (`print`), so
        // scripts can log to the console (the S3 self-test relies on it).
        let mut lua = Lua::full();

        lua.enter(|ctx| {
            // ---- hardware table -------------------------------------------------
            let hardware_table = Table::new(&ctx);

            let hw_sensor = hw.clone();
            let sensor_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let name = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_sensor.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let v = backend
                    .sensor_read(&name)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, v); // f64 → Lua number
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "sensor_read", sensor_read);

            let hw_gpio = hw.clone();
            let gpio_write = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let pin: i64 = stack
                    .from_front(ctx)
                    .map_err(|_| to_lua_error(LuaHostError::Lua("expected int pin".into())))?;
                let level: i64 = stack
                    .from_front(ctx)
                    .map_err(|_| to_lua_error(LuaHostError::Lua("expected int level".into())))?;
                let mut backend = hw_gpio.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .gpio_write(pin as u8, level as u8)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "gpio_write", gpio_write);

            let hw_gpio_read = hw.clone();
            let gpio_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let pin = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_gpio_read.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let v = backend
                    .gpio_read(pin as u8)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, v as i64);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "gpio_read", gpio_read);

            let hw_i2c_read = hw.clone();
            let i2c_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let reg = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let len = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_i2c_read.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let bytes = backend
                    .i2c_read(addr as u8, reg as u8, len as usize)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                let s = LuaString::from_slice(&ctx, &bytes);
                stack.replace(ctx, s);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "i2c_read", i2c_read);

            let hw_i2c_write = hw.clone();
            let i2c_write = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let reg = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let data = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_i2c_write.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .i2c_write(addr as u8, reg as u8, data.as_bytes())
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "i2c_write", i2c_write);

            let hw_adc = hw.clone();
            let adc_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let pin = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_adc.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let v = backend
                    .adc_read(pin as u8)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, v);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "adc_read", adc_read);

            let hw_pwm = hw.clone();
            let pwm_set = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let pin = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let duty = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_pwm.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .pwm_set(pin as u8, duty as u8)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "pwm_set", pwm_set);

            let hw_flash = hw.clone();
            let flash_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let len = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_flash.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let bytes = backend
                    .flash_read(addr as u32, len as usize)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                let s = LuaString::from_slice(&ctx, &bytes);
                stack.replace(ctx, s);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "flash_read", flash_read);

            let hw_flash = hw.clone();
            let flash_write = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let data = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_flash.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .flash_write(addr as u32, data.as_bytes())
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "flash_write", flash_write);

            let hw_flash = hw.clone();
            let flash_erase = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_flash.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .flash_erase_sector(addr as u32)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "flash_erase_sector", flash_erase);

            let hw_nvram = hw.clone();
            let nvram_get = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let key = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_nvram.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                match nvram::get(&mut *backend, &key)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?
                {
                    Some(v) => {
                        let s = LuaString::from_slice(&ctx, v.as_bytes());
                        stack.replace(ctx, s);
                    }
                    None => stack.replace(ctx, ()),
                }
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "nvram_get", nvram_get);

            let hw_nvram = hw.clone();
            let nvram_set = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let key = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let value = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_nvram.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                nvram::set(&mut *backend, &key, &value)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "nvram_set", nvram_set);

            let hw_ble = hw.clone();
            let ble_send = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let data = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_ble.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .ble_send(data.as_bytes())
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "ble_send", ble_send);

            let hw_power = hw.clone();
            let power_set = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let profile = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_power.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                backend
                    .power_set(profile as u8)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "power_set", power_set);

            let _ = ctx.set_global("hardware", hardware_table);

            // ---- agent table ----------------------------------------------------
            let agent_table = Table::new(&ctx);
            let ag_reason = ag.clone();
            let reason = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let context = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let prompt = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let out = agent_reason(&ag_reason, &context, &prompt).map_err(to_lua_error)?;
                let s = LuaString::from_slice(&ctx, out.as_bytes());
                stack.replace(ctx, s);
                Ok(CallbackReturn::Return)
            });
            let _ = agent_table.set(ctx, "reason", reason);
            let _ = ctx.set_global("agent", agent_table);

            // ---- portability shims ----------------------------------------------
            // piccolo 0.3.3's stdlib only ships `string.len` / `string.sub` and a
            // bare `base` library (no `tonumber`, no `string.format`,
            // no `string.match`). The enterprise Lua apps we host (`greenhouse`,
            // `datalogger`, `alarm`) need all four, so we register them here as
            // extra global bindings so the *same* script runs on either engine.
            let _ = ctx.set_global(
                "tonumber",
                Callback::from_fn(&ctx, |ctx, _, mut stack| {
                    let s: Option<piccolo::String> = stack.from_front(ctx).ok();
                    let radix: Option<i64> = stack.from_front(ctx).ok();
                    match (s, radix) {
                        (Some(s), r) => {
                            let bytes: Vec<u8> = s.as_bytes().to_vec();
                            let parsed = match r {
                                Some(base) if (2..=36).contains(&base) => {
                                    let s = std::str::from_utf8(&bytes).map_err(|e| {
                                        to_lua_error(LuaHostError::Lua(format!("bad utf8: {e}")))
                                    })?;
                                    i64::from_str_radix(s, base as u32).ok().map(|n| n as f64)
                                }
                                _ => std::str::from_utf8(&bytes)
                                    .ok()
                                    .and_then(|s| s.parse::<f64>().ok()),
                            };
                            match parsed {
                                Some(n) if n.fract() == 0.0 && n.is_finite() => {
                                    stack.replace(ctx, n as i64);
                                }
                                Some(n) => {
                                    stack.replace(ctx, n);
                                }
                                None => stack.replace(ctx, ()),
                            }
                            Ok(CallbackReturn::Return)
                        }
                        (None, _) => {
                            stack.replace(ctx, ());
                            Ok(CallbackReturn::Return)
                        }
                    }
                }),
            );

            // Build a `string.format` shim that supports the subset our apps
            // use: `%s`, `%d`, `%.<n>f`, `%%`. Anything else passes through
            // as-is (no panic).
            let string_table: Table = match ctx.globals().get(ctx, "string") {
                piccolo::Value::Table(t) => t,
                _ => Table::new(&ctx),
            };
            let _ = string_table.set(
                ctx,
                "format",
                Callback::from_fn(&ctx, |ctx, _, mut stack| {
                    let fmt: piccolo::String = stack.from_front(ctx).map_err(|_| {
                        to_lua_error(LuaHostError::Lua("string.format: expected fmt".into()))
                    })?;
                    let fmt_bytes: Vec<u8> = fmt.as_bytes().to_vec();
                    let fmt_str = std::str::from_utf8(&fmt_bytes)
                        .map_err(|e| to_lua_error(LuaHostError::Lua(format!("bad utf8: {e}"))))?
                        .to_owned();

                    // Collect varargs. We must peek them first (stack.get, doesn't pop)
                    // because stack.from_front() silently drops values when called
                    // from within a Lua function context (re-entrancy issue in piccolo
                    // 0.3.x). We record the count, peek all args, then drain the real
                    // stack to restore it.
                    let n = stack.len();
                    let mut args: Vec<String> = Vec::with_capacity(n);
                    for i in 0..n {
                        let v = stack.get(i);
                        args.push(v_to_owned_string(v));
                    }
                    // Drain the real stack so piccolo doesn't see a growing frame.
                    for _ in 0..n {
                        stack.pop_front();
                    }

                    let out = do_string_format(&fmt_str, &args);
                    stack.replace(ctx, LuaString::from_slice(&ctx, out.as_bytes()));
                    Ok(CallbackReturn::Return)
                }),
            );
            let _ = string_table.set(
                ctx,
                "match",
                Callback::from_fn(&ctx, |ctx, _, mut stack| {
                    let s: piccolo::String = stack.from_front(ctx).map_err(|_| {
                        to_lua_error(LuaHostError::Lua("string.match: expected string".into()))
                    })?;
                    let s_bytes: Vec<u8> = s.as_bytes().to_vec();
                    let s_str = std::str::from_utf8(&s_bytes)
                        .map_err(|e| to_lua_error(LuaHostError::Lua(format!("bad utf8: {e}"))))?
                        .to_owned();
                    let pat: piccolo::String = stack.from_front(ctx).map_err(|_| {
                        to_lua_error(LuaHostError::Lua("string.match: expected pattern".into()))
                    })?;
                    let p_bytes: Vec<u8> = pat.as_bytes().to_vec();
                    let pat_str = std::str::from_utf8(&p_bytes)
                        .map_err(|e| to_lua_error(LuaHostError::Lua(format!("bad utf8: {e}"))))?
                        .to_owned();
                    let init: Option<i64> = stack.from_front(ctx).ok();
                    let captures = do_string_match(&s_str, &pat_str, init.unwrap_or(1));
                    match captures {
                        Some(v) => {
                            let s = LuaString::from_slice(&ctx, v.as_bytes());
                            stack.replace(ctx, s);
                        }
                        None => stack.replace(ctx, ()),
                    }
                    Ok(CallbackReturn::Return)
                }),
            );
            let _ = ctx.set_global("string", string_table);
        });

        lua
    }

    /// Build the VM, register `hardware.*` + `agent.reason`, and load the core
    /// stdlib (base/math/string/table/coroutine — no `io`/`os`).
    pub fn new(hardware: SharedHardware, agent: SharedAgent) -> Self {
        let lua = Self::build_lua(hardware.clone(), agent.clone());
        Self {
            lua,
            hardware,
            agent,
        }
    }

    /// Rebuild the interpreter from a clean slate (fresh globals, re-registered
    /// bindings) — used by `AppRuntime::reload` for script hot-reload.
    pub fn reload_state(&mut self) -> Result<()> {
        self.lua = Self::build_lua(self.hardware.clone(), self.agent.clone());
        Ok(())
    }

    /// Run a Lua chunk. Errors are returned, never a panic.
    pub fn run_script(&mut self, script: &str) -> Result<()> {
        self.lua
            .enter(|ctx| {
                let proto = FunctionPrototype::compile(ctx, "<piccolo>", script.as_bytes())
                    .map_err(|e| LuaHostError::Lua(e.to_string()))?;
                let closure = Closure::new(&ctx, proto, Some(ctx.globals()))
                    .map_err(|e| LuaHostError::Lua(e.to_string()))?;
                let executor =
                    Executor::start(ctx, Function::from(closure), Variadic(Vec::<Value>::new()));
                Ok(ctx.stash(executor))
            })
            .and_then(|stashed| execute_bounded::<()>(&mut self.lua, &stashed))?;
        Ok(())
    }

    /// Whether a named global callable (function) is currently defined.
    pub fn has(&mut self, fname: &str) -> bool {
        self.lua.enter(|ctx| {
            let key = piccolo::String::from_slice(&ctx, fname.as_bytes());
            matches!(ctx.get_global(key), Value::Function(_))
        })
    }

    /// Invoke a named global Lua function with string args and return its
    /// string result — the event-loop entry point (`on_tick`, `on_event`).
    pub fn call(&mut self, fname: &str, args: &[String]) -> Result<String> {
        let stashed = self.lua.enter(|ctx| {
            let key = piccolo::String::from_slice(&ctx, fname.as_bytes());
            match ctx.get_global(key) {
                Value::Function(func) => {
                    let varargs = Variadic(
                        args.iter()
                            .map(|s| Value::String(LuaString::from_slice(&ctx, s.as_bytes())))
                            .collect::<Vec<Value>>(),
                    );
                    let executor = Executor::start(ctx, func, varargs);
                    Some(ctx.stash(executor))
                }
                _ => None,
            }
        });
        let stashed = stashed
            .ok_or_else(|| LuaHostError::Lua(format!("{fname} is not a callable global")))?;
        execute_bounded::<String>(&mut self.lua, &stashed)
    }
}

/// Convert a crate error into a `piccolo::Error` runtime error.
///
/// `LuaHostError` is `std::error::Error + Send + Sync`, so it satisfies
/// `RuntimeError: From<E: Into<anyhow::Error>>`.
fn to_lua_error<'gc>(e: LuaHostError) -> Error<'gc> {
    Error::Runtime(RuntimeError::from(e))
}

/// Drive a stashed executor to completion under a **total fuel (instruction)
/// budget**, so an infinite loop cannot hang the device.
///
/// `Lua::execute` runs to completion with no budget (it refills fuel each GC
/// step), which means a hostile or buggy `main.lua` — `while true do end` — would
/// wedge the `lua-thread` forever on the S3. Here we instead call
/// `Executor::step` with a bounded cumulative `Fuel` and error out once the
/// budget is exhausted, mirroring the mlua sandbox's instruction budget. Fuel is
/// refilled per iteration (as in `Lua::finish`) so the GC still runs between
/// steps, while `MAX_FUEL` caps the *total* work any single `run_script`/`call`
/// may do.
fn execute_bounded<R>(lua: &mut Lua, stashed: &StashedExecutor) -> Result<R>
where
    R: for<'gc> FromMultiValue<'gc>,
{
    const FUEL_PER_ITER: i32 = 4096; // per-step fuel (matches `Lua::finish`)
    const MAX_FUEL: i64 = 20_000_000; // total op budget (~tens of millions of ops)
                                      // Bound the piccolo VM's total allocation (gc-arena bytes). Generous enough
                                      // for the S3's 8 MB PSRAM, but a runaway allocation is still contained.
    const MAX_MEMORY: usize = 8 * 1024 * 1024;

    let mut budget: i64 = MAX_FUEL;
    loop {
        let mut fuel = Fuel::with(FUEL_PER_ITER);
        let done = lua.enter(|ctx| ctx.fetch(stashed).step(ctx, &mut fuel));
        budget -= i64::from(FUEL_PER_ITER - fuel.remaining());
        if lua.total_memory() > MAX_MEMORY {
            return Err(LuaHostError::Lua("script memory limit exceeded".into()));
        }
        if done {
            let result = lua.try_enter(|ctx| match ctx.fetch(stashed).take_result::<R>(ctx) {
                Ok(Ok(r)) => Ok(r),
                Ok(Err(e)) => Err(e),
                Err(_mode) => Err(to_lua_error(LuaHostError::Lua("bad executor mode".into()))),
            });
            return result.map_err(|e| LuaHostError::Lua(e.to_string()));
        }
        if budget <= 0 {
            return Err(LuaHostError::Lua(
                "script instruction budget exceeded (infinite loop?)".into(),
            ));
        }
    }
}

impl LuaEngine for PiccoloVm {
    fn engine_name(&self) -> &'static str { "piccolo" }
    fn run_script(&mut self, script: &str) -> Result<()> {
        self.run_script(script)
    }
    fn has(&mut self, fname: &str) -> bool {
        self.has(fname)
    }
    fn call(&mut self, fname: &str, args: &[String]) -> Result<String> {
        self.call(fname, args)
    }
    fn reload_state(&mut self) -> Result<()> {
        self.reload_state()
    }
}

// ---------------------------------------------------------------------------
// piccolo 0.3.3 portability shims
// ---------------------------------------------------------------------------
//
// piccolo's `Lua::full()` does NOT load `string.format`, `string.match`, or
// `tonumber` (its stdlib is deliberately minimal: `string` only ships `len`
// and `sub`, and `base` lacks `tonumber`). The enterprise Lua apps
// (`greenhouse`, `datalogger`, `alarm`) use the standard Lua string/number
// API, so `PiccoloVm` registers the missing pieces as Rust-backed callbacks.
//
// Supported:
//   * `tonumber(s [, base])`     → integer | float | nil
//   * `string.format(fmt, ...)`  → handles `%s`, `%d`, `%f`, `%.<n>f`, `%%`
//   * `string.match(s, pat [, init])` → first capture group (or whole match)
//
// Anything more exotic returns the literal input or `nil` rather than
// panicking — the calling Lua script's defensive parsing handles the rest.

/// Tiny Lua-`string.format`-compatible formatter. Handles the subset our apps
/// need: `%s`, `%d`, `%f`, `%.<n>f`, and `%%`. Unknown conversions are passed
/// through verbatim — never a panic.
fn do_string_format(fmt: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut args = args.iter();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            // Trailing `%` at end of format string is output literally.
            out.push('%');
            break;
        };
        match spec {
            '%' => out.push('%'),
            's' => {
                if let Some(a) = args.next() {
                    out.push_str(a);
                }
            }
            'd' | 'i' => {
                if let Some(a) = args.next() {
                    let n = a.parse::<f64>().unwrap_or(0.0);
                    out.push_str(&format!("{}", n as i64));
                }
            }
            'f' => {
                if let Some(a) = args.next() {
                    let n = a.parse::<f64>().unwrap_or(0.0);
                    out.push_str(&format!("{n}"));
                }
            }
            '.' => {
                let mut prec_digits = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        prec_digits.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let Some(conv) = chars.next() else {
                    // `%.` at end-of-string: emit literally and continue
                    // (any trailing chars after this incomplete specifier
                    // are still processed).
                    out.push('%');
                    out.push('.');
                    out.push_str(&prec_digits);
                    continue;
                };
                let prec: usize = prec_digits.parse().unwrap_or(0);
                if conv == 'f' {
                    if let Some(a) = args.next() {
                        let n = a.parse::<f64>().unwrap_or(0.0);
                        out.push_str(&format!("{n:.*}", prec));
                    }
                } else {
                    out.push('%');
                    out.push('.');
                    out.push_str(&prec_digits);
                    out.push(conv);
                }
            }
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

/// Convert any piccolo `Value` to its owned `String` representation.
/// This is a free function (no lifetimes) so it can be called inside loops
/// that consume values from the piccolo stack.
fn v_to_owned_string(v: piccolo::Value<'_>) -> String {
    match v {
        piccolo::Value::String(s) => std::str::from_utf8(s.as_bytes())
            .map(|s| s.to_owned())
            .unwrap_or_default(),
        piccolo::Value::Integer(i) => i.to_string(),
        piccolo::Value::Number(n) => n.to_string(),
        piccolo::Value::Boolean(b) => b.to_string(),
        piccolo::Value::Nil => "nil".to_string(),
        _ => {
            // For other types, try `tostring` meta-op; fall back to type name.
            // We can't call meta-ops from a no-'gc fn, so just return the type.
            format!("{:?}", v)
        }
    }
}

/// Tiny Lua-`string.match` shim. Supports the patterns our apps actually use
/// (`%d`/`%w`/`%s`/`%a` / `%D`/`%W`/`%S`/`%A` classes, `(...)` captures,
/// `+`/`*`/`?` repetition, `.` wildcard, anchored `^`/`$`).
///
/// **Returns the first capture group** when one or more `(...)` appear in the
/// pattern; otherwise the whole match. Multi-capture patterns are accepted
/// but only the first group's contents are surfaced (consumers that need
/// every capture should split the input with `string.find` instead).
/// Unsupported patterns return `None`.
fn do_string_match(s: &str, pat: &str, init: i64) -> Option<String> {
    let start = (init.max(1) as usize).saturating_sub(1).min(s.len());
    let s_tail = &s[start..];

    // Tokens are emitted for the entire pattern; `(...)` is encoded as a
    // single instruction with `capture = true` whose inner program is the
    // nested bytecode.
    #[derive(Clone, Copy)]
    enum Class {
        Lit(u8),
        Any,
        In(u8),
        NotIn(u8),
    }
    #[derive(Clone, Copy)]
    enum Quant {
        Once,
        Star,
        Plus,
        Opt,
    }
    #[derive(Clone)]
    struct Inst {
        tok: Class,
        quant: Quant,
        capture: bool,
        inner: Vec<Inst>,
    }

    let pb = pat.as_bytes();
    let mut code: Vec<Inst> = Vec::new();
    let mut i = 0;

    /// Parse one atomic (class + optional quantifier), advancing `i`.
    fn parse_atomic(pb: &[u8], i: &mut usize) -> Option<(Class, Quant)> {
        if *i >= pb.len() {
            return None;
        }
        let c = pb[*i];
        let tok = match c {
            b'%' => {
                *i += 1;
                if *i >= pb.len() {
                    return None;
                }
                let cls = pb[*i];
                match cls {
                    b'd' => Class::In(b'd'),
                    b'D' => Class::NotIn(b'd'),
                    b'w' => Class::In(b'w'),
                    b'W' => Class::NotIn(b'w'),
                    b's' => Class::In(b's'),
                    b'S' => Class::NotIn(b's'),
                    b'a' => Class::In(b'a'),
                    b'A' => Class::NotIn(b'a'),
                    other => Class::Lit(other),
                }
            }
            b'.' => Class::Any,
            other => Class::Lit(other),
        };
        *i += 1;
        let mut quant = Quant::Once;
        if *i < pb.len() {
            match pb[*i] {
                b'*' => {
                    *i += 1;
                    quant = Quant::Star;
                }
                b'+' => {
                    *i += 1;
                    quant = Quant::Plus;
                }
                b'?' => {
                    *i += 1;
                    quant = Quant::Opt;
                }
                _ => {}
            }
        }
        Some((tok, quant))
    }

    while i < pb.len() {
        let c = pb[i];
        // Anchors: `^` only at the very start, `$` only at the very end.
        if c == b'^' && i == 0 {
            i += 1;
            continue;
        }
        if c == b'$' && i + 1 == pb.len() {
            i += 1;
            continue;
        }
        if c == b'(' {
            // Parse a capture group until the matching `)`.
            i += 1;
            let mut depth = 1;
            let group_start = i;
            while i < pb.len() && depth > 0 {
                match pb[i] {
                    b'(' => {
                        depth += 1;
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            // Re-tokenize the group body so we can attach it as `inner`.
            let body = &pb[group_start..i.saturating_sub(1)];
            let mut inner: Vec<Inst> = Vec::new();
            let mut j = 0;
            while j < body.len() {
                if let Some((tok, quant)) = parse_atomic(body, &mut j) {
                    inner.push(Inst {
                        tok,
                        quant,
                        capture: false,
                        inner: Vec::new(),
                    });
                } else {
                    break;
                }
            }
            // Inline the inner program; mark the *first* inner instruction
            // so the matcher records the capture span when it runs.
            let inner_start = code.len();
            let inner_len = inner.len();
            code.extend(inner);
            if inner_len > 0 {
                code[inner_start].capture = true;
            }
            continue;
        }
        if let Some((tok, quant)) = parse_atomic(pb, &mut i) {
            code.push(Inst {
                tok,
                quant,
                capture: false,
                inner: Vec::new(),
            });
        } else {
            break;
        }
    }

    fn matches_class(c: &Class, b: u8) -> bool {
        match c {
            Class::Lit(l) => *l == b,
            Class::Any => true,
            Class::In(c) => match c {
                b'd' => b.is_ascii_digit(),
                b'w' => b.is_ascii_alphanumeric() || b == b'_',
                b's' => b.is_ascii_whitespace(),
                b'a' => b.is_ascii_alphabetic(),
                _ => false,
            },
            Class::NotIn(c) => !matches_class(&Class::In(*c), b),
        }
    }

    // Match a sub-program greedily from `pos`; returns (final_pos,
    // first_capture_span). An instruction marked `capture = true` records
    // the span it consumed (the bytes from before it runs to where it
    // leaves `p`).
    fn match_sub(code: &[Inst], s: &[u8], pos: usize) -> Option<(usize, Option<(usize, usize)>)> {
        let mut p = pos;
        let mut capture: Option<(usize, usize)> = None;
        for inst in code {
            let inst_start = p;
            match inst.quant {
                Quant::Once => {
                    if p >= s.len() || !matches_class(&inst.tok, s[p]) {
                        return None;
                    }
                    p += 1;
                }
                Quant::Opt => {
                    if p < s.len() && matches_class(&inst.tok, s[p]) {
                        p += 1;
                    }
                }
                Quant::Plus => {
                    if p >= s.len() || !matches_class(&inst.tok, s[p]) {
                        return None;
                    }
                    p += 1;
                    while p < s.len() && matches_class(&inst.tok, s[p]) {
                        p += 1;
                    }
                }
                Quant::Star => {
                    while p < s.len() && matches_class(&inst.tok, s[p]) {
                        p += 1;
                    }
                }
            }
            // Capture groups: the span is `inst_start .. p` (the bytes
            // this instruction consumed).
            if inst.capture && p > inst_start {
                capture = Some((inst_start, p));
            }
        }
        Some((p, capture))
    }

    // Try starting the match at every position.
    let sb = s_tail.as_bytes();
    for pos in 0..=sb.len() {
        if let Some((end, cap)) = match_sub(&code, sb, pos) {
            if let Some((gs, ge)) = cap {
                if gs < ge && ge <= sb.len() {
                    return Some(
                        std::str::from_utf8(&sb[gs..ge])
                            .map(|s| s.to_owned())
                            .unwrap_or_default(),
                    );
                }
            }
            let len = end.min(sb.len());
            if len >= pos {
                return Some(
                    std::str::from_utf8(&sb[pos..len])
                        .map(|s| s.to_owned())
                        .unwrap_or_default(),
                );
            }
        }
    }
    None
}
