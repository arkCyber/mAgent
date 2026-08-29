//! # magent-lua — aerospace-grade Lua 5.4 scripting host for mAgent
//!
//! Implements the **"user App as brain, AI agent as brain-trust"** pattern
//! from the chip roadmap: an enterprise developer writes a `main.lua` that
//! owns the deterministic control flow, and — when faced with fuzzy analysis
//! or natural-language requests — calls `agent.reason()` to delegate to the
//! embedded `MiniAgent` (which may in turn reach an LLM over Wi-Fi on a
//! memory-rich chip such as the ESP32-S3).
//!
//! ```lua
//! -- main.lua (enterprise application)
//! local temp = hardware.sensor_read("temp")
//! if temp > 85.0 then
//!     local action = agent.reason("Temp is high.", "What control?")
//!     if string.match(action, "COOL") then hardware.gpio_write(1, 1) end
//! end
//! ```
//!
//! # Design-assurance properties
//!
//! * **No panics in production** — every fallible path returns
//!   [`LuaHostError`]; `#![deny(clippy::panic_in_result_fn)]` is inherited
//!   from the workspace.
//! * **Sandboxed VM** — [`sandbox`] restricts Lua to safe stdlibs and caps
//!   both heap memory and instruction count so a hostile or buggy script
//!   cannot hang the device.
//! * **Caught longjmp** — Lua errors (`longjmp`) are converted to `Result`
//!   by `mlua` at the VM boundary; they never unwind across the Rust stack
//!   (UB).
//! * **Swappable hardware** — Lua only ever talks to [`HardwareBackend`], so
//!   the identical script runs on the host simulator ([`SimHardware`]) or a
//!   real chip.
//!
//! # Crate layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`error`] | Aeronautical-grade `Result` / [`LuaHostError`]. |
//! | [`hardware`] | `HardwareBackend` trait + host `SimHardware`. |
//! | [`agent`] | `agent.reason()` binding onto `MiniAgent`. |
//! | [`action`] | Action grammar + dispatcher (`Action::parse` / `apply_action`). |
//! | [`runtime`] | [`AppRuntime`]: boot + supervised tick loop with heartbeat + watchdog. |
//! | [`nvram`] | Persistent key-value store over the flash backend. |
//! | [`mock`] | [`MockLlmBackend`] / [`install_mock_agent`] for host end-to-end tests. |
//! | [`sandbox`] | Stdlib restriction + memory / instruction budgets. |
//! | [`vm`] | [`LuaVm`] facade that wires it all together. |

#![deny(clippy::panic_in_result_fn)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod action;
pub mod agent;
pub mod engine;
pub mod error;
pub mod hardware;
pub mod mock;
pub mod nvram;
pub mod runtime;

#[cfg(feature = "mlua")]
pub mod sandbox;
#[cfg(feature = "mlua")]
pub mod vm;

#[cfg(feature = "piccolo")]
pub use piccolo_vm::PiccoloVm;
#[cfg(feature = "piccolo")]
pub mod piccolo_vm;

pub use action::{apply_action, Action};
pub use agent::SharedAgent;
pub use engine::{assert_engine_output, engine_name, LuaEngine};
pub use error::{LuaHostError, Result};
#[cfg(not(target_os = "espidf"))]
pub use hardware::SimHardware;
pub use hardware::{HardwareBackend, SharedHardware};
pub use mock::{install_mock_agent, MockLlmBackend};
pub use runtime::{AppRuntime, Health, Tick};
#[cfg(feature = "mlua")]
pub use vm::LuaVm;
