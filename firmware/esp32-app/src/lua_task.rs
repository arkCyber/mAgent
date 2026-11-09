//! ESP32-S3 Lua application task (`board-s3` only).
//!
//! Implements [`magent_lua::HardwareBackend`] on the ESP32-S3 and runs the
//! **"user App as brain, AI agent as brain-trust"** [`magent_lua::AppRuntime`]
//! on a dedicated FreeRTOS thread.
//!
//! # Status
//! This is the **stage-2 skeleton**: the runtime, sandbox, action dispatch,
//! error containment, heartbeat and watchdog are all host-verified in the
//! `magent-lua` crate; only the `Esp32Hardware` adapter is chip-specific.
//! It must be compiled with the Xtensa toolchain and validated on real S3
//! hardware (see `docs/LUA_SCRIPTING_S3.md`).
//!
//! `Esp32Hardware` uses the raw ESP-IDF C API (the same approach as
//! `local_tools.rs`). **Wired:** GPIO I/O, internal die temperature, free
//! heap, PWM output (LEDC — per-pin lazy channels), ADC input (oneshot,
//! ADC1 GPIO1..=10), I2C master (I2C_NUM_0, register read/write, SDA/SCL pins
//! via `I2C_SDA_PIN`/`I2C_SCL_PIN`), and persistent flash (NVS-backed, address
//! → keyed blob). Every operation returns an explicit `Err` (never a panic)
//! when its hardware isn't present / connected. `ble_send` always returns
//! `Err`: BLE was removed from the firmware (see `docs/BACKLOG.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::sync::OnceLock;
use std::time::Duration;

use esp_idf_sys as sys;

use magent_core::MiniAgent;
use magent_lua::piccolo_vm::PiccoloVm;
use magent_lua::runtime::AppRuntime;
use magent_lua::{HardwareBackend, SharedHardware};

/// Default enterprise script: a boot self-test that probes every wired driver.
///
/// Each probe is wrapped in `pcall` so one failure (e.g. I2C with no device on
/// the bus) is reported but never stops the app. On the linked S3 this prints a
/// per-driver `ok`/`err` line to the console for validation.
///
/// NOTE: written against the `piccolo` stdlib — it must NOT use `string.format`
/// (piccolo does not implement it; use `..` concatenation instead).
///
/// This is the **fallback** app source: an operator-provided `main.lua` stored
/// in NVS (see [`set_lua_app_source`] / [`load_app_source`]) takes precedence at
/// boot, so the app can be updated without reflashing.
const DEFAULT_MAIN_LUA: &str = r#"
-- default enterprise app: passive boot + explicit hardware self-test.
--
-- The boot chunk deliberately does NOT touch peripherals. The Lua host is
-- brought up while Wi-Fi/TLS is still starting; a flash write or an
-- ADC/I2C/GPIO reconfiguration in that window is the riskiest thing this
-- process can do (cache-disabled flash operations / owned-pin reconfiguration
-- can fault the other core). The embedded app therefore only announces
-- itself. Call `selftest()` from an uploaded app, once the link is up, to
-- probe every wired driver with `pcall` containment.
local function probe(label, fn)
    local ok, res = pcall(fn)
    if ok then
        print("[lua] " .. label .. " ok  " .. tostring(res))
    else
        print("[lua] " .. label .. " err " .. tostring(res))
    end
end

function selftest()
    probe("temp",  function() return hardware.sensor_read("temp") end)
    probe("adc",   function() return hardware.adc_read(1) end)
    probe("pwm",   function() hardware.pwm_set(1, 50) return "duty=50%" end)
    probe("i2c",   function() return hardware.i2c_read(0x38, 0x0F, 1) end)
    probe("gpio",  function() hardware.gpio_write(2, 1) return "p2=1" end)
    probe("flash", function() hardware.flash_write(0x100, "HELLO") return hardware.flash_read(0x100, 5) end)
    return "selftest complete"
end

print("[lua] app booted (passive); call selftest() to probe hardware")
"#;

/// A [`HardwareBackend`] for the ESP32-S3.
#[derive(Debug, Default)]
pub struct Esp32Hardware;

// ---------------------------------------------------------------------------
// PWM (LEDC)
// ---------------------------------------------------------------------------
// `Esp32Hardware` is a stateless handle (unit struct), so the lazily-created
// LEDC timer/channel configuration lives in process-static state. It is only
// touched from the single `lua-thread`, so `Mutex` here is just for safe static
// mutability (never contended).
//
// Timer 0 is configured once (8-bit, 1 kHz, auto clock). PWM channels 0..7 are
// allocated on first use per GPIO. Duty is expressed as 0..=100 (%).
const LEDC_SPEED_MODE: sys::ledc_mode_t = sys::ledc_mode_t_LEDC_LOW_SPEED_MODE;
static PWM_TIMER_READY: OnceLock<()> = OnceLock::new();
static PWM_CHANNELS: Mutex<Option<BTreeMap<u8, u8>>> = Mutex::new(None); // gpio -> ledc channel 0..7

