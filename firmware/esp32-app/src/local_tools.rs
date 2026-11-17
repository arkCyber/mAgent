//! Real-hardware local tool executor for the ESP32 firmware.
//!
//! Implements [`magent_core::tools::ToolHandler`] so the agent's built-in
//! tools (`write_gpio`, `read_sensor`, ...) actually drive the ESP32-C61
//! hardware instead of returning simulated values — no network required.
//!
//! Tools we don't cover return `None`. `ToolRegistry::execute` treats `None`
//! on a handler-installed registry as "this target cannot run that tool" and
//! reports the failure — it does **not** fall back to the built-in simulation,
//! which would answer `flash_write` with "Wrote N bytes" without persisting
//! anything and `ble_send` with "Sent N bytes" on a firmware that has no BLE
//! stack at all.
//!
//! The agent's registry is populated by `register_builtin_tools`, which
//! registers the whole built-in set (including those two). Tools this handler
//! does not drive therefore reach the LLM as "not implemented on this target"
//! rather than as invented success.

use esp_idf_sys as sys;
use magent_core::agent::{ToolCall, ToolResult};
use magent_core::tools::ToolHandler;

/// Truncate a `std::string::String` into the fixed `heapless::String<256>`
/// field that `ToolResult::data` uses.
fn into_result_data(s: std::string::String) -> heapless::String<256> {
    let mut out = heapless::String::<256>::new();
    // `push_str_truncated` cuts on a character boundary. The old
    // `from_utf8(&bytes[..255]).unwrap_or("?")` turned a Chinese preview whose
    // 255-byte cut landed mid-codepoint into a lone "?", discarding the whole
    // reading.
    crate::text_util::push_str_truncated(&s, &mut out);
    out
}

/// Real-hardware executor installed into the agent's `ToolRegistry`.
#[derive(Debug, Default)]
pub struct Esp32ToolHandler;

fn tool_result(tool: &str, data: std::string::String, success: bool) -> ToolResult {
    ToolResult {
        tool_name: heapless::String::<32>::try_from(tool).unwrap_or_else(|_| heapless::String::new()),
        data: into_result_data(data),
        success,
        error: None,
    }
}

/// Parse a `key=value,key=value` argument string.
fn parse_kv(args: &str) -> Vec<(&str, &str)> {
    args.split(',')
        .filter_map(|part| {
            let t = part.trim();
            t.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
        })
        .collect()
}

fn kv<'a>(pairs: &[(&'a str, &'a str)], key: &str, default: &'a str) -> &'a str {
    for &(k, v) in pairs {
        if k == key && !v.is_empty() {
            return v;
        }
    }
    default
}

/// Like [`kv`], but with no default: `None` means the argument was omitted or
/// empty. Used where a fallback would be a *side effect* (driving a pin,
/// writing a flash address) rather than a harmless display choice — the core
/// simulator treats the same inputs as `MissingField` errors.
fn required_kv<'a>(pairs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, v)| *k == key && !v.is_empty())
        .map(|(_, v)| *v)
}

/// Like [`required_kv`], but an explicitly empty value counts as *present*.
///
/// For arguments where emptiness is a meaningful value rather than a typo:
/// `flash_write`'s `data=` is a request to write zero bytes, and the Lua
/// `hardware.flash_write` binding allows it. `required_kv` treated `data=`
/// the same as a missing `data`, so "set this key to empty" was refused with
/// "missing required argument". A *missing* key is still `None`.
fn kv_allowing_empty<'a>(pairs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}

/// Why `pin` must not be driven as a user GPIO on this board, or `None`.
///
/// The tool/Lua APIs take a **pin number chosen by the model or the script**.
/// ESP-IDF's `gpio_*` calls reject only *out-of-range* numbers; a valid number
/// that already belongs to the flash, the PSRAM, or a peripheral this firmware
/// owns passes straight through and gets reconfigured. Driving the SPI-flash
/// pins is the worst case: `gpio_reset_pin(26..=32)` detaches the SPI0/1
/// function, and the next instruction/data fetch from flash on *any* core
/// faults — a `StoreProhibited` reported on whichever core fetches first,
/// often Core 0 running the Wi-Fi/lwIP tasks, with the actual cause on the
/// thread that touched the pin. Both callers (`write_gpio` here and the Lua
/// `hardware.gpio_*` / `pwm_set` / `adc_read` bindings) refuse these pins so
/// the failure is a reported error instead of a crash.
///
/// `GPIO33..=37` are reserved only when the **octal-PSRAM** profile
/// (`s3-8mb-psram`) is compiled in; on the quad-PSRAM dev board they are
/// ordinary GPIOs. The map follows the memory profile, not `board-s3` alone.
///
/// Out-of-range and nonexistent numbers (the S3 has no GPIO22..=25) are left to
/// ESP-IDF, which already returns `ESP_ERR_INVALID_ARG` for them.
#[cfg(feature = "board-s3")]
pub(crate) fn reserved_pin_reason(pin: i32) -> Option<&'static str> {
    match pin {
        // SPI0/1 to the in-package flash.
        26..=32 => Some("SPI flash pin (GPIO26-32) — driving it detaches the flash"),
        // Octal-PSRAM (N8R8) modules route the PSRAM over GPIO33..=37. They
        // are plain GPIOs on the quad-PSRAM dev board, so the refusal is
        // gated on the memory profile rather than `board-s3` alone. Driving
        // one on an octal build breaks the PSRAM bus just like a flash pin.
        #[cfg(feature = "s3-8mb-psram")]
        33..=37 => Some("octal-PSRAM pin (GPIO33-37) — driving it breaks the PSRAM bus"),
        // Native USB-Serial/JTAG console, claimed by the ingress adapter.
        19 | 20 => Some("USB-Serial/JTAG D-/D+ — owned by the USB console ingress"),
        // UART0, claimed by the UART ingress adapter.
        10 | 11 => Some("UART0 RX/TX — owned by the UART ingress"),
        // I2C0, this firmware's Lua i2c bus.
        4 | 5 => Some("I2C0 SCL/SDA — owned by hardware.i2c_*"),
        _ => None,
    }
}

