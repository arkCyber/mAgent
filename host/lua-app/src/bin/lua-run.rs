//! `magent-lua` developer CLI — run a Lua app against the host simulator.
//!
//! Lets a developer iterate on `main.lua` from the shell without touching
//! Rust: it boots the script through `AppRuntime` and runs a number of
//! event-loop ticks against the RAM-backed `SimHardware`.
//!
//! ```
//! cargo run -p magent-lua --bin lua-run -- --script path/to/main.lua
//! cargo run -p magent-lua --bin lua-run -- --temp 60 --action SET_COOLING_PULSE:80 --ticks 3
//! ```
//!
//! `--action <s>` installs a mock LLM backend so `agent.reason()` returns a
//! fixed action (no network needed); without it a heuristic `MiniAgent` is
//! used. This is the host-side "Lua working environment" for the ESP32-S3
//! (whose real Lua VM is gated behind the firmware `lua` feature).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use magent_core::MiniAgent;
use magent_lua::hardware::SimHardware;
use magent_lua::runtime::AppRuntime;
#[cfg(feature = "mlua")]
use magent_lua::LuaVm;
use magent_lua::{install_mock_agent, HardwareBackend, SharedAgent, SharedHardware};

fn usage() -> &'static str {
    "magent-lua developer CLI\n\
     \n\
     USAGE: cargo run -p magent-lua --bin lua-run -- [OPTIONS]\n\
     \n\
     OPTIONS:\n\
     \x20 --script <path>   Lua app to boot (default: bundled lua/main.lua)\n\
     \x20 --temp <c>        simulated die temperature, °C (default 42)\n\
     \x20 --action <s>      mock agent.reason() answer (e.g. SET_COOLING_PULSE:80)\n\
     \x20 --ticks <n>       event-loop ticks to run (default 5)\n\
     \x20 --help            show this help"
}

#[cfg(feature = "mlua")]
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();

    let mut script: Option<PathBuf> = None;
    let mut temp = 42.0f64;
    let mut action: Option<String> = None;
    let mut ticks = 5u64;

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--script" => {
                i += 1;
                let p = args.get(i).ok_or("--script needs a path")?;
                script = Some(PathBuf::from(p));
            }
            "--temp" => {
                i += 1;
                temp = args
                    .get(i)
                    .ok_or("--temp needs a number")?
                    .parse()
                    .map_err(|e| format!("bad --temp: {e}"))?;
            }
            "--action" => {
                i += 1;
                action = Some(args.get(i).ok_or("--action needs a value")?.clone());
            }
            "--ticks" => {
                i += 1;
                ticks = args
                    .get(i)
                    .ok_or("--ticks needs a number")?
                    .parse()
                    .map_err(|e| format!("bad --ticks: {e}"))?;
            }
            "--help" | "-h" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
        i += 1;
    }

    // Build the hardware (warm die) + agent (mock or heuristic).
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(
        SimHardware::default().with_temperature(temp as f32),
    ));
    let hardware: SharedHardware = sim.clone();
    let agent: SharedAgent = match action.as_deref() {
        Some(a) => install_mock_agent(a).map_err(|e| e.to_string())?,
        None => {
            // TRACE: REQ-LUA-SANDBOX — single-threaded VM design; see examples/demo.rs.
            #[allow(clippy::arc_with_non_send_sync)]
            let agent = Arc::new(Mutex::new(
                MiniAgent::with_defaults().map_err(|e| format!("agent init: {e}"))?,
            ));
            agent
        }
    };

    let vm = LuaVm::new(hardware.clone(), agent).map_err(|e| format!("vm init: {e}"))?;
    let mut app = AppRuntime::new(vm, hardware.clone());

    // Boot the script (bundled default or a user file).
    let source = match &script {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("read {}: {e}", p.display()))?,
        None => include_str!("../../lua/main.lua").to_string(),
    };
    app.boot(&source).map_err(|e| format!("boot failed: {e}"))?;

    // Run the requested number of ticks.
    for _ in 0..ticks {
        let t = app.tick().map_err(|e| format!("tick failed: {e}"))?;
        println!(
            "tick {} @ {}ms result='{}' dispatched={}",
            t.index, t.uptime_ms, t.result, t.dispatched
        );
    }

    let h = app.health(std::time::Duration::from_secs(5));
    println!(
        "health: uptime={}ms ticks={} errors={} last_error={:?} stale={}",
        h.uptime_ms, h.tick_count, h.error_count, h.last_error, h.stale
    );
    // Show a couple of observable hardware effects.
    let mut sim = sim.lock().unwrap();
    println!(
        "hardware: pwm_fan={} gpio1={} adc1={}v",
        sim.pwm_duty(1),
        sim.gpio_read(1).unwrap_or(0),
        sim.adc_read(1).unwrap_or(0.0)
    );

    Ok(())
}

/// Host dev tool only: requires the default `mlua` engine. When built without
/// `mlua` (e.g. `--no-default-features --features piccolo` for the S3), print a
/// clear message and exit — this bin is not part of the firmware.
#[cfg(not(feature = "mlua"))]
fn main() {
    eprintln!("lua-run (host dev CLI) requires the default `mlua` feature. Use `cargo test -p magent-lua --features piccolo --test piccolo_tests` for the pure-Rust engine, or build with default features.");
    std::process::exit(2);
}