/// Configure LEDC timer 0 once (idempotent).
fn pwm_ensure_timer() -> Result<(), String> {
    if PWM_TIMER_READY.get().is_some() {
        return Ok(());
    }
    // SAFETY: raw ESP-IDF LEDC timer config; returns an `esp_err_t`
    // (0 == OK) which we check and propagate.
    let conf = sys::ledc_timer_config_t {
        speed_mode: LEDC_SPEED_MODE,
        duty_resolution: sys::ledc_timer_bit_t_LEDC_TIMER_8_BIT,
        timer_num: sys::ledc_timer_t_LEDC_TIMER_0,
        freq_hz: 1000,
        clk_cfg: sys::soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
        deconfigure: false,
    };
    let r = unsafe { sys::ledc_timer_config(&conf) };
    if r != 0 {
        return Err(format!("ledc_timer_config err={r}"));
    }
    PWM_TIMER_READY
        .set(())
        .map_err(|_| "pwm timer ready already set".to_string())
}

/// Return the LEDC channel bound to `pin`, allocating and configuring it on
/// first use. The ESP32-S3 has 8 LEDC channels total.
fn pwm_channel_for(pin: u8) -> Result<u8, String> {
    pwm_ensure_timer()?;
    let mut guard = PWM_CHANNELS
        .lock()
        .map_err(|_| "pwm_channels lock poisoned".to_string())?;
    let map = guard.get_or_insert_with(BTreeMap::new);
    if let Some(&ch) = map.get(&pin) {
        return Ok(ch);
    }
    let next: u32 = map.len() as u32;
    if next >= 8 {
        return Err(format!("pwm_set(pin={pin}): no free LEDC channel (max 8)"));
    }
    // SAFETY: raw ESP-IDF LEDC channel config; returns an `esp_err_t`.
    let conf = sys::ledc_channel_config_t {
        gpio_num: pin as i32,
        speed_mode: LEDC_SPEED_MODE,
        channel: next,
        intr_type: sys::ledc_intr_type_t_LEDC_INTR_DISABLE,
        timer_sel: sys::ledc_timer_t_LEDC_TIMER_0,
        duty: 0,
        hpoint: 0,
        sleep_mode: sys::ledc_sleep_mode_t_LEDC_SLEEP_MODE_NO_ALIVE_NO_PD,
        flags: Default::default(),
    };
    let r = unsafe { sys::ledc_channel_config(&conf) };
    if r != 0 {
        return Err(format!("ledc_channel_config(pin={pin}) err={r}"));
    }
    map.insert(pin, next as u8);
    Ok(next as u8)
}