/// Ask the GPIO matrix whether this pin is already owned by a peripheral.
///
/// The C61 board's pin map is not recorded in this repo, and copying the S3
/// table would be actively wrong (different flash/PSRAM assignment). But the
/// question this function has to answer — "has the system already claimed
/// this pin?" — is one ESP-IDF can answer at runtime, and more accurately
/// than any table: `gpio_get_io_config` reports the current IOMUX function
/// selection, and `FUNC_GPIO` (= 1 on every ESP32-family part) is the plain
/// "this is a GPIO" setting. Anything else means a peripheral owns the pad —
/// SPI flash, PSRAM, UART0, USB-JTAG — and reconfiguring it detaches that
/// peripheral, which on the flash pins is an immediate `StoreProhibited`.
///
/// This also covers pins the S3 table cannot know about, and it stays correct
/// if the board's pin map changes.
#[cfg(not(feature = "board-s3"))]
pub(crate) fn reserved_pin_reason(pin: i32) -> Option<&'static str> {
    /// IOMUX function selector for "plain GPIO".
    const FUNC_GPIO: u32 = 1;

    let Ok(pin) = u32::try_from(pin) else {
        return Some("negative pin number");
    };
    let mut config = core::mem::MaybeUninit::<esp_idf_sys::gpio_io_config_t>::uninit();
    // SAFETY: `gpio_get_io_config` writes the struct and returns an error
    // code; the struct is only read when the call reported `ESP_OK`.
    let rc = unsafe {
        esp_idf_sys::gpio_get_io_config(pin as esp_idf_sys::gpio_num_t, config.as_mut_ptr())
    };
    if rc != esp_idf_sys::ESP_OK {
        return Some("pin number is out of range for this chip");
    }
    // SAFETY: initialised by the successful call above.
    let config = unsafe { config.assume_init() };
    if config.fun_sel != FUNC_GPIO || config.oe_ctrl_by_periph {
        return Some(
            "pin is owned by a system peripheral (flash/PSRAM/UART/USB) — \
             driving it detaches that peripheral",
        );
    }
    // Defence in depth: a pin can read as plain-GPIO while the peripheral
    // that owns it is between initialisation steps, and driving a flash pin
    // is an immediate crash rather than an error.
    //
    // Taken from ESP-IDF's own definition for this part
    // (`components/soc/esp32c61/include/soc/spi_pins.h`), not from memory:
    // MSPI CS1=14, CS0=15, MISO=16, WP=17, HD=19, CLK=20, MOSI=21. Note that
    // 18 is *not* in the set — a "16..=21" range would have wrongly refused it.
    match pin {
        14..=17 | 19..=21 => {
            Some("SPI flash pin (GPIO14-17,19-21) — driving it detaches the flash")
        }
        // UART0, claimed by the ingress adapter.
        10 | 11 => Some("UART0 RX/TX — owned by the UART ingress"),
        _ => None,
    }
}

