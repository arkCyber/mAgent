//! mAgent Core - chip-agnostic embedded AI agent engine.
//!
//! This library provides the core functionality for the mAgent embedded
//! AI agent, designed for bare-metal environments without an operating
//! system. It is intentionally split into several orthogonal layers:
//!
//! 1. **Chip-agnostic modules** (always compiled):
//!    - `error`, `skills`, `tools`, `agent`
//!    - `health_sensors`, `sports_coach`, `sleep_manager`,
//!      `early_warning`, `voice_notification`
//!    - `safety`, `config`, `power`, `security`, `wear_leveling`,
//!      `hardware`, `monitoring`, `ollama`
//!    - `storage`, `communication`
//!
//! 2. **External-data ingress** (`link_adapters` + `ingress` features):
//!    - `communication::link` — `LinkAdapter` trait + `IngressSource` enum
//!    - `communication::manual` — stdin-based manual adapter (host)
//!    - `communication::mqtt` — MQTT adapter stub (host)
//!    - `ingress` — `IngressGateway` that routes adapter bytes into the
//!      agent loop, optionally wrapping them in `web3::SignedMessage`
//!
//! 3. **Host-only modules** (need `std`):
//!    - `simulator`, `agent_runner`, `real_tools`
//!
//! 3. **Chip-agnostic HAL** (lives in the [`magent-hal`] crate; under
//!    the `std` feature the old `magent_core::hal::*` and
//!    `magent_core::nrf52_hal::*` import paths are preserved by a
//!    compatibility shim).
//!
//! 4. **Chip-specific HAL drivers** (in `magent-hal`, feature-gated):
//!    - nRF52840: enabled by the `nrf52` feature
//!    - ESP32 family: enabled by the `esp32` feature
//!
//! # Feature flags
//!
//! Build matrix:
//!
//! | Target                          | Command                                                              |
//! |---------------------------------|----------------------------------------------------------------------|
//! | nRF52840 firmware               | `cargo build -p firmware/nrf52-app --release`                        |
//! | ESP32 firmware                  | `cargo build -p firmware/esp32-app --release`                        |
//! | Host tests (x86_64 / Linux)     | `cargo test -p magent-core --features std --target x86_64-...`       |
//! | Docs                            | `cargo doc -p magent-core --no-deps`                                  |
//!
//! # Building
//!
//! ```sh
//! # nRF52840 firmware (ARM Cortex-M)
//! cargo build -p magent-nrf52-app --target thumbv7em-none-eabihf --release
//!
//! # ESP32 firmware (Xtensa LX6 / LX7)
//! cargo build -p magent-esp32-app --target xtensa-esp32-espidf --release
//!
//! # ESP32-C3 / C6 firmware (RISC-V)
//! cargo build -p magent-esp32-app --target riscv32imc-esp-espidf --release
//!
//! # Host-side test build
//! cargo test -p magent-core --features std --target x86_64-apple-darwin
//! ```

#![no_std]
// Workspace-wide lints are inherited via `[lints] workspace = true`
// in `Cargo.toml` (missing_docs, unsafe_op_in_unsafe_fn). Crate-
// specific clippy / rustc lints should live here.

// Aerospace-grade guard: never `panic!`/`assert!` inside a function that
// returns `Result` — such paths must return an error instead of crashing
// (see the `boot_key::derive` feature-stub fix). Enforced under
// `cargo clippy` (CI); a regression becomes a hard error.
#![deny(clippy::panic_in_result_fn)]

/// AT command parser. Pure `no_std`, host-tested in `tests/at_tests.rs`.
///
/// NOTE: this is a chip-agnostic parser only — the firmware-side
/// dispatcher (which actually touches the Wi-Fi driver, NVS, GPIO,
/// identity, …) lives in `firmware/esp32-app/src/at_dispatch.rs` so
/// the parser can be exercised on the host without an SoC.
pub mod at;

/// Result type for AT command dispatch — moved from firmware into
/// the core so the pure-logic validators in [`at_validate`] can
/// return it without dragging in firmware-side dependencies.
pub mod at_dispatch_outcome;

/// Minimal JSON / HTML escaping helpers for rendering untrusted
/// operator-controlled strings (SSID, IP, model names, …) into output
/// consumed by another component. `no_std` + `alloc`, host-tested.
pub mod escape;

/// Pure-logic decision helpers for AT command validation. Lives
/// in the core (not firmware) so the security-sensitive rules
/// (length caps, NUL rejection, encoding checks, mode
/// restrictions) can be exercised on the host with hundreds of
/// unit tests against malicious / pathological inputs.
pub mod at_validate;

