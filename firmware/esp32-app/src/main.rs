//! mAgent firmware for the ESP32-C61-DevKitC-1-N8R2 (N8R2, std).
//!
//! This binary runs the chip-agnostic mAgent core on ESP32-C61 hardware
//! using the `esp-idf-svc` framework. The C61 has 8 MB Flash, 2 MB PSRAM,
//! and a single-core RISC-V RV32IMAC CPU.
//!
//! # Stack choices
//!
//! We use `esp-idf-svc` + `std` (not `esp-hal` + `no_std`) because the
//! N8R2 board provides 2 MB of PSRAM that backs the global `std::alloc`
//! allocator. This gives us enough heap for the ReAct loop, serde_json,
//! and the esp-idf-svc HTTP client without any custom allocator glue.
//!
//! # Thread model
//!
//! Two std threads run concurrently:
//!
//!  - **`agent-thread`** — drives the `MiniAgent` ReAct loop.
//!  - **`ingress-thread`** — monitors UART0 for incoming commands and feeds
//!    them to the `IngressGateway`.
//!
//! # Wi-Fi provisioning
//!
//! Wi-Fi STA credentials are read from NVS at boot. Use `espflash write-nvs`
//! to provision them (see `docs/ESP32.md` for the full procedure).
//!
//! # Building
//!
//! ```sh
//! # Requires: source ~/export-esp.sh (ESP-IDF toolchain)
//! cargo build -p magent-esp32-app --release
//! ```
//!
//! # Flashing
//!
//! ```sh
//! cargo run -p magent-esp32-app --release
//! ```
//!
//! TRACE: REQ-FW-001, REQ-FW-002, REQ-NET-001, REQ-SAFE-001.

mod link_adapters;
mod local_tools;

use core::convert::TryFrom;
use core::time::Duration;
use std::sync::{Arc, Mutex};
use std::thread;

use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use heapless::String as HeaplessString;

use magent_core::{AgentConfig, MiniAgent, VERSION};
use magent_core::{
    ingress::{IngressGateway, IngressMode},
    web3::Identity,
};

use link_adapters::UartAdapter;

/// Shared "current task" handle: the ingress thread writes UART commands
/// here; the agent thread drains the latest one and executes it against real
/// hardware (via `Esp32ToolHandler`).
type TaskHandle = Arc<Mutex<Option<std::string::String>>>;

/// Monotonic time in milliseconds (ESP-IDF `esp_timer`, shared across threads).
fn now_ms() -> u64 {
    unsafe { esp_idf_sys::esp_timer_get_time() as u64 / 1000 }
}

/// Free heap in bytes (internal + PSRAM).
fn free_heap() -> u32 {
    unsafe { esp_idf_sys::esp_get_free_heap_size() }
}

/// Shared heartbeat used to detect a hung (stalled, non-panicking) thread.
///
/// Each worker thread calls [`Heartbeat::beat`] on every loop iteration; the
/// supervisor (main loop) calls [`Heartbeat::stale`] to see if a worker has
/// stopped making progress. `last_ms == 0` means "never beat" (thread may not
/// have started yet), which we don't flag as stale.
#[derive(Clone, Default)]
struct Heartbeat {
    last_ms: Arc<std::sync::atomic::AtomicU32>,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            last_ms: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
    fn beat(&self) {
        self.last_ms
            .store(now_ms() as u32, std::sync::atomic::Ordering::SeqCst);
    }
    fn stale(&self, timeout_ms: u64) -> bool {
        let last = self.last_ms.load(std::sync::atomic::Ordering::SeqCst);
        // `wrapping_sub` tolerates the u32 millisecond counter wrapping
        // (~49.7 days of uptime).
        last != 0 && (now_ms() as u32).wrapping_sub(last) > timeout_ms as u32
    }
}

/// The UART0 peripheral + its TX/RX pins needed by the ingress thread
/// (only present when the `uart` feature is on).
#[cfg(feature = "uart")]
type IngressUartParts = (
    esp_idf_svc::hal::uart::UART0<'static>,
    esp_idf_svc::hal::gpio::Gpio11<'static>, // U0TXD
    esp_idf_svc::hal::gpio::Gpio10<'static>, // U0RXD
);
#[cfg(not(feature = "uart"))]
type IngressUartParts = ((), (), ());

// ---------------------------------------------------------------------------
// Boot diagnostics
// ---------------------------------------------------------------------------