/// `write_gpio pin=N,state=high|low` — drive a real GPIO on the C61.
fn write_gpio(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    // An omitted or unparseable `pin` used to fall back to GPIO13, so a
    // malformed model call silently drove a real pin. The core simulator
    // distinguishes "missing" from "invalid" and refuses both; this path does
    // the same instead of substituting a value.
    let pin = match required_kv(&pairs, "pin") {
        Some(raw) => match raw.parse::<i32>() {
            Ok(pin) => pin,
            Err(_) => {
                return tool_result(
                    "write_gpio",
                    std::format!("invalid pin: {raw} (expected an integer)"),
                    false,
                )
            }
        },
        None => {
            return tool_result(
                "write_gpio",
                "missing required argument: pin".to_string(),
                false,
            )
        }
    };
    let state = kv(&pairs, "state", "high").to_ascii_lowercase();
    // Only the documented tokens are accepted. The old code treated anything
    // that was not high/on as low, so `state=wobble` drove the pin low.
    let level: u32 = match state.as_str() {
        "high" | "on" | "1" => 1,
        "low" | "off" | "0" => 0,
        other => {
            return tool_result(
                "write_gpio",
                std::format!("expected state=high|low|1|0, got {other}"),
                false,
            )
        }
    };

    if let Some(reason) = reserved_pin_reason(pin) {
        return tool_result(
            "write_gpio",
            std::format!("refusing GPIO{pin}: {reason}"),
            false,
        );
    }

    let mut last;
    unsafe {
        last = sys::gpio_reset_pin(pin);
        if last == 0 {
            last = sys::gpio_set_direction(pin, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        }
        if last == 0 {
            last = sys::gpio_set_level(pin, level);
        }
    }

    let ok = last == 0;
    let data = std::format!(
        "GPIO{pin} set to {} (err={last})",
        if level == 1 { "high" } else { "low" }
    );
    tool_result("write_gpio", data, ok)
}

/// `read_sensor sensor=temperature|memory` — real hardware readings.
///
/// `temperature` reads the chip's internal temperature sensor; `memory` (and
/// anything else) reports the free heap, which changes over time. This gives
/// the agent a real, network-free sensor/status reading.
fn read_sensor(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    let sensor = kv(&pairs, "sensor", "temperature").to_ascii_lowercase();

    if sensor.contains("temp") || sensor.contains("die") {
        // Internal temperature sensor (ESP32-C6/C61).
        //
        // HARDENING (audit-2026-08 H2): the previous code used
        // `core::mem::zeroed()` to build this config struct. That
        // works for `temperature_sensor_config_t` today because every
        // field is a primitive (range_min, range_max, …), but
        // `zeroed` is UB if any future driver version adds a pointer
        // or `NonNull` field that the driver later dereferences —
        // NULL-deref at first read. We now construct the struct
        // explicitly with only the fields the driver documents as
        // user-settable, so a struct bump never produces surprise
        // NULLs.
        let cfg = sys::temperature_sensor_config_t {
            range_min: -10,
            range_max: 80,
            // HARDENING (audit-2026-08 H2): the C driver defaults
            // `clk_src` to `TEMPERATURE_SENSOR_CLK_SRC_DEFAULT` (0)
            // and `flags.allow_pd` to 0 — we set them explicitly so
            // a future bindgen bump that adds a new field doesn't
            // leave it uninitialised. The previous `mem::zeroed()`
            // would have silently zeroed a pointer field if such a
            // field was added (NULL deref at first read).
            clk_src: sys::soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
            flags: sys::temperature_sensor_config_t__bindgen_ty_1 { allow_pd: 0 },
        };
        let mut handle: sys::temperature_sensor_handle_t = core::ptr::null_mut();
        let mut celsius: f32 = 0.0;
        let last = unsafe {
            let mut e = sys::temperature_sensor_install(&cfg, &mut handle);
            if e == 0 {
                e = sys::temperature_sensor_enable(handle);
            }
            if e == 0 {
                // Capture `get_celsius`'s `esp_err_t` too. Discarding it meant a
                // failed read still reported `success:true` with `celsius` at
                // its 0.0 initialiser — a reading the sensor never produced.
                e = sys::temperature_sensor_get_celsius(handle, &mut celsius);
            }
            let _ = sys::temperature_sensor_disable(handle);
            let _ = sys::temperature_sensor_uninstall(handle);
            e
        };
        let ok = last == 0;
        let data = if ok {
            std::format!("temperature={celsius:.1} C (err={last})")
        } else {
            // Never hand back a value we did not obtain.
            std::format!("temperature read failed (err={last})")
        };
        tool_result("read_sensor", data, ok)
    } else if sensor.contains("mem") || sensor.contains("heap") || sensor.contains("sram") {
        // Free heap — a real, changing memory status.
        let free = unsafe { sys::esp_get_free_heap_size() };
        let min = unsafe { sys::esp_get_minimum_free_heap_size() };
        let data = std::format!("free_heap={free} B min_free_heap={min} B");
        tool_result("read_sensor", data, true)
    } else {
        // Unknown sensor (e.g. battery, glucose on hardware that has none).
        // Report "unsupported" honestly instead of silently returning heap
        // data that would look like a real reading.
        let data = std::format!("unsupported sensor: {sensor}");
        tool_result("read_sensor", data, false)
    }
}

/// Parse a flash address, accepting `0x`-prefixed hex or plain decimal.
fn parse_addr(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse::<u32>().ok(),
    }
}

/// `flash_write address=N,data=...` — persist bytes so they survive a reboot.
///
/// The ESP32 has no user-writable memory-mapped NOR, so the shared
/// [`crate::nvs_flash`] store maps the address onto an NVS blob. The same store
/// backs the Lua `hardware.flash_*` API, so a value the agent writes is
/// readable from an app (and vice versa). Distinct addresses never collide;
/// writing an address twice replaces the blob.
fn flash_write(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    // Address 0 is a real, writable key in the NVS-backed store, so an
    // omitted/malformed `address` must not silently write there.
    let addr_str = match required_kv(&pairs, "address") {
        Some(raw) => raw,
        None => {
            return tool_result(
                "flash_write",
                "missing required argument: address".to_string(),
                false,
            )
        }
    };
    let Some(addr) = parse_addr(addr_str) else {
        return tool_result(
            "flash_write",
            std::format!("bad address: {addr_str}"),
            false,
        );
    };
    // An omitted `data` is a missing field, not a successful write of zero
    // bytes: `kv(.., "data", "")` made a typo'd key ("data_") look like a
    // persisted value, and the model was told the write had happened.
    //
    // An *explicitly empty* `data=`, on the other hand, is a real request to
    // write nothing, and the Lua binding (`hardware.flash_write`) accepts it.
    // Conflating the two refused a legitimate "set this key to empty".
    let data = match kv_allowing_empty(&pairs, "data") {
        Some(d) => d,
        None => {
            return tool_result(
                "flash_write",
                "missing required argument: data".to_string(),
                false,
            )
        }
    };
    match crate::nvs_flash::write(addr, data.as_bytes()) {
        Ok(()) => tool_result(
            "flash_write",
            std::format!("Wrote {} bytes to address {addr}", data.len()),
            true,
        ),
        Err(e) => tool_result("flash_write", e, false),
    }
}

