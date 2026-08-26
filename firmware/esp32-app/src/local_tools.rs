//! Real-hardware local tool executor for the ESP32 firmware.
//!
//! Implements [`magent_core::tools::ToolHandler`] so the agent's built-in
//! tools (`write_gpio`, `read_sensor`, ...) actually drive the ESP32-C61
//! hardware instead of returning simulated values — no network required.
//!
//! Tools we don't cover return `None`, letting `ToolRegistry::execute` fall
//! back to its built-in simulation.

use esp_idf_sys as sys;
use magent_core::agent::{ToolCall, ToolResult};
use magent_core::tools::ToolHandler;

/// Truncate a `std::string::String` into the fixed `heapless::String<256>`
/// field that `ToolResult::data` uses.
fn into_result_data(s: std::string::String) -> heapless::String<256> {
    let mut out = heapless::String::<256>::new();
    let bytes = s.as_bytes();
    let take = bytes.len().min(255);
    // Truncation to 255 ASCII/UTF-8 boundary is best-effort; `push_str`
    // fails harmlessly if the payload doesn't fit.
    let _ = out.push_str(core::str::from_utf8(&bytes[..take]).unwrap_or("?"));
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

/// `write_gpio pin=N,state=high|low` — drive a real GPIO on the C61.
fn write_gpio(args: &str) -> ToolResult {
    let pairs = parse_kv(args);
    let pin: i32 = kv(&pairs, "pin", "13").parse().unwrap_or(13);
    let state = kv(&pairs, "state", "high");
    let level: u32 = if state.eq_ignore_ascii_case("high") || state.eq_ignore_ascii_case("on") {
        1
    } else {
        0
    };

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
                sys::temperature_sensor_get_celsius(handle, &mut celsius);
            }
            let _ = sys::temperature_sensor_disable(handle);
            let _ = sys::temperature_sensor_uninstall(handle);
            e
        };
        let ok = last == 0;
        let data = std::format!("temperature={celsius:.1} C (err={last})");
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

impl ToolHandler for Esp32ToolHandler {
    fn handle(&self, call: &ToolCall) -> Option<ToolResult> {
        match call.name.as_str() {
            "write_gpio" => Some(write_gpio(call.arguments.as_str())),
            "read_sensor" => Some(read_sensor(call.arguments.as_str())),
            _ => None,
        }
    }
}
