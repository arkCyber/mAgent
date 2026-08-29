//! [`LuaVm`] — the sandboxed interpreter with `hardware.*` and `agent.reason`
//! bound in.
//!
//! The VM is the single entry point an application boots. It:
//! 1. constructs a sandboxed interpreter ([`crate::sandbox`]);
//! 2. enforces the heap / instruction budgets;
//! 3. registers the `hardware` table backed by a [`HardwareBackend`];
//! 4. registers the `agent` table backed by a `SharedAgent`.

use mlua::Lua;

use crate::agent::{reason as agent_reason, SharedAgent};
use crate::error::{LuaHostError, Result};
use crate::hardware::SharedHardware;
use crate::nvram;
use crate::sandbox;
use crate::sandbox::Budget;

/// Upper bound (bytes) for a byte payload passed to `hardware.*` (BLE, I2C)
/// from a script — a defensive cap so a hostile script cannot force an
/// unbounded host-side allocation.
pub const MAX_PAYLOAD_LEN: usize = 4096;
/// Upper bound (bytes) for a single I2C write payload.
pub const MAX_I2C_LEN: usize = 256;
/// Upper bound (bytes) for the combined `agent.reason(context, prompt)` text.
pub const MAX_REASON_LEN: usize = 2048;

/// A sandboxed Lua 5.4 interpreter with the mAgent bindings registered.
pub struct LuaVm {
    lua: Lua,
    budget: Budget,
    /// Seeds kept so the interpreter can be rebuilt from a clean slate on
    /// script hot-reload (`[`LuaVm::reload_state`]`).
    hardware: SharedHardware,
    agent: SharedAgent,
}

impl LuaVm {
    /// Construct the VM, register the `hardware` / `agent` bindings, and
    /// enforce the resource budget. Every step returns `Err` on failure — it
    /// never panics.
    pub fn new(hardware: SharedHardware, agent: SharedAgent) -> Result<Self> {
        let lua = sandbox::new_sandbox().map_err(LuaHostError::from)?;
        let budget = sandbox::enforce_budget(&lua)?;
        Self::register_hardware(&lua, hardware.clone())?;
        Self::register_agent(&lua, agent.clone())?;
        Ok(Self {
            lua,
            budget,
            hardware,
            agent,
        })
    }

    /// Run a Lua chunk in the sandbox.
    ///
    /// A parse error, runtime error, sandbox rejection, or budget exhaustion
    /// is returned as `Err([`LuaHostError`])` — never propagated as a panic.
    /// This is the choke-point where a Lua `longjmp` becomes a `Result`.
    ///
    /// The per-script instruction budget is reset first so a long-lived VM
    /// never exhausts a cumulative budget across many scripts.
    pub fn run_script(&self, script: &str) -> Result<()> {
        self.budget.reset();
        self.lua.load(script).exec().map_err(LuaHostError::from)
    }

    /// Whether a named global function is currently defined.
    ///
    /// Used by [`crate::runtime::AppRuntime`] to tell "handler absent"
    /// (benign, empty tick) from "handler errored" (surfaced).
    pub fn has(&self, fname: &str) -> bool {
        self.lua.globals().get::<mlua::Function>(fname).is_ok()
    }

    /// Rebuild the interpreter from a clean slate: fresh sandbox + budgets,
    /// re-registered `hardware`/`agent` bindings, and empty globals.
    ///
    /// Used by [`crate::runtime::AppRuntime::reload`] for script hot-reload so
    /// stale globals (e.g. an old `on_tick`) cannot leak into the new app.
    pub fn reload_state(&mut self) -> Result<()> {
        let lua = sandbox::new_sandbox().map_err(LuaHostError::from)?;
        let budget = sandbox::enforce_budget(&lua)?;
        Self::register_hardware(&lua, self.hardware.clone())?;
        Self::register_agent(&lua, self.agent.clone())?;
        self.lua = lua;
        self.budget = budget;
        Ok(())
    }