/// `flash_read address=N,length=M` — read back up to `M` bytes from `N`.
///
/// Errors when nothing was ever written there, rather than reporting an empty
/// read as success.
fn flash_read(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    // See `flash_write`: a missing address used to read key 0.
    let addr_str = match required_kv(&pairs, "address") {
        Some(raw) => raw,
        None => {
            return tool_result(
                "flash_read",
                "missing required argument: address".to_string(),
                false,
            )
        }
    };
    let Some(addr) = parse_addr(addr_str) else {
        return tool_result("flash_read", std::format!("bad address: {addr_str}"), false);
    };
    let len: usize = kv(&pairs, "length", "256").parse().unwrap_or(256);
    match crate::nvs_flash::read(addr, len) {
        Ok(bytes) => {
            // The stored blob is arbitrary bytes but the result is a text
            // field that ends up on the UART/AT surface, so control bytes
            // (NUL, CR/LF, ANSI escapes) become spaces rather than being
            // replayed into the framing layer.
            let text: std::string::String = std::string::String::from_utf8_lossy(&bytes)
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            tool_result(
                "flash_read",
                std::format!("Read {} bytes from address {addr}: {text}", bytes.len()),
                true,
            )
        }
        Err(e) => tool_result("flash_read", e, false),
    }
}

/// Maximum bytes of a fetched body kept for a text preview.
///
/// PATCHED (MicroAgent): raised 256 → 1024 so a weather one-liner and a
/// news/JSON digest survive intact. It stays bounded because the preview is
/// copied into the agent's conversation (8 KiB per message) and echoed in the
/// `RESULT[...]` reply on the serial link.
const HTTP_PREVIEW_MAX: usize = 1024;

/// Bounded HTTPS GET shared by `fetch_web` / `get_weather` / `get_news`.
///
/// Mirrors `AT+HTTPGET`'s robustness so a black-holed host can never stall the
/// real-time agent thread: DNS + TCP preflight with a 5 s connect timeout
/// (fails fast instead of burning the TLS timeout), CA-bundle verification,
/// 6 s request timeout and a bounded read. Redirects are followed manually with
/// the SSRF guard re-run on every hop (see [`http_get_following_redirects`]).
/// Returns `(status, body)` or a human-readable failure reason.
///
/// PATCHED (MicroAgent): **one bounded retry**. A phone-hotspot uplink drops
/// the occasional TLS handshake (`E esp-tls-mbedtls: mbedtls_ssl_handshake
/// returned -0x0050` → `ESP_ERR_HTTP_CONNECT`, ~1 attempt in 4 observed on real
/// hardware) and the same request succeeds immediately afterwards — without a
/// retry, a single flake fails the whole agent task ("Task: Tool error: …").
/// The retry is capped at one extra attempt so a genuinely unreachable host
/// still fails within ~2× the single-shot budget.
fn http_get_bounded(
    url: &str,
    max: usize,
    accept: &str,
) -> Result<(u16, std::string::String), std::string::String> {
    http_get_bounded_headers(url, max, accept, &[])
}