/// Device-bound XOR-stream obfuscation for the Wi-Fi password (and
/// other medium-sensitivity secrets stored in NVS).
///
/// See the module-level docs for the threat model, wire format,
/// and aerograde properties. Lives in `magent-core` (not in
/// firmware) so the seal / open round-trip can be exercised on
/// the host by `cargo test` without an ESP-IDF toolchain.
pub mod wifi_pass_seal;

/// DBO2: stronger successor to `wifi_pass_seal` (DBO1).
///
/// Same threat model (NVS-dump attacker, no physical access)
/// but adds:
///
///   * **Cipher key stretching** — device_key + nonce are mixed
///     through HKDF-SHA256 to derive a per-entry cipher key.
///     Re-using the same device_key across many entries no
///     longer gives the attacker useful algebraic structure.
///   * **Per-entry MAC** — 16-byte HMAC-SHA256 over (nonce || ciphertext)
///     detects tamper / wrong-key conditions on open.
///   * **Versioned wire format** — `"DBO2:" || hex(nonce) || hex(cipher) || hex(mac)`
///     so the dispatcher can route by prefix.
///
/// Migration from DBO1 is transparent: [`open_sealed_v2`] first
/// tries DBO2, then falls back to DBO1 via the existing
/// [`wifi_pass_seal::open_sealed_bytes`]. Anything older (raw
/// plaintext) is treated as legacy.
pub mod wifi_pass_seal_v2;

/// Boot-time-derived key (BTDK) for sealing `dev_identity`.
///
/// The chicken-and-egg problem with sealing the device identity
/// is that the device identity *is* the seal key for everything
/// else. This module breaks the loop by deriving a per-device
/// key from hardware-unique material that lives *outside* NVS
/// (on ESP32: eFuse BLOCK0 + chip ID). The result is host-
/// testable (caller provides the material); the firmware binds
/// the material to the actual hardware.
pub mod boot_key;

/// Chip-agnostic time synchronisation. See module docs for the
/// authoritative-source / monotonic-drift model, NVS wire format,
/// and the AT surface. Pure no_std + alloc, host-tested in
/// `tests/time_sync_tests.rs`. The firmware-side SNTP supervisor
/// (ESP32) and BLE CTS subscriber (nRF52) are thin wrappers around
/// this module's [`TimeSync::record`] / [`TimeSync::now_unix`].
pub mod time_sync;

// `alloc` is always linked (even under `std`, where it's a re-export of
// `format!` / `vec!` from `alloc` (and `println!` / `dbg!` from `std`
// when std is enabled) are needed across the crate in both `no_std`
// and `std` builds. `#[macro_use] extern crate ...` is the only way
// to bring those macros into scope at the crate root in Rust 2018+
// when the consuming crate has not yet imported them via `use`. The
// `#[macro_use]` attribute here is therefore load-bearing despite
// what the unused_imports lint says.
#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

// ===========================================================================
// Chip-agnostic modules
// ===========================================================================
// These compile on every target (no_std or std, any architecture). They
// implement the ReAct loop, health monitoring, skills, tools, safety, etc.
// ===========================================================================

pub mod agent;
pub mod error;
pub mod skills;
pub mod tools;

// Health modules (work on both embedded and std)
pub mod early_warning;
pub mod health_sensors;
pub mod sleep_manager;
pub mod sports_coach;
pub mod voice_notification;

// Cross-cutting embedded modules — these only need `core`/`alloc` and
// are pure logic, but the historical convention was to gate them behind
// `embedded`. We now gate them behind the chip-family features instead
// so they only compile when a real chip is selected. Pure host tests
// (`--features std`) can still pull them in via the `embedded` alias
// (which expands to `nrf52`) for backward compatibility.
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod config;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod safety;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod power;

#[cfg(any(
    feature = "nrf52",
    feature = "esp32",
    feature = "embedded",
    feature = "std",
    feature = "web3"
))]
pub mod security;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod wear_leveling;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod hardware;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod monitoring;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod ollama;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub mod storage;

// ---------------------------------------------------------------------------
// Communication: link-layer adapter trait + concrete adapters.
// ---------------------------------------------------------------------------
// The parent module is still gated on a chip feature (existing callers
// expect `magent_core::communication::BleClient` only when building for
// firmware). New code that needs just the `LinkAdapter` trait can use
// `magent_core::communication::link` directly via the
// `link_adapters` feature, which is independent of any chip feature.
#[cfg(any(
    feature = "nrf52",
    feature = "esp32",
    feature = "embedded",
    feature = "link_adapters",
))]
pub mod communication;

// Ingress gateway — depends on `web3` (for `Identity` / `SignedMessage`)
// + `link_adapters` (for `LinkAdapter`). Pulling in both ensures the
// gateway only compiles where it can actually be used.
#[cfg(feature = "ingress")]
pub mod ingress;