/// Panic handler installed by esp-idf-svc. All panics end here.
///
/// The `esp-idf-svc` panic handler prints the location over UART0 before
/// PATCHED (MicroAgent): Removed the custom `#[panic_handler]` because
/// `esp-idf-svc` with the `std` feature pulls in the Rust standard library,
/// which provides its own panic handler. Having two `panic_impl` lang items
/// causes E0152. We rely on std's panic handler, which calls `abort()` on
/// panic and (via the panic-abort build-std flag we set in `.cargo/config.toml`)
/// triggers the ESP-IDF panic handler that prints diagnostics and reboots.
/// TRACE: REQ-SAFE-001.

// ---------------------------------------------------------------------------
// Global logger
// ---------------------------------------------------------------------------

/// Initialise the esp-idf-svc global logger.
///
/// TRACE: REQ-SAFE-001 — must be called before any other esp-idf-svc
/// code so all modules get the same filter.
fn init_logging() {
    // PATCHED (MicroAgent): `EspLogger::setup()` was removed in master
    // esp-idf-svc; the equivalent is `initialize_default()` which
    // installs the default IDF-side log filter as the global `log`
    // backend. The log level is controlled via sdkconfig's
    // CONFIG_LOG_MAXIMUM_LEVEL.
    EspLogger::initialize_default();
    // PATCHED (MicroAgent): bumped to Debug for boot-path diagnosis.
    log::set_max_level(log::LevelFilter::Debug);
    log::info!("[magent] v{VERSION} booting (esp-idf-svc 0.52 / ESP32-C61 std)");
}

// ---------------------------------------------------------------------------
// NVS helpers
// ---------------------------------------------------------------------------

/// NVS key for the Wi-Fi SSID.
const NVS_KEY_WIFI_SSID: &str = "wifi_ssid";
/// NVS key for the Wi-Fi password.
const NVS_KEY_WIFI_PASS: &str = "wifi_pass";

/// Load a string from NVS. Returns `None` if the key is absent or unreadable.
fn nvs_load_string(key: &str) -> Option<String> {
    // PATCHED (MicroAgent): `EspDefaultNvs::new()` now takes 3 args
    // (partition, namespace, read_write). The default partition is
    // obtained via `EspDefaultNvsPartition::take()` which also
    // initializes the NVS flash if it isn't already.
    let nvs = EspDefaultNvs::new(
        EspDefaultNvsPartition::take().ok()?,
        "magent",
        true,
    )
    .ok()?;
    // PATCHED (MicroAgent): `get_str` now requires an out-buffer for
    // safety (the API previously returned `Option<&str>` borrowing
    // from NVS storage, which had lifetime issues). The buffer is
    // 256 bytes — long enough for an SSID or passkey.
    let mut buf = [0u8; 256];
    nvs.get_str(key, &mut buf).ok().flatten().map(str::to_owned)
}