/// [`http_get_bounded`] with additional request headers (e.g. Kiwi's `apikey`).
///
/// Entries with an empty header name are ignored, so callers can pass a fixed
/// array and select "no extra headers" with a placeholder.
fn http_get_bounded_headers(
    url: &str,
    max: usize,
    accept: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(u16, std::string::String), std::string::String> {
    const ATTEMPTS: usize = 2;
    let mut last_err = std::string::String::from("no attempt made");
    for attempt in 1..=ATTEMPTS {
        crate::rt_watchdog::feed();
        match http_get_following_redirects(url, max, accept, extra_headers) {
            Ok(v) => return Ok(v),
            Err(e) => {
                log::warn!("[tool] http GET {url} failed (attempt {attempt}/{ATTEMPTS}): {e}");
                last_err = e;
                if attempt < ATTEMPTS {
                    std::thread::sleep(core::time::Duration::from_millis(300));
                }
            }
        }
    }
    Err(last_err)
}

/// Redirect hops followed per fetch, on top of the initial request.
///
/// The host tools allow five (`magent_core::web`); the on-device budget is
/// tighter because every hop is a fresh DNS + TCP + TLS handshake on the
/// real-time thread, and the whole chain runs inside the two-attempt retry.
const MAX_REDIRECT_HOPS: usize = 3;

/// [`http_get_once`] with manual, guard-checked redirect following.
///
/// The esp-idf HTTP client would follow GET redirects by itself
/// (`FollowRedirectsPolicy::FollowGetHead`, the default) — but it does so inside
/// C and the wrapper exposes **no** post-redirect URL, so the SSRF guard in
/// [`http_get_once`] would only ever see the *first* host. A model-supplied
/// public URL could then 302 the agent straight onto the device's own
/// web-admin port, a LAN service or the metadata endpoint, echoing the body
/// back out — the exact hole the guard exists to close. So `http_get_once`
/// turns the built-in following **off** (`FollowNone`) and we walk the chain
/// here: each hop is a fresh `http_get_once` (re-running the guard, DNS and TCP
/// preflight on the new host), and the next URL comes from
/// [`magent_core::at_validate::resolve_http_redirect`], which refuses any
/// `Location` it cannot re-vet. Mirrors the host-side post-redirect
/// re-validation in `magent_core::web`.
fn http_get_following_redirects(
    url: &str,
    max: usize,
    accept: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(u16, std::string::String), std::string::String> {
    let mut current = std::string::String::from(url);
    for _ in 0..=MAX_REDIRECT_HOPS {
        let (status, location, body) = http_get_once(&current, max, accept, extra_headers)?;
        // Only the five statuses that carry a `Location` are redirects; 300 and
        // 304 land here too and must be returned as-is.
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok((status, body));
        }
        match location
            .as_deref()
            .and_then(|loc| magent_core::at_validate::resolve_http_redirect(&current, loc))
        {
            Some(next) => current = next,
            // No usable `Location` (or one we decline to re-vet): hand the 3xx
            // back rather than follow blind.
            None => return Ok((status, body)),
        }
    }
    Err(format!(
        "too many redirects (> {MAX_REDIRECT_HOPS}) for {url}"
    ))
}

/// Single-shot half of [`http_get_bounded`] (see it for the contract). Returns
/// `(status, location, body)` — the `Location` header is surfaced for
/// [`http_get_following_redirects`] because the client's own following is off.
fn http_get_once(
    url: &str,
    max: usize,
    accept: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(u16, Option<std::string::String>, std::string::String), std::string::String> {
    use embedded_svc::http::client::Client as HttpClient;
    use embedded_svc::http::Method;
    use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
    use std::net::ToSocketAddrs;

    // P3: this tool does blocking DNS/TCP/TLS (up to ~11s) on the real-time
    // agent thread, so re-feed the RT watchdog at entry to reset the timer
    // before the long hop (no-op on the C61).
    crate::rt_watchdog::feed();

    let (host, port) = match crate::at_dispatch::url_host_port(url) {
        Some(hp) => hp,
        None => return Err("malformed URL".to_string()),
    };
    // SSRF guard shared with the host web tools. `fetch_web`'s URL is
    // model-generated and the fetched text can inject further instructions, so
    // the agent must not be able to read the device's own HTTP server, a LAN
    // service, or the cloud-metadata endpoint. `AT+HTTPGET` deliberately keeps
    // unrestricted reach — its URL is typed by the operator. Run on **every**
    // hop: `http_get_following_redirects` calls this function once per
    // redirect, so a public host cannot 302 the agent onto an internal one.
    if let Some(reason) = magent_core::at_validate::public_fetch_target_refusal(&host, false) {
        return Err(format!("{reason}: {host}"));
    }
    let authority = format!("{host}:{port}");
    // The lexical guard above cannot see a *hostname* that resolves inward:
    // `http://127.0.0.1.nip.io/` is not an IP literal, so it passed, resolved
    // to loopback, and the preflight happily connected to the device's own
    // services. Vet every resolved address too. (DNS rebinding between this
    // check and the client's own resolution is still theoretically possible;
    // esp-idf's HTTP client exposes no pinned-address hook.)
    match authority.to_socket_addrs() {
        Ok(addrs) => {
            let mut first: Option<std::net::SocketAddr> = None;
            for addr in addrs {
                let ip = addr.ip().to_string();
                if let Some(reason) =
                    magent_core::at_validate::public_fetch_target_refusal(&ip, false)
                {
                    return Err(format!("{reason}: {host} resolved to {ip}"));
                }
                if first.is_none() {
                    first = Some(addr);
                }
            }
            match first {
                Some(addr) => {
                    if std::net::TcpStream::connect_timeout(
                        &addr,
                        core::time::Duration::from_secs(5),
                    )
                    .is_err()
                    {
                        return Err(format!("connect timeout to {authority}"));
                    }
                }
                None => return Err(format!("no address for {authority}")),
            }
        }
        Err(e) => return Err(format!("dns failed: {e}")),
    }

    let cfg = HttpConfig {
        timeout: Some(core::time::Duration::from_secs(6)),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        // Follow redirects in Rust, not in C: the built-in following hides the
        // post-redirect URL, so the SSRF guard above could not re-vet it. See
        // `http_get_following_redirects`.
        follow_redirects_policy: esp_idf_svc::http::client::FollowRedirectsPolicy::FollowNone,
        ..Default::default()
    };
    let conn = EspHttpConnection::new(&cfg).map_err(|e| format!("connect: {e}"))?;
    let mut client = HttpClient::wrap(conn);
    // `accept` first, then the caller's extra headers (empty names skipped).
    let mut hs: [(&str, &str); 4] = [("accept", accept), ("", ""), ("", ""), ("", "")];
    let mut n = 1usize;
    for h in extra_headers.iter().take(3) {
        if !h.0.is_empty() {
            hs[n] = *h;
            n += 1;
        }
    }
    let request = client
        .request(Method::Get, url, &hs[..n])
        .map_err(|e| format!("request: {e}"))?;
    // The DNS + TCP preflight above and the TLS handshake inside `submit()`
    // together can approach the supervisor timeout; feed before each long hop.
    crate::rt_watchdog::feed();
    let mut response = request.submit().map_err(|e| format!("submit: {e}"))?;
    let status = response.status();
    // Capture `Location` before the body read (and before `response` drops) for
    // `http_get_following_redirects`; harmless on a non-3xx.
    let location = response.header("location").map(std::string::String::from);

    let mut buf = vec![0u8; max];
    let mut read = 0usize;
    let mut read_err: Option<std::string::String> = None;
    while read < buf.len() {
        // P3: re-feed the RT watchdog on every chunk. Feeding only at entry (as
        // the original `fetch_web` did) is only safe while the whole fetch stays
        // inside the 18 s supervisor timeout — a slow TLS body read (observed
        // ~24 s for a 4 KiB JSON digest over a phone hotspot) would otherwise
        // look like an agent-thread hang and trip the watchdog.
        crate::rt_watchdog::feed();
        match response.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            // A mid-body read error is NOT end-of-body. Treating it as `break`
            // (as this did) returned the truncated prefix as a 2xx body, so
            // `fetch_web`/`get_weather` reported `success=true` with a byte
            // count for data the server never fully delivered.
            Err(e) => {
                read_err = Some(format!("read: {e}"));
                break;
            }
        }
    }
    if let Some(e) = read_err {
        return Err(format!("{e} after {read} B"));
    }
    let body = std::string::String::from_utf8_lossy(&buf[..read]).into_owned();
    Ok((status, location, body))
}

/// Percent-encode a **whole URL** for `esp_http_client`.
///
/// Unlike [`encode_url_segment`] this keeps URL structure intact
/// (`:/?#[]@!$&'()*+,;=` stay literal) and encodes only what the client
/// rejects: non-ASCII bytes (a Chinese query string is what the LLM emits for a
/// Chinese question — `Error parse url https://www.google.com/search?q=上海…`
/// → `ESP_ERR_INVALID_ARG`), spaces, control characters and the few ASCII
/// characters that must never appear raw in a URL. A `%` is left alone so an
/// already-encoded URL is not double-encoded.
fn encode_url_preserving_reserved(s: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        let keep = b.is_ascii_graphic()
            && !matches!(b, b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'}' | b'|' | b'%');
        if keep {
            out.push(b as char);
        } else if b == b'%' {
            out.push('%'); // preserve existing escapes (no double-encoding)
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// fetch_web url=<http(s)://...> - bounded HTTP GET that returns a short
/// plain-text preview of the page body, mirroring AT+HTTPGET robustness
/// (DNS/TCP preflight + hard timeout + bounded read) so a hung TLS
/// handshake cannot stall the agent thread.
fn fetch_web(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    let url = kv(&pairs, "url", "").trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return tool_result("fetch_web", format!("refusing non-http(s) URL: {url}"), false);
    }
    if url.len() > 512 {
        return tool_result("fetch_web", "URL too long".into(), false);
    }
    // PATCHED (MicroAgent): encode before the request. The model hands us URLs
    // with raw UTF-8 (Chinese search terms) that `esp_http_client` refuses; the
    // pre-encoding length check above stays on the *raw* form so an oversized
    // URL is rejected rather than silently expanded.
    let url = encode_url_preserving_reserved(&url);

    match http_get_bounded(&url, HTTP_PREVIEW_MAX, "text/html,*/*") {
        Ok((status, body)) => {
            let data = format!(
                "+FETCH:{} bytes={} preview={}",
                status,
                body.len(),
                body.trim()
            );
            tool_result("fetch_web", data, (200..300).contains(&status))
        }
        Err(e) => tool_result("fetch_web", e, false),
    }
}


/// Percent-encode one URL path segment (RFC 3986 unreserved set stays literal,
/// space becomes `+`).
///
/// `esp_http_client` rejects a URL that contains non-ASCII bytes
/// (`request: ESP_ERR_INVALID_ARG`), so a Chinese city name like `上海` must be
/// encoded before it goes on the wire — verified against the live service:
/// `https://wttr.in/%E4%B8%8A%E6%B5%B7?format=3` → `上海: ☁️ +25°C`.
fn encode_url_segment(s: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `get_weather city=<name>` — live current conditions from wttr.in (no key).
///
/// `format=3` is wttr.in's compact one-liner (`<city>: <condition> <temp>`),
/// which fits the bounded tool-result buffer. The city is restricted to
/// alphanumerics / spaces / `-` / `_` (non-ASCII UTF-8, e.g. `北京`, is allowed
/// and percent-encoded) so a model-supplied value cannot smuggle extra URL
/// syntax into the request.
fn get_weather(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    let city = kv(&pairs, "city", "").trim();
    if city.is_empty() {
        return tool_result("get_weather", "missing city=<name>".into(), false);
    }
    if city.len() > 48
        || city.bytes().any(|b| {
            b < 0x80 && !(b.is_ascii_alphanumeric() || b == b' ' || b == b'-' || b == b'_')
        })
    {
        return tool_result("get_weather", format!("refusing odd city name: {city}"), false);
    }

    let url = format!(
        "https://wttr.in/{}?format=3",
        encode_url_segment(city)
    );
    match http_get_bounded(&url, 256, "text/plain,*/*") {
        Ok((status, body)) => {
            let text = body.trim();
            let ok = (200..300).contains(&status) && !text.is_empty();
            tool_result("get_weather", format!("+WEATHER:{status} {text}"), ok)
        }
        Err(e) => tool_result("get_weather", e, false),
    }
}

/// `get_news limit=<1-8>` — today's headlines from the 60s digest API (no key).
///
/// The upstream payload is JSON and the headlines live in `data.news`, either as
/// plain strings or as objects with a `title`. The emitted digest is bounded to
/// what `ToolResult::data` can hold (256 B), so a handful of short headlines.
fn get_news(args: &str) -> ToolResult {
    const NEWS_URL: &str = "https://60s.viki.moe/v2/60s";
    let pairs = parse_kv(args);
    let limit: usize = kv(&pairs, "limit", "3").parse().unwrap_or(3).clamp(1, 8);

    match http_get_bounded(NEWS_URL, 4096, "application/json,*/*") {
        Ok((status, body)) => {
            if !(200..300).contains(&status) {
                return tool_result("get_news", format!("+NEWS:{status} upstream error"), false);
            }
            let titles = extract_news_titles(&body, limit);
            if titles.is_empty() {
                let preview: std::string::String = body.chars().take(220).collect();
                return tool_result("get_news", format!("+NEWS:{status} raw={preview}"), false);
            }
            let mut out = std::string::String::from("+NEWS:");
            for (i, t) in titles.iter().enumerate() {
                let piece = format!("{}. {} ", i + 1, t.trim());
                // `ToolResult::data` is a `heapless::String<256>`; stop before the
                // truncation would cut a headline in half.
                if out.len() + piece.len() > 240 {
                    break;
                }
                out.push_str(&piece);
            }
            tool_result("get_news", out, true)
        }
        Err(e) => tool_result("get_news", e, false),
    }
}

/// `get_flight_price from=<IATA>,to=<IATA>[,date=YYYY-MM[-DD]][,limit=<1-5>]` —
/// cheapest fares between two airports/cities from a keyed provider
/// (`AT+FLIGHTCFG=`), e.g. `from=SHA,to=LXA` (Shanghai → Lhasa).
///
/// Providers (both single-GET, so they fit the bounded helper):
/// * `travelpayouts` — `api.travelpayouts.com/aviasales/v3/prices_for_dates`,
///   token passed as a query parameter, `departure_at` optional (`YYYY-MM`).
/// * `kiwi` — `api.tequila.kiwi.com/v2/search`, key passed in an `apikey`
///   header, **requires** a date (`date=YYYY-MM-DD`).
///
/// Without a configured provider this returns a clear "not configured" message
/// instead of guessing — the LLM is also instructed never to invent prices.
fn get_flight_price(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    let from = kv(&pairs, "from", "").trim().to_ascii_uppercase();
    let to = kv(&pairs, "to", "").trim().to_ascii_uppercase();
    let date = kv(&pairs, "date", "").trim().to_string();
    let limit: usize = kv(&pairs, "limit", "3").parse().unwrap_or(3).clamp(1, 5);

    if from.is_empty() || to.is_empty() {
        return tool_result(
            "get_flight_price",
            "need from=<IATA> and to=<IATA> (e.g. from=SHA,to=LXA)".into(),
            false,
        );
    }
    for code in [&from, &to] {
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return tool_result(
                "get_flight_price",
                format!("airport/city code must be 3 letters (got {code})"),
                false,
            );
        }
    }
    // `date` is interpolated straight into the query string, so it has to be
    // constrained to the ISO shape. Unvalidated, a model-supplied
    // `date=2026-01&token=…` would append arbitrary parameters to our request.
    if !date.is_empty()
        && (date.len() > 10
            || date.bytes().any(|b| !(b.is_ascii_digit() || b == b'-'))
            || date.matches('-').count() > 2)
    {
        return tool_result(
            "get_flight_price",
            format!("date must be YYYY-MM or YYYY-MM-DD (got {date})"),
            false,
        );
    }

    let Some((provider, token)) = crate::at_dispatch::flight_cfg() else {
        return tool_result(
            "get_flight_price",
            "no flight provider configured — run AT+FLIGHTCFG=<provider>,<token>".into(),
            false,
        );
    };

    let mut header_store = std::string::String::new();
    let (url, header_key): (std::string::String, Option<&str>) = match provider.as_str() {
        "travelpayouts" => {
            let mut u = format!(
                "https://api.travelpayouts.com/aviasales/v3/prices_for_dates\
                 ?origin={from}&destination={to}&currency=cny&limit={limit}&sorting=price"
            );
            if !date.is_empty() {
                u.push_str(&format!("&departure_at={date}"));
            }
            u.push_str(&format!("&token={token}"));
            (u, None)
        }
        "kiwi" => {
            if date.is_empty() {
                return tool_result(
                    "get_flight_price",
                    "the kiwi provider needs date=YYYY-MM-DD".into(),
                    false,
                );
            }
            header_store = token.clone();
            let u = format!(
                "https://api.tequila.kiwi.com/v2/search?fly_from={from}&fly_to={to}\
                 &date_from={date}&date_to={date}&curr=CNY&limit={limit}&sort=price"
            );
            (u, Some("apikey"))
        }
        other => {
            return tool_result(
                "get_flight_price",
                format!("unsupported flight provider '{other}'"),
                false,
            )
        }
    };

    let headers_arr: [(&str, &str); 1] = match header_key {
        Some(k) => [(k, header_store.as_str())],
        None => [("", "")],
    };
    let extra: &[(&str, &str)] = if header_key.is_some() {
        &headers_arr
    } else {
        &[]
    };

    match http_get_bounded_headers(&url, 4096, "application/json,*/*", extra) {
        Ok((status, body)) => {
            if !(200..300).contains(&status) {
                return tool_result(
                    "get_flight_price",
                    format!("+FLIGHT:{status} upstream error ({provider})"),
                    false,
                );
            }
            let fares = extract_fares(&body, limit, &provider);
            if fares.is_empty() {
                return tool_result(
                    "get_flight_price",
                    format!("+FLIGHT:{status} no fares in response ({provider})"),
                    false,
                );
            }
            let mut out = std::string::String::from("+FLIGHT:");
            for f in &fares {
                // `ToolResult::data` is `heapless::String<256>`: keep whole entries.
                if out.len() + f.len() > 240 {
                    break;
                }
                out.push_str(f);
            }
            tool_result("get_flight_price", out, true)
        }
        Err(e) => tool_result("get_flight_price", e, false),
    }
}

/// Pull up to `limit` fares out of a provider payload, formatted as
/// `CNY<price> <YYYY-MM-DD> <airline> <直飞|N转>; `. Handles both provider shapes:
/// Travelpayouts `data[]` (objects with `price` / `departure_at` / `airline` /
/// `transfers`) and Kiwi `data[]` (objects with `price` / `local_departure` /
/// `airlines` / `route[]`).
fn extract_fares(body: &str, limit: usize, provider: &str) -> Vec<std::string::String> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return out;
    };
    let Some(items) = v.get("data").and_then(|d| d.as_array()) else {
        return out;
    };

    for item in items.iter().take(limit) {
        let price = item
            .get("price")
            .and_then(|p| p.as_f64())
            .map(|p| p.round() as i64)
            .unwrap_or(0);
        if price <= 0 {
            continue;
        }
        let when = item
            .get("departure_at")
            .or_else(|| item.get("local_departure"))
            .or_else(|| item.get("departure_date"))
            .and_then(|d| d.as_str())
            .unwrap_or("");
        let day: std::string::String = when.chars().take(10).collect();
        let airline = if provider == "kiwi" {
            item.get("airlines")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|a| a.as_str())
                .unwrap_or("")
        } else {
            item.get("airline").and_then(|a| a.as_str()).unwrap_or("")
        };
        // Stopovers: Travelpayouts reports a `transfers` count, Kiwi a `route`
        // array whose legs are one more than the number of stops.
        let stops = item
            .get("transfers")
            .and_then(|t| t.as_i64())
            .or_else(|| {
                item.get("route")
                    .and_then(|r| r.as_array())
                    .map(|r| (r.len() as i64 - 1).max(0))
            })
            .unwrap_or(0);
        let stops_txt = if stops <= 0 {
            "直飞".to_string()
        } else {
            format!("{stops}转")
        };
        out.push(format!("CNY{price} {day} {airline} {stops_txt}; "));
    }
    out
}

