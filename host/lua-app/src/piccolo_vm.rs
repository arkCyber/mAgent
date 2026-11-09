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
    Callback, CallbackReturn, Closure, Error, Executor, Fuel, Function,
    FunctionPrototype, Lua, RuntimeError, StashedExecutor, String as LuaString, Table, Value,
    Variadic,
};

use crate::agent::{reason as agent_reason, SharedAgent};
use crate::engine::LuaEngine;
use crate::error::{LuaHostError, Result};
use crate::hardware::{
    SharedHardware, MAX_FLASH_READ_LEN, MAX_I2C_READ_LEN, MAX_I2C_WRITE_LEN,
    MAX_PAYLOAD_LEN, MAX_REASON_LEN,
};
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

/// Read a `hardware.*` read length and bound it to `max`.
///
/// The value is script-supplied, and the backend allocates a buffer of that
/// size, so an unchecked value is a host abort: a negative Lua integer wraps
/// to a huge `usize` through the `as usize` cast the caller would apply, and a
/// large positive one is just as fatal. Reject both here, at the boundary,
/// where the value is still known to be untrusted.
/// Convert a script-supplied Lua integer to `u8`, rejecting out-of-range
/// values instead of truncating with `as u8`.
///
/// The mlua engine rejects these at the binding boundary; this engine used
/// `as u8`, so `hardware.gpio_write(256, 1)` silently drove pin 0 and
/// `i2c_write(0x148, …)` addressed device 0x48. Same script, different
/// hardware action depending on the engine — the cast must be checked.
fn to_u8_arg(value: i64, what: &str) -> Result<u8> {
    u8::try_from(value)
        .map_err(|_| LuaHostError::Lua(format!("{what} out of range 0..=255: {value}")))
}

/// Convert a script-supplied Lua integer to `u32` (flash addresses),
/// rejecting negative or over-wide values instead of truncating.
fn to_u32_arg(value: i64, what: &str) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| LuaHostError::Lua(format!("{what} out of range 0..=4294967295: {value}")))
}