/// Set PWM duty (`0..=100` %) on a pin, mapping % to 8-bit resolution.
fn pwm_set_duty(pin: u8, duty: u8) -> Result<(), String> {
    let duty = u32::from(duty.min(100)); // clamp to 0..=100 %
    let bits = (duty * 255) / 100; // 8-bit LEDC duty
    let ch = pwm_channel_for(pin)?;
    // SAFETY: raw ESP-IDF LEDC duty set; returns an `esp_err_t`.
    let r = unsafe { sys::ledc_set_duty(LEDC_SPEED_MODE, u32::from(ch), bits) };
    if r != 0 {
        return Err(format!("ledc_set_duty(pin={pin}) err={r}"));
    }
    let r = unsafe { sys::ledc_update_duty(LEDC_SPEED_MODE, u32::from(ch)) };
    if r != 0 {
        return Err(format!("ledc_update_duty(pin={pin}) err={r}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ADC (oneshot, ADC1)
// ---------------------------------------------------------------------------
// ESP32-S3 ADC1: GPIO1..=10 map to channels 0..=9. We deliberately use ADC1
// only — ADC2 (GPIO11..) conflicts with the Wi-Fi peripheral the firmware
// uses, so those pins return an explicit error. The oneshot unit is created
// lazily once; each GPIO's channel is configured once on first use.
//
// The raw ESP-IDF unit handle is a raw pointer, so it is wrapped in `AdcUnit`
// with an `unsafe Send + Sync` so it can live in a `static`. Safe because it is
// only dereferenced (through the raw API) from the single `lua-thread`, and the
// handle is never freed while the process lives.
#[derive(Clone, Copy)]
struct AdcUnit(sys::adc_oneshot_unit_handle_t);
unsafe impl Send for AdcUnit {}
unsafe impl Sync for AdcUnit {}

struct Adc1State {
    unit: Option<AdcUnit>,
    channels: BTreeSet<u8>,
}

static ADC1_STATE: Mutex<Option<Adc1State>> = Mutex::new(None);

/// Create the ADC1 oneshot unit once, returning its raw handle.
fn adc_ensure_unit(state: &mut Adc1State) -> Result<sys::adc_oneshot_unit_handle_t, String> {
    if let Some(unit) = &state.unit {
        return Ok(unit.0);
    }
    let init_cfg = sys::adc_oneshot_unit_init_cfg_t {
        unit_id: sys::adc_unit_t_ADC_UNIT_1,
        clk_src: sys::soc_periph_adc_rtc_clk_src_t_ADC_RTC_CLK_SRC_DEFAULT,
        ulp_mode: sys::adc_ulp_mode_t_ADC_ULP_MODE_DISABLE,
    };
    let mut handle: sys::adc_oneshot_unit_handle_t = std::ptr::null_mut();
    // SAFETY: raw ESP-IDF ADC unit create; returns an `esp_err_t`.
    let r = unsafe { sys::adc_oneshot_new_unit(&init_cfg, &mut handle) };
    if r != 0 {
        return Err(format!("adc_oneshot_new_unit err={r}"));
    }
    state.unit = Some(AdcUnit(handle));
    Ok(handle)
}

/// Configure one ADC channel (12-bit, 11 dB attenuation) on first use.
fn adc_ensure_channel(
    state: &mut Adc1State,
    unit: sys::adc_oneshot_unit_handle_t,
    channel: u8,
) -> Result<(), String> {
    if state.channels.contains(&channel) {
        return Ok(());
    }
    let cfg = sys::adc_oneshot_chan_cfg_t {
        atten: sys::adc_atten_t_ADC_ATTEN_DB_11,
        bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_12,
    };
    // SAFETY: raw ESP-IDF ADC channel config; returns an `esp_err_t`.
    let r = unsafe { sys::adc_oneshot_config_channel(unit, u32::from(channel), &cfg) };
    if r != 0 {
        return Err(format!("adc_oneshot_config_channel(ch={channel}) err={r}"));
    }
    state.channels.insert(channel);
    Ok(())
}

/// Read a pin as volts via the ADC1 oneshot driver (GPIO1..=10).
fn adc_read_pin(pin: u8) -> Result<f64, String> {
    if !(1..=10).contains(&pin) {
        return Err(format!("adc_read(pin={pin}): only ADC1 GPIO1..=10 supported"));
    }
    let channel: u8 = pin - 1; // GPIO1 -> ADC_CHANNEL_0
    let mut guard = ADC1_STATE
        .lock()
        .map_err(|_| "adc state lock poisoned".to_string())?;
    let state = guard.get_or_insert_with(|| Adc1State {
        unit: None,
        channels: BTreeSet::new(),
    });
    let unit = adc_ensure_unit(state)?;
    adc_ensure_channel(state, unit, channel)?;
    let mut raw: core::ffi::c_int = 0;
    // SAFETY: raw ESP-IDF ADC oneshot read; returns an `esp_err_t`.
    let r = unsafe { sys::adc_oneshot_read(unit, u32::from(channel), &mut raw) };
    if r != 0 {
        return Err(format!("adc_oneshot_read(pin={pin}) err={r}"));
    }
    // Linear full-scale approximation: 12-bit, 0..=3.3 V. (For production use
    // `adc_cali_create_scheme_curve_fitting` + `adc_cali_raw_to_voltage`.)
    Ok((raw as f64) / 4095.0 * 3.3)
}

// ---------------------------------------------------------------------------
// I2C (master, I2C_NUM_0)
// ---------------------------------------------------------------------------
// Register-style master on I2C_NUM_0: `i2c_read` does a write(reg) then read
// (repeated start); `i2c_write` sends reg followed by payload. SDA/SCL pins are
// a board-wiring choice — adjust to match the hardware.
//
// PATCHED (MicroAgent): moved from GPIO8=SCL / GPIO9=SDA to GPIO4=SCL /
// GPIO5=SDA. GPIO8 is a strapping / SPI-flash-related pin on the ESP32-S3;
// reconfiguring it as I2C and driving it while flash is accessed can fault
// (candidate for the Lua `Core 0 StoreProhibited`). GPIO4/GPIO5 are plain
// general-purpose pins, safe for I2C on the S3 devkit.
pub(crate) const I2C_SCL_PIN: u8 = 4;
pub(crate) const I2C_SDA_PIN: u8 = 5;
const I2C_TIMEOUT_TICKS: u32 = 1000; // ~1 s at the default 1 kHz tick

static I2C_READY: OnceLock<()> = OnceLock::new();

/// Install the I2C0 master driver once (idempotent).
fn i2c_ensure_installed() -> Result<(), String> {
    if I2C_READY.get().is_some() {
        return Ok(());
    }
    let conf = sys::i2c_config_t {
        mode: sys::i2c_mode_t_I2C_MODE_MASTER,
        sda_io_num: i32::from(I2C_SDA_PIN),
        scl_io_num: i32::from(I2C_SCL_PIN),
        sda_pullup_en: true,
        scl_pullup_en: true,
        __bindgen_anon_1: sys::i2c_config_t__bindgen_ty_1 {
            master: sys::i2c_config_t__bindgen_ty_1__bindgen_ty_1 { clk_speed: 100_000 },
        },
        clk_flags: 0,
    };
    // SAFETY: raw ESP-IDF I2C param config; returns an `esp_err_t`.
    let r = unsafe { sys::i2c_param_config(sys::i2c_port_t_I2C_NUM_0, &conf) };
    if r != 0 {
        return Err(format!("i2c_param_config err={r}"));
    }
    // SAFETY: raw ESP-IDF I2C driver install (master mode, no slave buffers).
    let r = unsafe {
        sys::i2c_driver_install(sys::i2c_port_t_I2C_NUM_0, sys::i2c_mode_t_I2C_MODE_MASTER, 0, 0, 0)
    };
    if r != 0 {
        return Err(format!("i2c_driver_install err={r}"));
    }
    I2C_READY
        .set(())
        .map_err(|_| "i2c ready already set".to_string())
}

/// Write `data` to `reg` on a 7-bit `addr` I2C device.
fn i2c_write_device(addr: u8, reg: u8, data: &[u8]) -> Result<(), String> {
    i2c_ensure_installed()?;
    let mut buf = Vec::with_capacity(1 + data.len());
    buf.push(reg);
    buf.extend_from_slice(data);
    // SAFETY: raw ESP-IDF I2C master write; returns an `esp_err_t`.
    let r = unsafe {
        sys::i2c_master_write_to_device(
            sys::i2c_port_t_I2C_NUM_0,
            addr,
            buf.as_ptr(),
            buf.len(),
            I2C_TIMEOUT_TICKS,
        )
    };
    if r != 0 {
        return Err(format!(
            "i2c_master_write_to_device(addr=0x{addr:02x},reg=0x{reg:02x}) err={r}"
        ));
    }
    Ok(())
}

/// Read `len` bytes starting at `reg` on a 7-bit `addr` I2C device.
fn i2c_read_device(addr: u8, reg: u8, len: usize) -> Result<Vec<u8>, String> {
    i2c_ensure_installed()?;
    let mut buf = vec![0u8; len];
    let reg_buf = [reg];
    // SAFETY: raw ESP-IDF I2C master write-then-read (repeated start).
    let r = unsafe {
        sys::i2c_master_write_read_device(
            sys::i2c_port_t_I2C_NUM_0,
            addr,
            reg_buf.as_ptr(),
            1,
            buf.as_mut_ptr(),
            len,
            I2C_TIMEOUT_TICKS,
        )
    };
    if r != 0 {
        return Err(format!(
            "i2c_master_write_read_device(addr=0x{addr:02x},reg=0x{reg:02x}) err={r}"
        ));
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Flash (NVS-backed)
// ---------------------------------------------------------------------------
// Persistent storage mapped to ESP-IDF NVS by `crate::nvs_flash`:
// `flash_write(addr, data)` stores a blob under key `flash_<addr:08x>`,
// `flash_read` reads it back (up to `len`), `flash_erase_sector` removes the
// key. The same store backs the agent's `flash_read` / `flash_write` tools, so
// this module keeps no copy of the address->key scheme.

// ---------------------------------------------------------------------------
// App source (operator-updatable `main.lua` stored in NVS)
// ---------------------------------------------------------------------------
// NVS key holding the operator-provided `main.lua` source. At boot we prefer
// this over the embedded `DEFAULT_MAIN_LUA`, so operators can update the app
// without reflashing (write it with [`set_lua_app_source`]). Key <=15 chars.
const NVS_APP_KEY: &[u8] = b"main.lua\0";

/// Read the operator-provided `main.lua` from NVS, if present.
fn nvs_read_app_source() -> Result<Option<String>, String> {
    let handle = crate::nvs_flash::open_rw()?;
    let mut required: usize = 0;
    // SAFETY: raw ESP-IDF NVS blob length query; returns an `esp_err_t`.
    let q = unsafe {
        sys::nvs_get_blob(
            handle,
            NVS_APP_KEY.as_ptr() as *const core::ffi::c_char,
            std::ptr::null_mut(),
            &mut required,
        )
    };
    if q == sys::ESP_ERR_NVS_NOT_FOUND {
        // SAFETY: raw ESP-IDF NVS handle close.
        unsafe { sys::nvs_close(handle) };
        return Ok(None);
    }
    if q != 0 {
        // SAFETY: raw ESP-IDF NVS handle close.
        unsafe { sys::nvs_close(handle) };
        return Err(format!("nvs_read_app_source query err={q}"));
    }
    let mut buf = vec![0u8; required];
    let mut out_len = buf.len();
    // SAFETY: raw ESP-IDF NVS blob read; returns an `esp_err_t`.
    let r = unsafe {
        sys::nvs_get_blob(
            handle,
            NVS_APP_KEY.as_ptr() as *const core::ffi::c_char,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut out_len,
        )
    };
    // SAFETY: raw ESP-IDF NVS handle close.
    unsafe { sys::nvs_close(handle) };
    if r != 0 {
        return Err(format!("nvs_read_app_source err={r}"));
    }
    buf.truncate(out_len);
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Load the app source: the operator's NVS copy if present and non-empty,
/// otherwise the embedded `DEFAULT_MAIN_LUA`.
fn load_app_source() -> String {
    match nvs_read_app_source() {
        Ok(Some(src)) if !src.trim().is_empty() => {
            log::info!("[lua] loaded main.lua from NVS ({} bytes)", src.len());
            src
        }
        _ => {
            log::info!("[lua] using embedded main.lua");
            DEFAULT_MAIN_LUA.to_string()
        }
    }
}

/// Maximum size of an operator-supplied `main.lua` (16 KiB).
///
/// The AT line limit is 256 bytes, so a real app arrives in chunks; 16 KiB is
/// far larger than the embedded bootstrap script and comfortably below what a
/// single NVS blob can hold on the S3 partition. A runaway upload is refused
/// instead of consuming the NVS partition or the Lua heap.
pub const MAX_LUA_APP_BYTES: usize = 16 * 1024;

/// Persist `main.lua` into NVS so a future boot runs it (operator update path).
///
/// This replaces any previously stored app. `AT+LUAAPP=` drives it (and
/// `AT+LUAAPP=+<b64>` appends a chunk through [`append_lua_app_source`]).
/// Returns an error if the source exceeds [`MAX_LUA_APP_BYTES`] or the NVS
/// write fails.
pub fn set_lua_app_source(source: &str) -> Result<(), String> {
    if source.len() > MAX_LUA_APP_BYTES {
        return Err(format!(
            "lua app is {} bytes (max {MAX_LUA_APP_BYTES})",
            source.len()
        ));
    }
    nvs_write_app_source(source)
}

/// Append a decoded chunk to the stored `main.lua`.
///
/// The AT command line is capped at 256 bytes, so the operator uploads a
/// script in chunks (`AT+LUAAPP=+<url-safe-base64>`); each call reads the
/// stored blob, appends the chunk and writes it back. That is a read-modify-
/// write per chunk (and therefore an NVS commit per chunk), which is fine for
/// occasional operator updates but not a hot path.
pub fn append_lua_app_source(chunk: &str) -> Result<(), String> {
    if chunk.is_empty() {
        return Err("empty chunk".to_string());
    }
    let mut source = nvs_read_app_source()?.unwrap_or_default();
    let new_len = source
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| "lua app size overflow".to_string())?;
    if new_len > MAX_LUA_APP_BYTES {
        return Err(format!(
            "lua app would become {new_len} bytes (max {MAX_LUA_APP_BYTES})"
        ));
    }
    source.push_str(chunk);
    nvs_write_app_source(&source)
}

/// Shared NVS write for [`set_lua_app_source`] / [`append_lua_app_source`].
fn nvs_write_app_source(source: &str) -> Result<(), String> {
    let handle = crate::nvs_flash::open_rw()?;
    // SAFETY: raw ESP-IDF NVS blob set; returns an `esp_err_t`.
    let r = unsafe {
        sys::nvs_set_blob(
            handle,
            NVS_APP_KEY.as_ptr() as *const core::ffi::c_char,
            source.as_ptr() as *const core::ffi::c_void,
            source.len(),
        )
    };
    let c = if r == 0 { unsafe { sys::nvs_commit(handle) } } else { r };
    // SAFETY: raw ESP-IDF NVS handle close.
    unsafe { sys::nvs_close(handle) };
    if c != 0 {
        return Err(format!("set_lua_app_source err={c}"));
    }
    Ok(())
}

/// Length in bytes of the operator-stored `main.lua` in NVS, or `None` if no
/// custom app has been set (the embedded `DEFAULT_MAIN_LUA` is used).
pub fn lua_app_source_len() -> Option<usize> {
    nvs_read_app_source().ok().flatten().map(|s| s.len())
}

use crate::nvs_flash::{read as flash_read_device, write as flash_write_device};

/// `flash_erase_sector` takes an *address*; the shared store keys by address,
/// so this is a thin alias rather than a sector erase.
fn flash_erase_sector_device(addr: u32) -> Result<(), String> {
    crate::nvs_flash::erase(addr)
}

impl HardwareBackend for Esp32Hardware {
    fn gpio_write(&mut self, pin: u8, level: u8) -> std::result::Result<(), String> {
        // Refuse pins the board already uses (flash / PSRAM / the ingress
        // links / I2C0): a Lua script picks the pin number, and reconfiguring
        // a flash pin faults the chip. See `reserved_pin_reason`.
        if let Some(reason) = crate::local_tools::reserved_pin_reason(i32::from(pin)) {
            return Err(format!("gpio_write(pin={pin}): {reason}"));
        }
        let lvl: u32 = u32::from(level != 0);
        // SAFETY: raw ESP-IDF GPIO calls on a plain pin number; each returns
        // an `esp_err_t` (0 == OK) which we check and propagate.
        unsafe {
            let mut r = sys::gpio_reset_pin(pin as i32);
            if r == 0 {
                r = sys::gpio_set_direction(pin as i32, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
            }
            if r == 0 {
                r = sys::gpio_set_level(pin as i32, lvl);
            }
            if r == 0 {
                Ok(())
            } else {
                Err(format!("gpio_write(pin={pin}) err={r}"))
            }
        }
    }

    fn gpio_read(&mut self, pin: u8) -> std::result::Result<u8, String> {
        if let Some(reason) = crate::local_tools::reserved_pin_reason(i32::from(pin)) {
            return Err(format!("gpio_read(pin={pin}): {reason}"));
        }
        // SAFETY: raw ESP-IDF GPIO; configure as input then read the level.
        unsafe {
            let mut r = sys::gpio_reset_pin(pin as i32);
            if r == 0 {
                r = sys::gpio_set_direction(pin as i32, sys::gpio_mode_t_GPIO_MODE_INPUT);
            }
            if r != 0 {
                return Err(format!("gpio_read(pin={pin}) config err={r}"));
            }
            Ok(sys::gpio_get_level(pin as i32) as u8)
        }
    }

    fn sensor_read(&mut self, name: &str) -> std::result::Result<f64, String> {
        let lower = name.to_ascii_lowercase();
        if lower.contains("temp") || lower.contains("die") {
            // Internal die temperature (ESP32-S3). Mirrors `local_tools::read_sensor`
            // exactly — explicit config struct, checked sys calls, always uninstall.
            let cfg = sys::temperature_sensor_config_t {
                range_min: -10,
                range_max: 80,
                clk_src: sys::soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
                flags: sys::temperature_sensor_config_t__bindgen_ty_1 { allow_pd: 0 },
            };
            let mut handle: sys::temperature_sensor_handle_t = core::ptr::null_mut();
            let mut celsius: f32 = 0.0;
            // SAFETY: mirrors the C61 firmware's hardened temperature read; each
            // call's `esp_err_t` is checked and the handle is always cleaned up.
            let last = unsafe {
                let mut e = sys::temperature_sensor_install(&cfg, &mut handle);
                if e == 0 {
                    e = sys::temperature_sensor_enable(handle);
                }
                if e == 0 {
                    // Check `get_celsius`'s return as well: dropping it made a
                    // failed read return `Ok(0.0)` — a value the sensor never
                    // measured (this is the Lua `household temp` path).
                    e = sys::temperature_sensor_get_celsius(handle, &mut celsius);
                }
                let _ = sys::temperature_sensor_disable(handle);
                let _ = sys::temperature_sensor_uninstall(handle);
                e
            };
            if last == 0 {
                Ok(celsius as f64)
            } else {
                Err(format!("temp sensor err={last}"))
            }
        } else if lower.contains("mem") || lower.contains("heap") || lower.contains("sram") {
            // SAFETY: plain FFI read of the free-heap counter.
            let free = unsafe { sys::esp_get_free_heap_size() };
            Ok(free as f64)
        } else {
            Err(format!("unsupported sensor: {name}"))
        }
    }

    fn flash_read(&mut self, address: u32, len: usize) -> std::result::Result<Vec<u8>, String> {
        // NVS-backed persistent storage; see `flash_read_device`.
        flash_read_device(address, len)
    }

    fn flash_write(&mut self, address: u32, data: &[u8]) -> std::result::Result<(), String> {
        // NVS-backed persistent storage; see `flash_write_device`.
        flash_write_device(address, data)
    }

    fn flash_erase_sector(&mut self, address: u32) -> std::result::Result<(), String> {
        // NVS-backed persistent storage; see `flash_erase_sector_device`.
        flash_erase_sector_device(address)
    }

    fn i2c_read(&mut self, addr: u8, reg: u8, len: usize) -> std::result::Result<Vec<u8>, String> {
        // Register read via I2C0 master (repeated start); see `i2c_read_device`.
        i2c_read_device(addr, reg, len)
    }

    fn i2c_write(&mut self, addr: u8, reg: u8, data: &[u8]) -> std::result::Result<(), String> {
        // Register write via I2C0 master; see `i2c_write_device`.
        i2c_write_device(addr, reg, data)
    }

    fn adc_read(&mut self, pin: u8) -> std::result::Result<f64, String> {
        // ADC1 oneshot (GPIO1..=10), 12-bit, DB_11; see `adc_read_pin`. The
        // ADC range overlaps UART0 (GPIO10) and I2C0 (GPIO4/5), so the
        // reserved-pin guard applies here too.
        if let Some(reason) = crate::local_tools::reserved_pin_reason(i32::from(pin)) {
            return Err(format!("adc_read(pin={pin}): {reason}"));
        }
        adc_read_pin(pin)
    }

    fn pwm_set(&mut self, pin: u8, duty: u8) -> std::result::Result<(), String> {
        // LEDC PWM, 8-bit resolution, duty 0..=100 %. See `pwm_set_duty`.
        if let Some(reason) = crate::local_tools::reserved_pin_reason(i32::from(pin)) {
            return Err(format!("pwm_set(pin={pin}): {reason}"));
        }
        pwm_set_duty(pin, duty)
    }

    fn ble_send(&mut self, _data: &[u8]) -> std::result::Result<(), String> {
        // No BLE radio in this firmware (removed 2026-09-20 — see
        // `docs/BACKLOG.md`). Always an error so a Lua app that tries to use
        // the channel fails loudly instead of silently dropping the payload.
        Err("ble_send: this firmware has no BLE support".into())
    }

    fn power_set(&mut self, _profile: u8) -> std::result::Result<(), String> {
        // ESP32 power states are managed by the firmware / ESP-IDF; no-op.
        Ok(())
    }
}

/// Log the headroom that matters for the Lua host.
///
/// The on-device Core0 crash is not yet root-caused, so each boot phase logs
/// the free internal DRAM (the scarce pool shared with Wi-Fi/TLS), free PSRAM
/// (where the VM heap lives via `std::alloc`) and this task's minimum-ever
/// free stack. A boot log then shows *how far* the runtime got and whether it
/// ran out of headroom before the reset, instead of only the fault address.
fn log_headroom(stage: &str) {
    // SAFETY: read-only ESP-IDF heap queries plus a FreeRTOS stack
    // high-water query; `NULL` is documented as "the calling task".
    let internal = unsafe { sys::esp_get_free_heap_size() };
    let psram = unsafe { sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM) };
    let stack_free = unsafe { sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()) };
    log::info!(
        "[lua] {stage}: internal_free={internal}B psram_free={psram}B \
         stack_min_free={stack_free}B"
    );
}

/// Build and run the Lua application runtime on a dedicated thread.
///
/// The non-`Send` `AppRuntime` (it owns the sandboxed Lua interpreter —
/// `PiccoloVm` on this target, since `magent-lua` is built with
/// `--features piccolo` because `mlua` cannot build for Xtensa) is
/// constructed **inside** the spawned closure, so the closure only captures
/// `Send` values and `std::thread` accepts it. Everything is created on the
/// new thread and never moved across it.
pub fn start_lua_task() {
    // Piccolo's parser/VM frames plus a nested `agent.reason()` ReAct loop
    // need more headroom than the agent thread's 32 KiB. Try 64 KiB first;
    // if the internal-DRAM budget refuses it (shared with Wi-Fi/TLS on the
    // S3), fall back to the historical 32 KiB so the host still starts.
    let r = crate::core_affinity::spawn_thread(
        "lua-thread",
        64 * 1024,
        crate::core_affinity::ThreadProfile::REALTIME_AGENT,
        run_lua_host,
    )
    .or_else(|e| {
        log::warn!("[lua] 64 KiB stack refused ({e}); retrying with 32 KiB");
        crate::core_affinity::spawn_thread(
            "lua-thread",
            32 * 1024,
            crate::core_affinity::ThreadProfile::REALTIME_AGENT,
            run_lua_host,
        )
    });
    match r {
        Ok(_) => log::info!("[lua] thread started"),
        Err(e) => log::error!("[lua] thread spawn failed: {e}"),
    }
}

/// Thread body for the Lua host.
///
/// Kept as a plain `fn` (not a closure) so the 64 KiB -> 32 KiB spawn fallback
/// can hand the same `FnOnce` to a second `Builder::spawn` — a closure is
/// consumed/dropped on the failed attempt and cannot be retried.
fn run_lua_host() {
            // NOTE (audit-2026-08): the Lua app host is intentionally NOT
            // subscribed to the RT watchdog. Its event loop (`AppRuntime::
            // run_until_stop`) is a long-lived black box that never calls
            // `esp_task_wdt_reset()`, so subscribing it here would make even a
            // *normal* long-running Lua script false-trip the 18s watchdog and
            // reboot the board. The watchdog protects the critical real-time
            // paths (agent + ingress); the Lua host is best-effort and its
            // stalls don't affect those. Its LLM/fetch_web feeds are no-ops.
            log_headroom("thread start");
            let hardware: SharedHardware = Arc::new(Mutex::new(Esp32Hardware));
            let mut agent = match MiniAgent::with_defaults() {
                Ok(a) => a,
                Err(e) => {
                    log::error!("[lua] MiniAgent init: {e}");
                    return;
                }
            };
            // Real hardware tools (GPIO / temperature) so agent-driven tool calls
            // work on-device (mirrors `main::setup_agent`).
            agent.set_tool_handler(&crate::local_tools::Esp32ToolHandler);
            // Install the DeepSeek chat-LLM backend when configured in NVS
            // (via AT+LLMCFG), so `agent.reason` returns real decisions instead
            // of the canned heuristic. BLE-only / no-LLM builds fall back to the
            // local heuristic. `Box::leak` mirrors the main agent's one-shot
            // `&'static mut` backend pattern.
            #[cfg(feature = "board-s3")]
            {
                // Share the **single** Core-0 LLM worker published by the main
                // agent's `setup_agent`. Spawning a second `llm-worker` here
                // meant the Lua build ran two concurrent mbedTLS clients on
                // Core 0 (2 x 24 KiB stack + TLS buffers) — a plausible
                // contributor to the on-device Core0 fault, and pure memory
                // waste. The worker serves requests serially, so two agents
                // can share it.
                let configured = matches!(
                    (
                        crate::nvs_load_string(crate::NVS_KEY_LLM_MODEL),
                        crate::nvs_load_string(crate::NVS_KEY_LLM_API_KEY),
                    ),
                    (Some(model), Some(key)) if !model.is_empty() && !key.is_empty()
                );
                if configured {
                    let mut tx = crate::llm::shared_sender();
                    if tx.is_none() {
                        // The agent thread may not have reached `setup_agent`
                        // yet; wait briefly rather than degrade to the local
                        // heuristic for the whole session.
                        for _ in 0..40 {
                            std::thread::sleep(Duration::from_millis(50));
                            crate::rt_watchdog::feed();
                            tx = crate::llm::shared_sender();
                            if tx.is_some() {
                                break;
                            }
                        }
                    }
                    match tx {
                        Some(tx) => {
                            let backend: &'static mut crate::llm::ChannelLlmBackend =
                                Box::leak(Box::new(crate::llm::ChannelLlmBackend::new(tx)));
                            // HARDENING (audit-2026-08 H9): register the leaked
                            // pointer for duplicate-leak detection, mirroring
                            // the main agent boot path.
                            if !crate::leaked_boxes().insert(backend as *mut _ as usize) {
                                log::error!(
                                    "[lua] leaking a duplicate LLM backend (same pointer \
                                     as a previous leak); refactor leak site"
                                );
                            }
                            agent.set_llm_backend(backend);
                            log::info!("[lua] sharing the main agent's Core-0 LLM worker");
                        }
                        None => log::warn!(
                            "[lua] LLM configured but no shared worker became available — \
                             Lua uses the local heuristic"
                        ),
                    }
                }
            }
            let agent = Arc::new(Mutex::new(agent));
            log_headroom("before PiccoloVm");
            let vm = PiccoloVm::new(hardware.clone(), agent);
            let mut app = AppRuntime::new(vm, hardware);
            log_headroom("after PiccoloVm+AppRuntime");
            // Prefer the operator's NVS copy of `main.lua`; fall back to the
            // embedded bootstrap script.
            let app_src = load_app_source();
            if let Err(e) = app.boot(&app_src) {
                log::error!("[lua] boot main.lua: {e}");
                // A bad/partial NVS upload must not kill the Lua host: retry
                // with the embedded bootstrap. Without this the thread simply
                // exited and Lua stayed disabled until a re-upload + reboot.
                if app_src != DEFAULT_MAIN_LUA {
                    log::warn!("[lua] falling back to the embedded DEFAULT_MAIN_LUA");
                    if let Err(e2) = app.boot(DEFAULT_MAIN_LUA) {
                        log::error!("[lua] embedded fallback also failed: {e2}");
                        return;
                    }
                } else {
                    return;
                }
            }
            log_headroom("after main.lua boot");
            log::info!("[lua] AppRuntime booted; entering event loop");

            // Drive the loop until a supervisor sets the stop flag (e.g. on
            // OTA/reboot). Per-tick errors are contained by the runtime.
            app.run_until_stop(Duration::from_millis(50), None);
            log::info!("[lua] loop stopped");
}