/// Pull up to `limit` headline strings out of the 60s API payload.
fn extract_news_titles(body: &str, limit: usize) -> Vec<std::string::String> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return out;
    };
    let Some(items) = v
        .get("data")
        .and_then(|d| d.get("news"))
        .and_then(|n| n.as_array())
    else {
        return out;
    };
    for item in items.iter().take(limit) {
        let title = match item {
            serde_json::Value::String(s) => s.clone(),
            other => other
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        if !title.trim().is_empty() {
            out.push(title);
        }
    }
    out
}


/// Tools `Esp32ToolHandler` can actually run: the hardware/sensor set plus the
/// four network tools.
///
/// This governs what the LLM is told it can do
/// ([`magent_core::tools::ToolRegistry::describe_served`]). The registry still
/// holds the whole built-in hardware pack — the health sensors, `voice_output`,
/// `send_notification`, `ble_send` — none of which this board has hardware for;
/// without this list the prompt advertised them and every call came back "not
/// implemented on this target". Keep in step with the `handle` arms below (the
/// fallback logs if a name here has no arm).
const SERVED: &[&str] = &[
    "write_gpio",
    "read_sensor",
    "flash_write",
    "flash_read",
    "fetch_web",
    "get_weather",
    "get_news",
    "get_flight_price",
];