// Error-recovery strategies and retry manager. no_std-compatible; the retry
// backoff delay hook is optional and installed by a host/firmware layer.
// PATCHED (MicroAgent): this module was previously orphaned (not declared),
// so `RecoveryManager` was dead code. Wiring it in makes the recovery logic
// part of the crate and its unit tests run.
pub mod recovery;

// ===========================================================================
// Host-only modules (desktop simulation / testing)
// ===========================================================================
// These pull in `reqwest`, the `AgentSimulator`, and the full ReAct loop
// runner, so they only make sense on a host OS.
// PATCHED (MicroAgent): excluded on the ESP-IDF *device* target
// (`target_os = "espidf"`), so reqwest/ring never enter a firmware build.
// We deliberately use `not(target_os = "espidf")` rather than
// `not(feature = "esp32")`: a host crate such as `magent-lua` enables the
// `esp32` feature (to get `MiniAgent`) while still building on a host OS,
// and must NOT flip these modules off (that would break host crates like the
// `cli` that import `agent_runner`).
#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod simulator;

#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod agent_runner;

#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod conversation;

#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod summary;

#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod real_tools;

#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod web;

#[cfg(feature = "web3")]
pub mod web3;

#[cfg(feature = "web3_app")]
pub mod web3_app;

// ===========================================================================
// Backwards-compatibility shim for the chip-agnostic HAL
// ===========================================================================
// The HAL trait surface and the host-side nRF52840 simulator used to
// live directly under `magent-core` (`magent_core::hal::*` and
// `magent_core::nrf52_hal::*`). They've moved to a dedicated
// `magent-hal` crate so the chip-agnostic traits can evolve on their
// own and the ~1200-line nRF52840 simulator doesn't bloat the core's
// build graph.
//
// To keep existing user code compiling we re-export everything from
// `magent-hal` under the old paths. New code is encouraged to depend
// on `magent-hal` directly (`use magent_hal::*`).
// ===========================================================================

/// Old name for the chip-agnostic HAL trait surface. New code should
/// `use magent_hal::*` instead — `magent-hal` is the canonical home.
#[cfg(feature = "std")]
pub mod hal {
    pub use magent_hal::*;
}

/// Old name for the host-side nRF52840 simulator. New code should
/// `use magent_hal::nrf52::sim::*` instead.
#[cfg(all(feature = "std", not(target_os = "espidf")))]
pub mod nrf52_hal {
    pub use magent_hal::nrf52::sim::*;
}

// ===========================================================================
// Re-exports of commonly-used chip-agnostic types
// ===========================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub use config::AgentConfig;

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub use security::{EncryptionMode, SecurityLevel, SecurityManager};

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
pub use agent::MiniAgent;

#[cfg(feature = "web3")]
pub use web3::{
    base58_decode, base58_encode, verify_signature, verify_signature_detailed,
    verify_signed_message, verify_signed_message_detailed, DidKey, Identity, PublicKey, SecretKey,
    Signature, SignedMessage,
};

// ===========================================================================
// Compile-time constants
// ===========================================================================

/// mAgent version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum memory budget for agent operations (bytes)
pub const MAX_MEMORY_BUDGET: usize = 50 * 1024;

/// Maximum iteration budget for ReAct loop
pub const MAX_ITERATION_BUDGET: usize = 50;

/// Maximum buffer size for messages (bytes).
///
/// Each conversation message holds up to this many bytes of content. This is
/// deliberately bounded: the embedded `MiniAgent` uses fixed `heapless`
/// buffers, and raising this value would inflate the stack-local
/// `String<MAX_BUFFER_SIZE>` temporaries in `think()`/`get_final_result()`
/// beyond the agent-thread stack. Larger context is obtained instead by
/// holding more *messages* (`MAX_CONVERSATION_MESSAGES`) on the PSRAM heap
/// (the MiniAgent itself is heap-allocated on the ESP32-C61).
pub const MAX_BUFFER_SIZE: usize = 2048;

/// Maximum number of conversation messages retained by the embedded agent.
/// Larger history means the ReAct loop can keep more tool results / turns in
/// context before dropping the oldest. Buffers are on the PSRAM heap.
pub const MAX_CONVERSATION_MESSAGES: usize = 20;

/// Maximum number of concurrent tools
pub const MAX_CONCURRENT_TOOLS: usize = 3;

/// Watchdog timeout in seconds
pub const WATCHDOG_TIMEOUT_SECS: u64 = 10;

/// Stack size for agent task
pub const AGENT_STACK_SIZE: usize = 8192;

/// Stack size for communication task
pub const COMM_STACK_SIZE: usize = 4096;

/// Stack size for storage task
pub const STORAGE_STACK_SIZE: usize = 2048;