/// Save a string to NVS. Returns `Ok(())` on success.
fn nvs_save_string(key: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let nvs = EspDefaultNvs::new(
        EspDefaultNvsPartition::take()?,
        "magent",
        true,
    )?;
    nvs.set_str(key, value)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Safety: crash-loop detection (auto-restart recovery)
// ---------------------------------------------------------------------------
// ESP-IDF's panic handler already reboots the board automatically on a crash.
// To recover from a *crash loop* (the board reboots before the app can do any
// useful work), we keep a consecutive-boot counter in NVS. If we see >= 3
// consecutive fast reboots we enter "safe mode" (skip Wi-Fi / risky bring-up)
// so the board can at least boot, log, and serve UART. Once it has been up
// long enough we treat the boot as stable and reset the counter.

/// NVS key for the consecutive-crash boot counter (stored as a decimal string).
const NVS_KEY_BOOT_COUNT: &str = "boot_count";
/// Consecutive reboots before we assume a crash loop.
const CRASH_LOOP_THRESHOLD: u32 = 3;

/// Advance the boot counter. Returns `true` if we've hit the crash-loop
/// threshold and should boot into safe mode.
fn check_and_advance_crash_counter() -> bool {
    let prev = nvs_load_string(NVS_KEY_BOOT_COUNT)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let next = prev + 1;
    // Best-effort persist; if NVS write fails we still return based on `next`.
    let _ = nvs_save_string(NVS_KEY_BOOT_COUNT, &next.to_string());

    if next >= CRASH_LOOP_THRESHOLD {
        log::error!(
            "[safety] consecutive boot #{next} (>= {CRASH_LOOP_THRESHOLD}) — crash loop suspected, safe mode"
        );
        true
    } else {
        log::info!("[safety] boot #{next}");
        false
    }
}

/// Mark the current boot as stable (called after the board has been up a
/// while), resetting the crash-loop counter.
fn mark_stable_boot() {
    let _ = nvs_save_string(NVS_KEY_BOOT_COUNT, &0u32.to_string());
    log::info!("[safety] boot considered stable — crash counter reset");
}

// ---------------------------------------------------------------------------
// Device identity
// ---------------------------------------------------------------------------

/// NVS key for the device's secret seed (32 bytes, hex-encoded).
const NVS_KEY_IDENTITY: &str = "dev_identity";

/// Load a persisted device identity from NVS, or generate a fresh one.
///
/// TRACE: REQ-SAFE-001. The identity is derived from hardware TRNG and
/// persisted to NVS so it survives across reboots.
fn load_or_create_identity() -> Identity {
    // Try to load from NVS first.
    if let Some(hex) = nvs_load_string(NVS_KEY_IDENTITY) {
        if let Ok(seed) = hex::decode(&hex) {
            if seed.len() == 32 {
                if let Ok(id) = Identity::from_secret_bytes(&seed) {
                    log::info!(
                        "[magent] identity loaded from NVS (pubkey={}...)",
                        // PATCHED (MicroAgent): `Identity::public_key()` returns
                        // `&magent_core::web3::identity::PublicKey`,
                        // a tuple struct whose inner array is
                        // private. The wrapper exposes the bytes
                        // via `as_bytes()` (returns `&[u8; 32]`);
                        // we then slice the first 8 bytes for the
                        // diagnostic prefix.
                        hex::encode(&id.public_key().as_bytes()[..8])
                    );
                    return id;
                }
            }
        }
    }

    // Generate a fresh identity from the hardware TRNG.
    log::warn!("[magent] no identity in NVS — generating fresh key from TRNG");
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed)
        .expect("hardware TRNG is required on this platform");

    let id = Identity::from_secret_bytes(&seed)
        .expect("secret bytes are valid Ed25519 seed");

    // Persist to NVS so next boot re-uses the same identity.
    let hex = hex::encode(seed);
    if let Err(e) = nvs_save_string(NVS_KEY_IDENTITY, &hex) {
        log::warn!("[magent] failed to persist identity to NVS: {e} (will regenerate on next boot)");
    } else {
        log::info!("[magent] new identity generated and persisted");
    }

    id
}

// ---------------------------------------------------------------------------
// Wi-Fi
// ---------------------------------------------------------------------------