impl ToolHandler for Esp32ToolHandler {
    fn handle(&self, call: &ToolCall) -> Option<ToolResult> {
        match call.name.as_str() {
            "write_gpio" => Some(write_gpio(call.arguments.as_str())),
            "read_sensor" => Some(read_sensor(call.arguments.as_str())),
            "flash_write" => Some(flash_write(call.arguments.as_str())),
            "flash_read" => Some(flash_read(call.arguments.as_str())),
            "fetch_web" => Some(fetch_web(call.arguments.as_str())),
            "get_weather" => Some(get_weather(call.arguments.as_str())),
            "get_news" => Some(get_news(call.arguments.as_str())),
            "get_flight_price" => Some(get_flight_price(call.arguments.as_str())),
            _ => {
                // A name advertised as served but with no arm here would be a
                // tool the prompt promises and this handler refuses — the exact
                // gap `SERVED` exists to close. Say so rather than fall through
                // silently.
                if SERVED.contains(&call.name.as_str()) {
                    log::error!(
                        "[tools] '{}' is listed in SERVED but has no handler arm — \
                         the prompt advertises a tool this target cannot run",
                        call.name
                    );
                }
                None
            }
        }
    }

    fn serves(&self, name: &str) -> bool {
        SERVED.contains(&name)
    }

    /// Advertise the sensors this board really reads.
    ///
    /// The registry's shared `read_sensor` description lists twelve sensors,
    /// but this handler runs only two of them: the chip's internal temperature
    /// sensor and the free heap. Shipping the shared text taught the model it
    /// could read a battery, accelerometer, humidity, pressure, light, heart
    /// rate, HRV, glucose, ECG or stress level on a board that answers every
    /// one of those with "unsupported sensor" (`read_sensor`'s `else` arm) —
    /// and left `memory`, which the board *can* read, unmentioned. Keep this in
    /// step with the `read_sensor` arms above; a change there without a change
    /// here re-opens the gap.
    fn describe_tool(&self, name: &str) -> Option<&'static str> {
        match name {
            "read_sensor" => Some(
                "Read a real device value. args: sensor=temperature (chip die temperature) \
                 | sensor=memory (free heap)",
            ),
            _ => None,
        }
    }
}
