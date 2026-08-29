//! Host demo: runs the three bundled Lua apps (greenhouse / datalogger / alarm)
//! against the sandboxed VM, using a mock LLM backend so the agent path is
//! exercised deterministically.
//!
//! Usage (requires the `mlua` feature):
//! ```
//! cargo run -p magent-lua --example apps --features mlua -- greenhouse   # default temp 42, mock SET_COOLING_PULSE:60
//! cargo run -p magent-lua --example apps --features mlua -- datalogger
//! cargo run -p magent-lua --example apps --features mlua -- alarm
//! ```
//!
//! Each app:
//!   1. boots the sandboxed VM with the matching `lua/apps/<name>.lua`,
//!   2. drives the supervised event loop for a number of ticks,
//!   3. prints the resulting hardware state (PWM, GPIO, BLE payloads, NVRAM).

use std::sync::{Arc, Mutex};

use magent_lua::hardware::SimHardware;
use magent_lua::runtime::AppRuntime;
use magent_lua::{install_mock_agent, SharedHardware};

#[cfg(feature = "mlua")]
use magent_lua::LuaVm;

/// Run one of the three apps, with the given mock agent answer, for `ticks`
/// ticks. Returns the simulator (so the caller can read out the post-run state).
#[cfg(feature = "mlua")]
fn run_app(
    name: &str,
    script: &str,
    mock_action: &str,
    ticks: u64,
) -> Result<Arc<Mutex<SimHardware>>, String> {
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(
        // Warm die so the greenhouse app crosses its THERMAL_HIGH threshold
        // and exercises the agent path.
        SimHardware::default().with_temperature(42.0),
    ));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent(mock_action).map_err(|e| e.to_string())?;
    let vm = LuaVm::new(hardware.clone(), agent).map_err(|e| format!("vm init: {e}"))?;
    let mut app = AppRuntime::new(vm, hardware);

    app.boot(script)
        .map_err(|e| format!("{name}: boot failed: {e}"))?;

    for i in 0..ticks {
        let t = app
            .tick()
            .map_err(|e| format!("{name}: tick {i} failed: {e}"))?;
        println!(
            "[{name}] tick {} @ {}ms result='{}' dispatched={}",
            t.index, t.uptime_ms, t.result, t.dispatched
        );
    }

    let h = app.health(std::time::Duration::from_secs(5));
    println!(
        "[{name}] health: uptime={}ms ticks={} errors={} last_error={:?}",
        h.uptime_ms, h.tick_count, h.error_count, h.last_error
    );
    Ok(sim)
}

fn main() -> Result<(), String> {
    #[cfg(feature = "mlua")]
    {
        let args: Vec<String> = std::env::args().collect();
        let which = args.get(1).map(String::as_str).unwrap_or("greenhouse");

        let (script, mock_action, ticks) = match which {
            "greenhouse" => (
                include_str!("../lua/apps/greenhouse.lua"),
                "SET_COOLING_PULSE:60",
                5,
            ),
            "datalogger" => (
                include_str!("../lua/apps/datalogger.lua"),
                "BLE_SEND:magent-01,42.00,3.70",
                10,
            ),
            "alarm" => (
                include_str!("../lua/apps/alarm.lua"),
                "OK",
                8,
            ),
            other => {
                return Err(format!(
                    "unknown app: {other}\nusage: greenhouse | datalogger | alarm"
                ))
            }
        };

        let sim = run_app(which, script, mock_action, ticks)?;

        let s = sim.lock().unwrap();
        println!(
            "[{which}] hardware state: pwm1={} pwm3={} pwm4={}",
            s.pwm_duty(1),
            s.pwm_duty(3),
            s.pwm_duty(4),
        );
        Ok(())
    }
    #[cfg(not(feature = "mlua"))]
    {
        Err("apps example requires the `mlua` feature".into())
    }
}