/// Connect to Wi-Fi STA using credentials passed in (loaded from NVS BEFORE
/// the WiFi subsystem takes ownership of the default NVS partition).
///
/// Blocks until association succeeds or 10 seconds elapse. Panics on timeout.
/// TRACE: REQ-FW-002, REQ-NET-001.
///
/// PATCHED (MicroAgent): the credentials are passed in rather than re-read
/// from NVS here. `EspDefaultNvsPartition::take()` (the `DEFAULT_TAKEN`
/// singleton) is already held by `EspWifi` at this point, so a second
/// `take()` inside this function returns `ESP_ERR_INVALID_STATE` and the
/// WiFi would be silently skipped ("no SSID in NVS").
fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'_>>, ssid: &str, password: &str) {
    if ssid.is_empty() {
        log::warn!("[wifi] no SSID — skipping Wi-Fi");
        return;
    }

    log::info!("[wifi] connecting to SSID={ssid}");

    // PATCHED (MicroAgent): `ClientConfiguration.ssid` is a
    // `heapless::String<32>` (not a plain `String`). We truncate
    // and convert via `TryFrom`. The `password` field is
    // `heapless::String<64>`.
    let cfg = ClientConfiguration {
        ssid: HeaplessString::<32>::try_from(ssid)
            .unwrap_or_else(|_| HeaplessString::try_from("invalid").unwrap()),
        password: HeaplessString::<64>::try_from(password)
            .unwrap_or_default(),
        ..Default::default()
    };

    // PATCHED (MicroAgent): `set_configuration`/`start` can fail at runtime
    // (bad driver state, etc.). Previously `.expect()` here would panic and
    // reboot the whole board. Now we log and bail out of the connect attempt,
    // leaving the firmware running (agent local tools + UART ingress work
    // without network).
    if let Err(e) = wifi.set_configuration(&Configuration::Client(cfg)) {
        log::warn!("[wifi] set_configuration failed: {e}");
        return;
    }
    if let Err(e) = wifi.start() {
        log::warn!("[wifi] start failed: {e}");
        return;
    }

    // PATCHED (MicroAgent): The outer `BlockingWifi` exposes the
    // blocking-style wrapper; to reach the underlying `EspWifi`
    // (which owns the netif handle) we need to drop the wrapper
    // for the duration of the wait. We poll `is_connected` on the
    // wrapper for up to 30s, then read the netif info via
    // `wifi.wifi_mut().sta_netif()`.
    //
    // PATCHED (MicroAgent): on timeout we log a warning and return instead
    // of panicking — a panic here put the board into a reboot loop whenever
    // the AP wasn't reachable within the (too short) window. Real-world
    // association (scan + connect) can take >10s, so we allow 30s and keep
    // running even if the network is unreachable (the agent loop continues;
    // Wi-Fi can be re-tried on a later boot).
    let start = std::time::Instant::now();
    while !wifi.is_connected().unwrap_or(false) {
        if start.elapsed() > Duration::from_secs(30) {
            log::warn!(
                "[wifi] association timed out after 30s — continuing without network"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let ip = wifi
        .wifi_mut()
        .sta_netif()
        .get_ip_info()
        .map(|i| i.ip.to_string())
        .unwrap_or_else(|_| "unknown".into());
    log::info!("[wifi] connected — ip={ip}");
}

/// Read the Wi-Fi credentials from NVS, provisioning them from build-time
/// env vars (`MAGENT_WIFI_SSID` / `MAGENT_WIFI_PASS`) if absent.
///
/// PATCHED (MicroAgent): MUST be called BEFORE `EspWifi` / `EspDefaultNvs`
/// takes ownership of the default NVS partition (i.e. before
/// `EspDefaultNvsPartition::take()` in `main`). After that, a second
/// `take()` fails with `ESP_ERR_INVALID_STATE`, so we load the credentials
/// up-front and pass them into `connect_wifi`.
///
/// Returns `(ssid, password)`.
fn provision_and_load_wifi_credentials() -> (Option<String>, Option<String>) {
    // Provision from build-time env vars if the key is absent. This avoids
    // hard-coding credentials in the source and uses the firmware's own NVS
    // API (so the entries are written in the runtime-compatible format, which
    // an externally generated NVS image may not be).
    if let (Some(ssid), Some(pass)) =
        (option_env!("MAGENT_WIFI_SSID"), option_env!("MAGENT_WIFI_PASS"))
    {
        if nvs_load_string(NVS_KEY_WIFI_SSID).is_none() {
            match nvs_save_string(NVS_KEY_WIFI_SSID, ssid) {
                Ok(()) => log::info!("[wifi] SSID provisioned to NVS"),
                Err(e) => log::warn!("[wifi] failed to persist SSID: {e}"),
            }
        }
        if nvs_load_string(NVS_KEY_WIFI_PASS).is_none() {
            let _ = nvs_save_string(NVS_KEY_WIFI_PASS, pass);
        }
    }

    (
        nvs_load_string(NVS_KEY_WIFI_SSID),
        nvs_load_string(NVS_KEY_WIFI_PASS),
    )
}

// ---------------------------------------------------------------------------
// Ingress thread
// ---------------------------------------------------------------------------

/// Entry point for the `ingress-thread`.
///
/// Runs the `IngressGateway` over UART0 (115 200 8N1). In a real product
/// this thread would also monitor a GPIO button and feed press events
/// into the gateway as `IngressSource::Other`. TRACE: REQ-SAFE-002.
fn run_ingress(
    identity: Identity,
    uart_parts: Option<IngressUartParts>,
    task_handle: TaskHandle,
    reply_outbox: TaskHandle,
    heartbeat: Heartbeat,
) {
    log::info!("[ingress] thread starting");

    // PATCHED (MicroAgent): `UartAdapter` is now generic over the
    // underlying UART driver type (`T: Read + Write`). On the C61
    // we use `esp_idf_svc::hal::uart::UartDriver`. The driver borrows
    // `'static` GPIO pins taken from `Peripherals`, so the gateway's
    // adapter type is `UartDriver<'static>`.
    let mut gw: IngressGateway<UartAdapter<esp_idf_svc::hal::uart::UartDriver<'static>>> =
        IngressGateway::new(IngressMode::Signed);
    gw.set_signer(identity);

    // Build a UART adapter on UART0 (wired to the USB-UART bridge on the
    // C61 DevKit). UART0 pins: TX=GPIO11, RX=GPIO10 (U0TXD/U0RXD). The
    // ingress driver reads the RX pin (GPIO10 — where the host sends bytes)
    // while the console writes logs to the TX pin (GPIO11), so they share the
    // UART0 hardware without a TX→RX feedback loop. If `UartDriver::new` fails
    // (e.g. the UART or pins are already owned by the console), we fall back to
    // dummy mode.
    #[cfg(feature = "uart")]
    {
        use esp_idf_svc::hal::gpio;
        use esp_idf_svc::hal::uart::{self, UartDriver};
        use esp_idf_svc::hal::units::Hertz;

        if let Some((uart0, tx, rx)) = uart_parts {
            let config = uart::config::Config::new().baudrate(Hertz(115_200));
            match UartDriver::new(
                uart0,
                tx,
                rx,
                Option::<gpio::Gpio0>::None,
                Option::<gpio::Gpio1>::None,
                &config,
            ) {
                Ok(u) => {
                    let adapter = UartAdapter::new(u, "UART0");
                    let _ = gw.register(adapter);
                    log::info!("[ingress] UART0 registered");
                }
                Err(e) => {
                    log::warn!("[ingress] UART0 unavailable ({e}) — running in dummy mode");
                }
            }
        } else {
            log::warn!("[ingress] uart feature enabled but no UART0 parts — dummy mode");
        }
    }

    #[cfg(not(feature = "uart"))]
    {
        let _ = uart_parts;
        log::info!("[ingress] uart feature disabled — running in dummy mode");
    }

    log::info!("[ingress] gateway ready");

    loop {
        heartbeat.beat();

        // PATCHED (MicroAgent): drain any agent reply and send it back to the
        // host over the UART link (bidirectional communication). Adapter index
        // 0 is the UART adapter registered below.
        if let Ok(mut guard) = reply_outbox.lock() {
            if let Some(reply) = guard.take() {
                match gw.send_to_adapter(0, reply.as_bytes()) {
                    Ok(()) => log::info!("[ingress] reply sent to host: {reply}"),
                    Err(e) => log::warn!("[ingress] reply send failed: {e}"),
                }
            }
        }

        match gw.ingest() {
            Ok(Some(frame)) => {
                // PATCHED (MicroAgent): log at INFO so received frames are
                // visible with the default CONFIG_LOG_DEFAULT_LEVEL (the
                // previous `log::debug!` was filtered out, making it look
                // like UART ingress never received anything).
                log::info!(
                    "[ingress] frame src={:?} size={}B payload={}",
                    frame.source,
                    frame.payload.len(),
                    hex::encode(&frame.payload)
                );
                if let Some(envelope) = &frame.envelope_json {
                    log::info!("[ingress] signed envelope: {envelope}");
                }
                // Feed the raw payload as a command for the agent (network-free
                // local tool execution). Anything we can decode as UTF-8 text
                // becomes the next task the agent runs.
                if let Ok(text) = core::str::from_utf8(&frame.payload) {
                    let command = text.trim();
                    if !command.is_empty() {
                        if let Ok(mut guard) = task_handle.lock() {
                            log::info!("[ingress] feeding agent command: {command}");
                            *guard = Some(command.to_string());
                        }
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                log::warn!("[ingress] error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent thread
// ---------------------------------------------------------------------------

/// Entry point for the `agent-thread`.
///
/// Drives the `MiniAgent` ReAct loop forever. TRACE: REQ-SAFE-001.
fn run_agent_loop(task_handle: TaskHandle, reply_outbox: TaskHandle, heartbeat: Heartbeat) {
    log::info!("[agent] thread starting");

    let config = AgentConfig::new()
        .with_name("mAgent-ESP32-C61")
        .expect("agent name fits")
        .with_max_iterations(20)
        .expect("iterations in range")
        .with_max_memory(256 * 1024) // 256 KiB — max allowed; PSRAM now enabled (2 MB heap)
        .expect("memory budget in range");

    let mut agent = match MiniAgent::new(config) {
        Ok(a) => a,
        Err(e) => {
            // Config is compile-time constants so this is unreachable in
            // practice, but don't panic the thread if it ever happens.
            log::error!("[agent] MiniAgent::new failed: {e}");
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
    };

    // PATCHED (MicroAgent): install the real-hardware tool handler so
    // `write_gpio` / `read_sensor` drive actual GPIO / the internal
    // temperature sensor instead of returning simulated values. This works
    // with no network at all.
    agent.set_tool_handler(&local_tools::Esp32ToolHandler);

    log::info!("[agent] MiniAgent ready");

    // PATCHED (MicroAgent): log a health line (uptime + free heap) roughly
    // every 60s so an operator can see the agent is alive and how much memory
    // remains (helps catch leaks before they bite). Also warns if free heap is
    // critically low.
    let boot_ms = now_ms();
    let mut last_health_log = boot_ms;
    const LOW_HEAP_WARN: u32 = 64 * 1024;

    loop {
        heartbeat.beat();
        let elapsed_ms = now_ms().saturating_sub(boot_ms);

        // PATCHED (MicroAgent): use the latest command queued by the ingress
        // thread (from UART), defaulting to a temperature read. Track whether
        // the task came from a real UART command so we can reply over UART.
        let pending = task_handle.lock().ok().and_then(|mut g| g.take());
        let from_command = pending.is_some();
        let task = pending.unwrap_or_else(|| "read sensor temperature".to_string());

        // PATCHED (MicroAgent): `MiniAgent::run` is an `async fn` (it
        // awaits the LLM and tool futures). On a bare esp-idf-svc
        // std thread we don't run a Tokio runtime, so we drive the
        // future synchronously via the single-threaded
        // `futures::executor::block_on`. This is fine because
        // MiniAgent itself uses poll-based cooperative scheduling
        // — no thread::spawn is invoked from inside the agent.
        //
        // Reliability: wrap the run in `catch_unwind` so a panic inside a
        // tool handler or the ReAct loop (e.g. a bad GPIO op) does NOT kill
        // the agent thread — we log it and keep serving the next command.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            futures::executor::block_on(agent.run(&task))
        }));
        match outcome {
            Ok(Ok(result)) => {
                log::info!("[agent] result({task}): {result}");
                // Bidirectional: if this task came from a UART command, put
                // the result into the reply outbox; the ingress thread sends
                // it back over the UART link to the host.
                if from_command {
                    if let Ok(mut guard) = reply_outbox.lock() {
                        *guard = Some(std::format!("RESULT[{task}]: {result}"));
                    }
                }
            }
            Ok(Err(e)) => log::error!("[agent] error({task}): {e}"),
            Err(_) => log::error!("[agent] task panicked ({task}) — continuing"),
        }

        // Periodic health metrics + low-memory warning.
        let heap = free_heap();
        if heap < LOW_HEAP_WARN {
            log::warn!("[health] LOW FREE HEAP: {heap} B (uptime {elapsed_ms} ms)");
        }
        if now_ms().saturating_sub(last_health_log) >= 60_000 {
            log::info!(
                "[health] agent alive — uptime {elapsed_ms} ms, free_heap {heap} B, iterations in current task ok"
            );
            last_health_log = now_ms();
        }

        // PATCHED (MicroAgent): poll frequently so UART-fed commands take
        // effect quickly (a 60s sleep made interactive commands feel dead).
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Firmware entry point.
///
///  1. Initialise esp-idf-svc logging.
///  2. Load or generate device identity (persisted to NVS).
///  3. Connect to Wi-Fi STA (NVS credentials).
///  4. Spawn `agent-thread` and `ingress-thread`.
///  5. Join both threads (they run forever).
///
/// TRACE: REQ-FW-001, REQ-FW-002, REQ-NET-001, REQ-SAFE-001.
///
/// ESP-IDF's startup code (in `esp_system`/`start_app`) calls
/// `app_main` (force-linked via `-u app_main`). We re-export
/// `app_main` as a thin wrapper around the standard Rust
/// `main` entry point. The wrapper has `extern "C"` ABI so the
/// C-side startup code (which calls it from the ESP-IDF main
/// task) finds the right symbol with the right calling
/// convention.
#[no_mangle]
pub extern "C" fn app_main() {
    // PATCHED (MicroAgent): removed the `diag_marker` calls that used to run
    // *before* `main()`. They wrote directly to `0x3F400000` (a classic-ESP32
    // UART0 base), which is NOT a valid/peripheral-mapped address on the
    // ESP32-C61 — it caused a "Guru Meditation Error (Store access fault)"
    // at the very first instruction of `app_main`, panicking the firmware
    // before the logger came up. Normal `log::info!` via EspLogger → ESP-IDF
    // console (UART0) already gives us reliable boot visibility, so the raw
    // marker is unnecessary and harmful.
    main();
}

/// Bring up the platform (event loop, peripherals, NVS) and try to connect
/// to Wi-Fi, returning the ingress UART parts.
///
/// Reliability: this is deliberately NON-FATAL. Any subsystem that fails
/// (event loop, peripherals, NVS, `EspWifi`, `BlockingWifi`, association) is
/// logged and skipped, and the function still returns the UART parts so the
/// agent + ingress threads run regardless (local tools + UART don't need
/// network). Previously a single `.expect()` anywhere in this path panicked
/// and rebooted the whole board.
///
/// If `safe_mode` is `true` (crash-loop suspected) we skip the Wi-Fi bring-up
/// entirely so the board boots as fast and risk-free as possible and can at
/// least serve UART + local tools.
fn setup_platform(wifi_ssid: &str, wifi_pass: &str, safe_mode: bool) -> Option<IngressUartParts> {
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::peripherals::Peripherals;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;

    let sysloop = match EspSystemEventLoop::take() {
        Ok(s) => {
            log::info!("[magent] boot phase 3/8: sysloop ready");
            s
        }
        Err(e) => {
            log::warn!("[wifi] event loop unavailable ({e}) — running without Wi-Fi");
            return None;
        }
    };

    let peripherals = match Peripherals::take() {
        Ok(p) => {
            log::info!("[magent] boot phase 4/8: peripherals ready");
            p
        }
        Err(e) => {
            log::warn!("[wifi] peripherals unavailable ({e}) — running without Wi-Fi/UART");
            return None;
        }
    };
    let nvs_partition = match EspDefaultNvsPartition::take() {
        Ok(n) => n,
        Err(e) => {
            log::warn!("[wifi] NVS partition unavailable ({e}) — running without Wi-Fi");
            return None;
        }
    };

    let modem = peripherals.modem;
    // Extract the UART parts before handing `modem` to EspWifi.
    #[cfg(feature = "uart")]
    let ingress_uart = Some((peripherals.uart0, peripherals.pins.gpio11, peripherals.pins.gpio10));
    #[cfg(not(feature = "uart"))]
    let ingress_uart: Option<IngressUartParts> = None;

    // Crash-loop recovery: in safe mode skip Wi-Fi entirely — the board is
    // rebooting repeatedly and Wi-Fi bring-up (with its 30s association wait)
    // is the most likely thing to keep it in the loop. Serve UART + local tools
    // only, so an operator can connect and diagnose.
    if safe_mode {
        log::warn!("[wifi] safe mode — skipping Wi-Fi init (crash-loop recovery)");
        return ingress_uart;
    }

    match EspWifi::new(modem, sysloop.clone(), Some(nvs_partition)) {
        Ok(esp_wifi) => {
            log::info!("[magent] boot phase 5/8: esp_wifi ready");
            match BlockingWifi::wrap(esp_wifi, sysloop) {
                Ok(mut wifi) => {
                    log::info!("[magent] boot phase 6/8: BlockingWifi ready");
                    connect_wifi(&mut wifi, wifi_ssid, wifi_pass);
                }
                Err(e) => log::warn!("[wifi] BlockingWifi::wrap failed: {e}"),
            }
        }
        Err(e) => log::warn!("[wifi] EspWifi::new failed: {e}"),
    }

    ingress_uart
}

fn main() {
    init_logging();
    // Boot-path progress markers. Each phase advances the number so a
    // crash mid-boot leaves a record in the UART ring buffer.
    log::info!("[magent] boot phase 1/8: logger up");

    // Load / generate device identity.
    let identity = load_or_create_identity();
    log::info!("[magent] boot phase 2/8: identity ready");

    // PATCHED (MicroAgent): crash-loop detection. ESP-IDF auto-reboots on a
    // panic; if we're stuck rebooting, this returns true and we boot into
    // safe mode (skip Wi-Fi) so the board stays up long enough to diagnose
    // over UART.
    let safe_mode = check_and_advance_crash_counter();

    // PATCHED (MicroAgent): load (and provision) the Wi-Fi credentials NOW,
    // before the WiFi subsystem takes ownership of the default NVS partition.
    // (See `provision_and_load_wifi_credentials` for why this must be early.)
    let (wifi_ssid, wifi_pass) = provision_and_load_wifi_credentials();

    // PATCHED (MicroAgent): platform bring-up (event loop, peripherals, NVS,
    // Wi-Fi) is now NON-FATAL — see `setup_platform`. If Wi-Fi can't be set up
    // the firmware still boots and runs the agent/ingress threads (local tools
    // + UART don't need network). Returns the UART parts for the ingress thread.
    let ingress_uart = setup_platform(
        wifi_ssid.as_deref().unwrap_or(""),
        wifi_pass.as_deref().unwrap_or(""),
        safe_mode,
    );
    log::info!("[magent] boot phase 7/8: platform ready");

    // Shared identity + task handle for the threads.
    let identity_clone = identity.clone();
    let task_handle: TaskHandle = Arc::new(Mutex::new(None));
    let agent_task = task_handle.clone();
    let ingress_task = task_handle.clone();
    // PATCHED (MicroAgent): reply outbox — the agent writes results here and
    // the ingress thread sends them back over UART to the host.
    let reply_outbox: TaskHandle = Arc::new(Mutex::new(None));
    let agent_reply = reply_outbox.clone();
    let ingress_reply = reply_outbox.clone();

    // PATCHED (MicroAgent): heartbeats for hang detection — the supervisor
    // (busy-loop) flags a worker as hung if it stops beating.
    let agent_hb = Heartbeat::new();
    let ingress_hb = Heartbeat::new();
    let agent_hb_for_thread = agent_hb.clone();
    let ingress_hb_for_thread = ingress_hb.clone();

    // PATCHED (MicroAgent): thread spawns are non-fatal — a failure is logged
    // and the firmware keeps running (the busy-loop below feeds the HW
    // watchdog). Previously a `.expect()` panicked and rebooted the board.
    let agent_handle = thread::Builder::new()
        .name("agent-thread".into())
        .stack_size(48 * 1024)
        .spawn(move || run_agent_loop(agent_task, agent_reply, agent_hb_for_thread))
        .ok();
    if agent_handle.is_none() {
        log::error!("[agent] thread spawn failed — continuing without agent");
    }

    let ingress_handle = thread::Builder::new()
        .name("ingress-thread".into())
        .stack_size(24 * 1024)
        .spawn(move || {
            run_ingress(identity_clone, ingress_uart, ingress_task, ingress_reply, ingress_hb_for_thread)
        })
        .ok();
    if ingress_handle.is_none() {
        log::error!("[ingress] thread spawn failed — continuing without ingress");
    }

    log::info!("[magent] all threads running");
    log::info!("[magent] boot phase 8/8: all systems nominal");

    // PATCHED (MicroAgent): explicit busy-loop instead of `join()`.
    // The previous code used `agent_handle.join()` which blocks on
    // a FreeRTOS `taskNotify` and ends up in the ROM WFI loop. The
    // main task's WFI doesn't feed the Timer Group 0 hardware
    // watchdog (which is independent of the FreeRTOS task WDT we
    // disabled), so after ~5 seconds the chip resets with
    // `rst:0x7 (TG0_WDT_HPSYS)`. The fixed loop calls `sleep` which
    // yields to the scheduler but doesn't enter WFI, keeping the
    // main task active enough to feed the HW WDT.
    // PATCHED (MicroAgent): once the board has been up long enough (stable
    // boot), reset the crash-loop counter so a single transient crash later
    // doesn't trigger safe mode.
    let boot_start = std::time::Instant::now();
    let mut stable_marked = false;

    loop {
        std::thread::sleep(Duration::from_secs(1));
        if !stable_marked && boot_start.elapsed() > Duration::from_secs(60) {
            mark_stable_boot();
            stable_marked = true;
        }
        // Detect thread panics/exits so we can report them (rather than
        // silently running with a dead worker).
        if let Some(h) = &agent_handle {
            if h.is_finished() {
                log::error!("[magent] agent-thread exited unexpectedly");
            }
        }
        if let Some(h) = &ingress_handle {
            if h.is_finished() {
                log::error!("[magent] ingress-thread exited unexpectedly");
            }
        }
        // PATCHED (MicroAgent): heartbeat-based hang detection — a thread that
        // hasn't beaten for >15s is stalled (not crashed), which a panic
        // handler wouldn't catch. We log loudly so an operator / watchdog can
        // act.
        if agent_hb.stale(15_000) {
            log::error!("[health] agent-thread heartbeat stale (>15s) — possible hang");
        }
        if ingress_hb.stale(15_000) {
            log::error!("[health] ingress-thread heartbeat stale (>15s) — possible hang");
        }
    }
}