fn pop_read_len<'gc>(
    ctx: piccolo::Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    what: &str,
    max: usize,
) -> Result<usize> {
    let raw = pop_i64(ctx, stack)?;
    if raw < 0 {
        return Err(LuaHostError::Lua(format!("{what}: negative length {raw}")));
    }
    let len = raw as usize;
    if len > max {
        return Err(LuaHostError::Lua(format!(
            "{what}: length {len} exceeds maximum {max}"
        )));
    }
    Ok(len)
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
                let pin = to_u8_arg(pin, "gpio_write pin").map_err(to_lua_error)?;
                let level = to_u8_arg(level, "gpio_write level").map_err(to_lua_error)?;
                backend
                    .gpio_write(pin, level)
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
                let pin = to_u8_arg(pin, "gpio_read pin").map_err(to_lua_error)?;
                let v = backend
                    .gpio_read(pin)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, v as i64);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "gpio_read", gpio_read);

            let hw_i2c_read = hw.clone();
            let i2c_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let reg = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let len = pop_read_len(ctx, &mut stack, "i2c_read", MAX_I2C_READ_LEN)
                    .map_err(to_lua_error)?;
                let mut backend = hw_i2c_read.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let addr = to_u8_arg(addr, "i2c_read addr").map_err(to_lua_error)?;
                let reg = to_u8_arg(reg, "i2c_read reg").map_err(to_lua_error)?;
                let bytes = backend
                    .i2c_read(addr, reg, len)
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
                let addr = to_u8_arg(addr, "i2c_write addr").map_err(to_lua_error)?;
                let reg = to_u8_arg(reg, "i2c_write reg").map_err(to_lua_error)?;
                if data.len() > MAX_I2C_WRITE_LEN {
                    return Err(to_lua_error(LuaHostError::Lua(format!(
                        "i2c_write payload too long ({} > {MAX_I2C_WRITE_LEN})",
                        data.len()
                    ))));
                }
                backend
                    .i2c_write(addr, reg, data.as_bytes())
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "i2c_write", i2c_write);

            // The mlua engine exposes `i2c_transfer`; without it here, an app
            // debugged on the host failed on the S3 with "call a nil value".
            let hw_i2c_transfer = hw.clone();
            let i2c_transfer = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let reg = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let tx = pop_str(ctx, &mut stack).map_err(to_lua_error)?;
                let rx_len = pop_read_len(ctx, &mut stack, "i2c_transfer", MAX_I2C_READ_LEN)
                    .map_err(to_lua_error)?;
                if tx.len() > MAX_I2C_WRITE_LEN {
                    return Err(to_lua_error(LuaHostError::Lua(format!(
                        "i2c_transfer tx too long ({} > {MAX_I2C_WRITE_LEN})",
                        tx.len()
                    ))));
                }
                let addr = to_u8_arg(addr, "i2c_transfer addr").map_err(to_lua_error)?;
                let reg = to_u8_arg(reg, "i2c_transfer reg").map_err(to_lua_error)?;
                let mut backend = hw_i2c_transfer.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let out = backend
                    .i2c_transfer(addr, reg, tx.as_bytes(), rx_len)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                let s = LuaString::from_slice(&ctx, &out);
                stack.replace(ctx, s);
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "i2c_transfer", i2c_transfer);

            let hw_adc = hw.clone();
            let adc_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let pin = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let mut backend = hw_adc.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let pin = to_u8_arg(pin, "adc_read pin").map_err(to_lua_error)?;
                let v = backend
                    .adc_read(pin)
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
                let pin = to_u8_arg(pin, "pwm_set pin").map_err(to_lua_error)?;
                let duty = to_u8_arg(duty, "pwm_set duty").map_err(to_lua_error)?;
                backend
                    .pwm_set(pin, duty)
                    .map_err(|e| to_lua_error(LuaHostError::Hardware(e)))?;
                stack.replace(ctx, ());
                Ok(CallbackReturn::Return)
            });
            let _ = hardware_table.set(ctx, "pwm_set", pwm_set);

            let hw_flash = hw.clone();
            let flash_read = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                let addr = pop_i64(ctx, &mut stack).map_err(to_lua_error)?;
                let len = pop_read_len(ctx, &mut stack, "flash_read", MAX_FLASH_READ_LEN)
                    .map_err(to_lua_error)?;
                let mut backend = hw_flash.lock().map_err(|_| {
                    to_lua_error(LuaHostError::Hardware("hardware lock poisoned".into()))
                })?;
                let addr = to_u32_arg(addr, "flash_read address").map_err(to_lua_error)?;
                let bytes = backend
                    .flash_read(addr, len)
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
                let addr = to_u32_arg(addr, "flash_write address").map_err(to_lua_error)?;
                backend
                    .flash_write(addr, data.as_bytes())
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
                let addr = to_u32_arg(addr, "flash_erase address").map_err(to_lua_error)?;
                backend
                    .flash_erase_sector(addr)
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
                if data.len() > MAX_PAYLOAD_LEN {
                    return Err(to_lua_error(LuaHostError::Lua(format!(
                        "ble_send payload too long ({} > {MAX_PAYLOAD_LEN})",
                        data.len()
                    ))));
                }
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
                let profile = to_u8_arg(profile, "power_set profile").map_err(to_lua_error)?;
                backend
                    .power_set(profile)
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
                // Include the separator the agent inserts between the two
                // parts, so a request at exactly the cap is not rejected
                // later as a TooLong agent error.
                let sep = usize::from(!context.is_empty() && !prompt.is_empty());
                if context.len() + prompt.len() + sep > MAX_REASON_LEN {
                    return Err(to_lua_error(LuaHostError::Lua(format!(
                        "agent.reason text too long ({} > {MAX_REASON_LEN})",
                        context.len() + prompt.len() + sep
                    ))));
                }
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
            .and_then(|stashed| execute_bounded(&mut self.lua, &stashed).map(|_| ()))?;
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
        // Do NOT demand a string return: piccolo's `String: FromValue` rejects
        // nil/number/boolean with a TypeError, while the mlua engine converts
        // them (`on_tick` returning nil is the normal case for a hook that
        // only performs actions). `execute_bounded` performs the same
        // conversion.
        execute_bounded(&mut self.lua, &stashed)
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
fn execute_bounded(lua: &mut Lua, stashed: &StashedExecutor) -> Result<String> {
    const FUEL_PER_ITER: i32 = 4096; // per-step fuel (matches `Lua::finish`)
    const MAX_FUEL: i64 = 20_000_000; // total op budget (~tens of millions of ops)
    // Bound the piccolo VM's total allocation (gc-arena bytes). The cap has to
    // follow the **board's** PSRAM, not the largest module: the shipped quad
    // dev board has 2 MB total, shared with Wi-Fi/TLS/the agent, so the old
    // flat 8 MiB allowed a script to exhaust the pool and abort the process.
    // 512 KiB is the same bound the mlua sandbox uses; the N8R8 (8 MB octal)
    // profile raises it to 2 MiB.
    #[cfg(feature = "s3-8mb-psram")]
    const MAX_MEMORY: usize = 2 * 1024 * 1024;
    #[cfg(not(feature = "s3-8mb-psram"))]
    const MAX_MEMORY: usize = 512 * 1024;

    let mut budget: i64 = MAX_FUEL;
    loop {
        let mut fuel = Fuel::with(FUEL_PER_ITER);
        let done = lua.enter(|ctx| ctx.fetch(stashed).step(ctx, &mut fuel));
        budget -= i64::from(FUEL_PER_ITER - fuel.remaining());
        if lua.total_memory() > MAX_MEMORY {
            return Err(LuaHostError::Lua("script memory limit exceeded".into()));
        }
        if done {
            // Convert the value to an owned String *inside* the arena closure:
            // a piccolo `Value` borrows the GC arena, so it cannot be returned
            // through `try_enter`.
            let result = lua.try_enter(|ctx| {
                match ctx.fetch(stashed).take_result::<piccolo::Value>(ctx) {
                    Ok(Ok(value)) => match value {
                        piccolo::Value::String(s) => Ok(std::str::from_utf8(s.as_bytes())
                            .unwrap_or_default()
                            .to_owned()),
                        piccolo::Value::Integer(i) => Ok(i.to_string()),
                        piccolo::Value::Number(n) => Ok(n.to_string()),
                        piccolo::Value::Boolean(b) => Ok(b.to_string()),
                        piccolo::Value::Nil => Ok(String::new()),
                        other => Err(to_lua_error(LuaHostError::Lua(format!(
                            "call: unsupported return type {other:?}"
                        )))),
                    },
                    Ok(Err(e)) => Err(e),
                    Err(_mode) => {
                        Err(to_lua_error(LuaHostError::Lua("bad executor mode".into())))
                    }
                }
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
                // Cap the precision: `%.1000000000f` used to allocate ~1 GiB
                // on the host heap (outside the arena memory limit) and
                // `%.18446744073709551615f` hit a capacity overflow panic.
                let prec: usize = prec_digits.parse().unwrap_or(0).min(32);
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
            .map(std::borrow::ToOwned::to_owned)
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
/// Comparison budget for a single `string.match` call. ~1M byte comparisons
/// is well under a millisecond on the host and a few ms on the S3, while a
/// pathological pattern would otherwise run for minutes.
const MAX_MATCH_STEPS: usize = 1_000_000;

fn do_string_match(s: &str, pat: &str, init: i64) -> Option<String> {
    // `init` is a Lua 1-based index into a *byte* string. This shim holds a
    // `&str`, so a script passing an index that lands inside a multi-byte
    // code point would panic on the slice below. Back it up to the previous
    // character boundary instead — real Lua would index the raw byte and
    // return garbage, which is better than aborting the VM.
    let mut start = (init.max(1) as usize).saturating_sub(1).min(s.len());
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
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
        /// Non-empty for a capture group: the group's own instruction
        /// program. The matcher runs it recursively (including nested
        /// groups) so the whole group — not just its first atom — is
        /// matched and captured.
        inner: Vec<Inst>,
    }

    let pb = pat.as_bytes();

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

    /// Parse a pattern body into instructions, recursing into `(...)` groups.
    ///
    /// A group becomes one `Inst` with a non-empty `inner` program and the
    /// quantifier written after `)` applied to the *whole* group. The old
    /// parser inlined the group's atoms (capturing only the first) and never
    /// consumed the trailing quantifier, so `(ab)` captured just `"a"` and
    /// `(ab)+` failed (the `+` became a literal).
    fn parse_group(pb: &[u8]) -> Option<Vec<Inst>> {
        let mut out: Vec<Inst> = Vec::new();
        let mut i = 0;
        while i < pb.len() {
            let c = pb[i];
            // Anchors: `^` only at the very start, `$` only at the very end.
            if c == b'^' && i == 0 {
                i += 1;
                continue;
            }
            // `$` is only an anchor at the end and only when it is not
            // escaped (`%$` is a literal dollar).
            if c == b'$' && i + 1 == pb.len() && !is_escaped(pb, i) {
                i += 1;
                continue;
            }
            if c == b'(' {
                i += 1;
                let mut depth = 1usize;
                let group_start = i;
                while i < pb.len() && depth > 0 {
                    match pb[i] {
                        b'(' => {
                            depth += 1;
                            i += 1;
                        }
                        b')' => {
                            depth -= 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                if depth != 0 {
                    return None; // unterminated capture
                }
                let body = &pb[group_start..i - 1];
                let inner = parse_group(body)?;
                let mut quant = Quant::Once;
                if i < pb.len() {
                    match pb[i] {
                        b'*' => {
                            quant = Quant::Star;
                            i += 1;
                        }
                        b'+' => {
                            quant = Quant::Plus;
                            i += 1;
                        }
                        b'?' => {
                            quant = Quant::Opt;
                            i += 1;
                        }
                        _ => {}
                    }
                }
                out.push(Inst {
                    tok: Class::Lit(0),
                    quant,
                    capture: true,
                    inner,
                });
                continue;
            }
            if let Some((tok, quant)) = parse_atomic(pb, &mut i) {
                out.push(Inst {
                    tok,
                    quant,
                    capture: false,
                    inner: Vec::new(),
                });
            } else {
                return None;
            }
        }
        Some(out)
    }

    /// Whether the byte at `idx` is preceded by an odd number of `%` escapes.
    fn is_escaped(pb: &[u8], idx: usize) -> bool {
        let mut count = 0usize;
        let mut j = idx;
        while j > 0 && pb[j - 1] == b'%' {
            count += 1;
            j -= 1;
        }
        count % 2 == 1
    }

    let code = parse_group(pb)?;

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

    /// Charge one comparison against the shared budget. Returns `false`
    /// when the budget is exhausted.
    fn bump(steps: &mut usize) -> bool {
        *steps += 1;
        *steps <= MAX_MATCH_STEPS
    }

    // Match a sub-program greedily from `pos`; returns (final_pos,
    // first_capture_span). An instruction marked `capture = true` records
    // the span it consumed (the bytes from before it runs to where it
    // leaves `p`).
    fn match_sub(
        code: &[Inst],
        s: &[u8],
        pos: usize,
        steps: &mut usize,
    ) -> Option<(usize, Option<(usize, usize)>)> {
        let mut p = pos;
        let mut capture: Option<(usize, usize)> = None;
        for inst in code {
            if !inst.inner.is_empty() {
                // Capture group: run the inner program, repeating it when the
                // group itself carries `+`/`*`/`?`. The captured span is the
                // first repetition's match, so `(ab)` captures "ab" rather
                // than just its first atom.
                let group_start = p;
                let mut first_end: Option<usize> = None;
                let required = matches!(inst.quant, Quant::Once | Quant::Plus);
                let max_reps = s.len().saturating_add(1);
                let mut reps = 0usize;
                loop {
                    if reps >= max_reps {
                        break;
                    }
                    let before = p;
                    match match_sub(&inst.inner, s, p, steps) {
                        Some((end, _)) => {
                            if reps == 0 {
                                first_end = Some(end);
                            }
                            p = end;
                            reps += 1;
                            // A zero-width repetition can never advance; stop
                            // rather than loop forever.
                            if end == before {
                                break;
                            }
                            match inst.quant {
                                Quant::Once | Quant::Opt => break,
                                Quant::Star | Quant::Plus => {}
                            }
                        }
                        None => break,
                    }
                }
                if required && reps == 0 {
                    return None;
                }
                if inst.capture && capture.is_none() {
                    if let Some(end) = first_end {
                        if end > group_start {
                            capture = Some((group_start, end));
                        }
                    }
                }
                continue;
            }
            let inst_start = p;
            match inst.quant {
                Quant::Once => {
                    if !bump(steps) || p >= s.len() || !matches_class(&inst.tok, s[p]) {
                        return None;
                    }
                    p += 1;
                }
                Quant::Opt => {
                    if !bump(steps) {
                        return None;
                    }
                    if p < s.len() && matches_class(&inst.tok, s[p]) {
                        p += 1;
                    }
                }
                Quant::Plus => {
                    if !bump(steps) || p >= s.len() || !matches_class(&inst.tok, s[p]) {
                        return None;
                    }
                    p += 1;
                    while p < s.len() {
                        if !bump(steps) {
                            return None;
                        }
                        if !matches_class(&inst.tok, s[p]) {
                            break;
                        }
                        p += 1;
                    }
                }
                Quant::Star => {
                    while p < s.len() {
                        if !bump(steps) {
                            return None;
                        }
                        if !matches_class(&inst.tok, s[p]) {
                            break;
                        }
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
    // `^` anchors the match to the start of the (post-`init`) subject;
    // `$` requires it to end at the very end. Both were previously parsed
    // and then silently ignored, so `match("xabc", "^abc")` returned a
    // match at offset 1 and validation patterns could be bypassed.
    let anchored_start = pb.first() == Some(&b'^');
    // `%$` is an escaped literal dollar, not the end anchor.
    let anchored_end =
        pb.last() == Some(&b'$') && !is_escaped(pb, pb.len().saturating_sub(1));
    let starts: std::ops::RangeInclusive<usize> = if anchored_start {
        0..=0
    } else {
        0..=sb.len()
    };
    // Total comparisons across every start position. Without this cap the
    // matcher is O(n·m) *inside one Rust callback*, which piccolo's fuel
    // cannot preempt: two ~200 KiB strings and `match(s, s)` locked the S3
    // Lua thread for minutes. Exceeding the cap returns "no match".
    let mut steps = 0usize;
    for pos in starts {
        let matched = match_sub(&code, sb, pos, &mut steps);
        if steps > MAX_MATCH_STEPS {
            return None;
        }
        if let Some((end, cap)) = matched {
            if anchored_end && end != sb.len() {
                continue;
            }
            if let Some((gs, ge)) = cap {
                if gs < ge && ge <= sb.len() {
                    return Some(
                        std::str::from_utf8(&sb[gs..ge])
                            .map(std::borrow::ToOwned::to_owned)
                            .unwrap_or_default(),
                    );
                }
            }
            let len = end.min(sb.len());
            if len >= pos {
                return Some(
                    std::str::from_utf8(&sb[pos..len])
                        .map(std::borrow::ToOwned::to_owned)
                        .unwrap_or_default(),
                );
            }
        }
    }
    None
}