    /// Invoke a named global Lua function with string arguments and return its
    /// string result.
    ///
    /// This is the event-loop entry point: a script defines `on_tick(...)` /
    /// `on_event(...)` and the host calls it on a timer / interrupt instead of
    /// re-running a whole chunk. A missing function, a runtime error, or a
    /// non-stringifiable return is returned as `Err`, never a panic.
    pub fn call(&self, fname: &str, args: &[String]) -> Result<String> {
        // Reset the per-script instruction budget before invoking the handler.
        self.budget.reset();
        let func: mlua::Function = self.lua.globals().get(fname).map_err(LuaHostError::from)?;
        // Build a vararg `MultiValue` so each element is a separate argument.
        // (A bare `Vec`/slice would otherwise be packed as one Lua table.)
        let mut multi = mlua::MultiValue::new();
        for a in args {
            let s = self
                .lua
                .create_string(a.as_str())
                .map_err(LuaHostError::from)?;
            multi.push_back(mlua::Value::String(s));
        }
        let value: mlua::Value = func.call(multi).map_err(LuaHostError::from)?;
        match value {
            mlua::Value::String(s) => Ok(s.to_str().map_err(LuaHostError::from)?.to_owned()),
            mlua::Value::Integer(i) => Ok(i.to_string()),
            mlua::Value::Number(n) => Ok(n.to_string()),
            mlua::Value::Boolean(b) => Ok(b.to_string()),
            mlua::Value::Nil => Ok(String::new()),
            other => Err(LuaHostError::Lua(format!(
                "call {fname}: unsupported return type {other:?}"
            ))),
        }
    }

