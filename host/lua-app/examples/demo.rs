//! Host demo: boots the sandboxed Lua VM and runs an embedded `main.lua`.
//!
//! This is the *host validation* half of the two-stage plan. It proves the
//! `hardware.*` + `agent.reason()` bindings end-to-end on a desktop OS with
//! `cargo run`, before the same bindings are wired into the ESP32-S3 firmware
//! (which needs Xtensa toolchain + real board).
//!
//! Run with (requires `mlua` feature): `cargo run -p magent-lua --example demo --features mlua`

use std::sync::{Arc, Mutex};

use magent_core::MiniAgent;
use magent_lua::hardware::SimHardware;
use magent_lua::HardwareBackend;

#[cfg(feature = "mlua")]
use magent_lua::LuaVm;

fn main() -> Result<(), String> {
    #[cfg(feature = "mlua")]
    {
        // Simulated die starts warm (42 °C) so the demo exercises the
        // `agent.reason()` path rather than only the deterministic branch.
        let hardware: Arc<Mutex<dyn HardwareBackend>> =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));

        // TRACE: REQ-LUA-SANDBOX — the Lua VM is single-threaded by design; each
        // VM and its `MiniAgent` live on one thread (mlua default is non-`Send`),
        // so `Arc<Mutex<MiniAgent>>` is intentionally not required to be `Send`.
        #[allow(clippy::arc_with_non_send_sync)]
        let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(
            MiniAgent::with_defaults().map_err(|e| format!("agent init: {e}"))?,
        ));

        let vm = LuaVm::new(hardware, agent).map_err(|e| format!("vm init: {e}"))?;

        let script = include_str!("../lua/main.lua");
        vm.run_script(script)
            .map_err(|e| format!("main.lua failed: {e}"))?;

        println!("demo: main.lua completed without error");
        Ok(())
    }
    #[cfg(not(feature = "mlua"))]
    {
        Err("demo requires the `mlua` feature".into())
    }
}
