//! Real event-loop demo: boot `main.lua`, then advance the supervised runtime
//! and let a Lua `on_tick` dispatch actions to hardware.
//!
//! Run with (requires `mlua` feature): `cargo run -p magent-lua --example event_loop --features mlua`

use std::sync::{Arc, Mutex};

use magent_core::MiniAgent;
use magent_lua::hardware::SimHardware;
use magent_lua::runtime::AppRuntime;
use magent_lua::SharedHardware;

#[cfg(feature = "mlua")]
use magent_lua::LuaVm;

fn main() -> Result<(), String> {
    #[cfg(feature = "mlua")]
    {
        let hardware: SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        // TRACE: REQ-LUA-SANDBOX — the Lua VM is single-threaded by design; each
        // VM and its `MiniAgent` live on one thread, so `Arc<Mutex<MiniAgent>>` is
        // intentionally not required to be `Send`.
        #[allow(clippy::arc_with_non_send_sync)]
        let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(
            MiniAgent::with_defaults().map_err(|e| format!("agent init: {e}"))?,
        ));

        let vm = LuaVm::new(hardware.clone(), agent).map_err(|e| format!("vm init: {e}"))?;
        let mut app = AppRuntime::new(vm, hardware);

        // Boot the enterprise app once.
        app.boot(include_str!("../lua/main.lua"))
            .map_err(|e| format!("main.lua boot failed: {e}"))?;

        // Define the per-tick handler: raise the fan when the first second of each
        // minute boundary passes, otherwise idle.
        app.boot("function on_tick(ms) if ms % 1000 < 100 then return 'FAN_ON' end return '' end")
            .map_err(|e| format!("on_tick define failed: {e}"))?;

        for _ in 0..5 {
            let t = app.tick().map_err(|e| format!("tick failed: {e}"))?;
            println!(
                "tick {} @ {} ms result='{}' dispatched={}",
                t.index, t.uptime_ms, t.result, t.dispatched
            );
        }

        println!(
            "event_loop: {} ticks, uptime {} ms",
            app.tick_count(),
            app.uptime_ms()
        );
        Ok(())
    }
    #[cfg(not(feature = "mlua"))]
    {
        Err("event_loop requires the `mlua` feature".into())
    }
}