    /// Register the `hardware` table onto the globals.
    fn register_hardware(lua: &Lua, hw: SharedHardware) -> Result<()> {
        let table = lua.create_table().map_err(LuaHostError::from)?;

        let hw_gpio_write = hw.clone();
        let gpio_write = lua
            .create_function(move |_, (pin, level): (u8, u8)| {
                let mut backend = hw_gpio_write
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .gpio_write(pin, level)
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("gpio_write", gpio_write)
            .map_err(LuaHostError::from)?;

        let hw_gpio_read = hw.clone();
        let gpio_read = lua
            .create_function(move |_, pin: u8| {
                let mut backend = hw_gpio_read
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend.gpio_read(pin).map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("gpio_read", gpio_read)
            .map_err(LuaHostError::from)?;

        let hw_sensor_read = hw.clone();
        let sensor_read = lua
            .create_function(move |_, name: String| {
                let mut backend = hw_sensor_read
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .sensor_read(&name)
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("sensor_read", sensor_read)
            .map_err(LuaHostError::from)?;

        let hw_flash_read = hw.clone();
        let flash_read = lua
            .create_function(move |lua, (address, len): (u32, usize)| {
                let mut backend = hw_flash_read
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                let bytes = backend
                    .flash_read(address, len)
                    .map_err(mlua::Error::RuntimeError)?;
                // Build an explicit Lua string (mlua would otherwise map a
                // `Vec<u8>` to a numeric table, not a byte string).
                lua.create_string(&bytes)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("flash_read", flash_read)
            .map_err(LuaHostError::from)?;

        let hw_flash_write = hw.clone();
        let flash_write = lua
            .create_function(move |_, (address, data): (u32, mlua::String)| {
                let mut backend = hw_flash_write
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .flash_write(address, data.as_bytes().as_ref())
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("flash_write", flash_write)
            .map_err(LuaHostError::from)?;

        let hw_i2c_read = hw.clone();
        let i2c_read = lua
            .create_function(move |lua, (addr, reg, len): (u8, u8, usize)| {
                let mut backend = hw_i2c_read
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                let bytes = backend
                    .i2c_read(addr, reg, len)
                    .map_err(mlua::Error::RuntimeError)?;
                // Explicit Lua byte string (mlua would map `Vec<u8>` to a
                // numeric table otherwise).
                lua.create_string(&bytes)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("i2c_read", i2c_read)
            .map_err(LuaHostError::from)?;

        let hw_i2c_transfer = hw.clone();
        let i2c_transfer = lua
            .create_function(
                move |lua, (addr, reg, tx, rx_len): (u8, u8, mlua::String, usize)| {
                    let tx_bytes = tx.as_bytes();
                    if tx_bytes.len() > MAX_I2C_LEN {
                        return Err(mlua::Error::RuntimeError(format!(
                            "i2c_transfer tx too long ({} > {MAX_I2C_LEN})",
                            tx_bytes.len()
                        )));
                    }
                    let mut backend = hw_i2c_transfer
                        .lock()
                        .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                    let out = backend
                        .i2c_transfer(addr, reg, tx_bytes.as_ref(), rx_len)
                        .map_err(mlua::Error::RuntimeError)?;
                    lua.create_string(&out)
                },
            )
            .map_err(LuaHostError::from)?;
        table
            .set("i2c_transfer", i2c_transfer)
            .map_err(LuaHostError::from)?;

        let hw_adc_read = hw.clone();
        let adc_read = lua
            .create_function(move |_, pin: u8| {
                let mut backend = hw_adc_read
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend.adc_read(pin).map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("adc_read", adc_read)
            .map_err(LuaHostError::from)?;

        let hw_i2c_write = hw.clone();
        let i2c_write = lua
            .create_function(move |_, (addr, reg, data): (u8, u8, mlua::String)| {
                let bytes = data.as_bytes();
                if bytes.len() > MAX_I2C_LEN {
                    return Err(mlua::Error::RuntimeError(format!(
                        "i2c_write payload too long ({} > {MAX_I2C_LEN})",
                        bytes.len()
                    )));
                }
                let mut backend = hw_i2c_write
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .i2c_write(addr, reg, bytes.as_ref())
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("i2c_write", i2c_write)
            .map_err(LuaHostError::from)?;

        let hw_pwm_set = hw.clone();
        let pwm_set = lua
            .create_function(move |_, (pin, duty): (u8, u8)| {
                let mut backend = hw_pwm_set
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .pwm_set(pin, duty)
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table.set("pwm_set", pwm_set).map_err(LuaHostError::from)?;

        let hw_ble_send = hw.clone();
        let ble_send = lua
            .create_function(move |_, data: mlua::String| {
                let bytes = data.as_bytes();
                if bytes.len() > MAX_PAYLOAD_LEN {
                    return Err(mlua::Error::RuntimeError(format!(
                        "ble_send payload too long ({} > {MAX_PAYLOAD_LEN})",
                        bytes.len()
                    )));
                }
                let mut backend = hw_ble_send
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .ble_send(bytes.as_ref())
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("ble_send", ble_send)
            .map_err(LuaHostError::from)?;

        let hw_power_set = hw.clone();
        let power_set = lua
            .create_function(move |_, profile: u8| {
                let mut backend = hw_power_set
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                backend
                    .power_set(profile)
                    .map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("power_set", power_set)
            .map_err(LuaHostError::from)?;

        let hw_nvram_get = hw.clone();
        let nvram_get = lua
            .create_function(move |lua, key: String| {
                let mut backend = hw_nvram_get
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                let value = nvram::get(&mut *backend, &key).map_err(mlua::Error::RuntimeError)?;
                match value {
                    Some(s) => Ok(mlua::Value::String(lua.create_string(s.as_bytes())?)),
                    None => Ok(mlua::Value::Nil),
                }
            })
            .map_err(LuaHostError::from)?;
        table
            .set("nvram_get", nvram_get)
            .map_err(LuaHostError::from)?;

        let hw_nvram_set = hw.clone();
        let nvram_set = lua
            .create_function(move |_, (key, value): (String, String)| {
                let mut backend = hw_nvram_set
                    .lock()
                    .map_err(|_| mlua::Error::RuntimeError("hardware lock poisoned".into()))?;
                nvram::set(&mut *backend, &key, &value).map_err(mlua::Error::RuntimeError)
            })
            .map_err(LuaHostError::from)?;
        table
            .set("nvram_set", nvram_set)
            .map_err(LuaHostError::from)?;

        lua.globals()
            .set("hardware", table)
            .map_err(LuaHostError::from)?;
        Ok(())
    }

    /// Register the `agent` table onto the globals.
    fn register_agent(lua: &Lua, agent: SharedAgent) -> Result<()> {
        let table = lua.create_table().map_err(LuaHostError::from)?;

        let reason = lua
            .create_function(move |_, (context, prompt): (String, String)| {
                if context.len() + prompt.len() > MAX_REASON_LEN {
                    return Err(mlua::Error::RuntimeError(format!(
                        "agent.reason text too long ({} > {MAX_REASON_LEN})",
                        context.len() + prompt.len()
                    )));
                }
                agent_reason(&agent, &context, &prompt)
                    .map_err(|e| mlua::Error::RuntimeError(e.to_string()))
            })
            .map_err(LuaHostError::from)?;
        table.set("reason", reason).map_err(LuaHostError::from)?;

        lua.globals()
            .set("agent", table)
            .map_err(LuaHostError::from)?;
        Ok(())
    }
}
