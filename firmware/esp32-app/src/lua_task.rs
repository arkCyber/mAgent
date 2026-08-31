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
//! via `I2C_SDA_PIN`/`I2C_SCL_PIN`), persistent flash (NVS-backed, address →
//! keyed blob), and BLE TX (pushes to the connected client's SYS_RSP via the
//! firmware `ble_config` GATT server — enabled with the `ble` feature). Every
//! operation returns an explicit `Err` (never a panic) when its hardware isn't
//! present / connected.

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
-- default enterprise app (embedded bootstrap + hardware self-test)
local function probe(label, fn)
    local ok, res = pcall(fn)
    if ok then
        print("[lua] " .. label .. " ok  " .. tostring(res))
    else
        print("[lua] " .. label .. " err " .. tostring(res))
    end
end

-- Wired drivers (see firmware/esp32-app/src/lua_task.rs):
probe("temp",   function() return hardware.sensor_read("temp") end)
probe("adc",    function() return hardware.adc_read(1) end)          -- GPIO1
probe("pwm",    function() hardware.pwm_set(1, 50) return "duty=50%" end)
probe("i2c",    function() return hardware.i2c_read(0x38, 0x0F, 1) end)
probe("gpio",   function() hardware.gpio_write(2, 1) return "p2=1" end)
probe("flash",  function() hardware.flash_write(0x100, "HELLO") return hardware.flash_read(0x100, 5) end)
probe("ble",    function() hardware.ble_send("lua-ok") return "sent" end)

-- Enterprise control loop example: overheat -> drive the fan.
local temp = hardware.sensor_read("temp")
if temp > 85.0 then
    hardware.gpio_write(1, 1) -- fan on
end
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
// Persistent storage mapped to ESP-IDF NVS. `flash_write(addr, data)` stores a
// blob under key `flash_<addr:08x>`; `flash_read` reads it back (up to `len`);
// `flash_erase_sector` removes the key. NVS keys are <=15 chars, so
// `flash_` + 8 hex digits + NUL = 15 — fits. NVS is already initialised by
// `main::init_default_nvs()` before the Lua task starts, so we just `nvs_open`
// here (no `nvs_flash_init`, which would conflict with the esp-idf-svc wrapper).
const NVS_NAMESPACE: &[u8] = b"magent_lua\0";

/// Open the `magent_lua` NVS namespace read-write.
fn nvs_open_rw() -> Result<sys::nvs_handle_t, String> {
    let mut handle: sys::nvs_handle_t = 0;
    // SAFETY: raw ESP-IDF NVS open; `NVS_NAMESPACE` is a NUL-terminated C string.
    let r = unsafe {
        sys::nvs_open(
            NVS_NAMESPACE.as_ptr() as *const core::ffi::c_char,
            sys::nvs_open_mode_t_NVS_READWRITE,
            &mut handle,
        )
    };
    if r != 0 {
        return Err(format!("nvs_open err={r}"));
    }
    Ok(handle)
}

// ---------------------------------------------------------------------------
// App source (operator-updatable `main.lua` stored in NVS)
// ---------------------------------------------------------------------------
// NVS key holding the operator-provided `main.lua` source. At boot we prefer
// this over the embedded `DEFAULT_MAIN_LUA`, so operators can update the app
// without reflashing (write it with [`set_lua_app_source`]). Key <=15 chars.
const NVS_APP_KEY: &[u8] = b"main.lua\0";

/// Read the operator-provided `main.lua` from NVS, if present.
fn nvs_read_app_source() -> Result<Option<String>, String> {
    let handle = nvs_open_rw()?;
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

/// Persist `main.lua` into NVS so a future boot runs it (operator update path).
///
/// This is the device-side API surface; it is wired from an ingress/AT/BLE
/// command or a host tool. Returns an error if the source is too large for a
/// single NVS value or the write fails.
#[allow(dead_code)] // operator-update entry point; wired from an ingress/AT command
pub fn set_lua_app_source(source: &str) -> Result<(), String> {
    let handle = nvs_open_rw()?;
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

/// NVS key for a flash address: `flash_%08x` (14 chars + NUL).
fn nvs_key(addr: u32) -> [core::ffi::c_char; 15] {
    // `c_char` is `u8` on Xtensa/ESP-IDF and `i8` on x86_64 — stay portable.
    let mut key = [0 as core::ffi::c_char; 15];
    let s = format!("flash_{addr:08x}");
    for (dst, b) in key.iter_mut().zip(s.as_bytes()) {
        *dst = *b as core::ffi::c_char;
    }
    key
}

fn flash_write_device(addr: u32, data: &[u8]) -> Result<(), String> {
    let handle = nvs_open_rw()?;
    let key = nvs_key(addr);
    // SAFETY: raw ESP-IDF NVS blob set; returns an `esp_err_t`.
    let r = unsafe {
        sys::nvs_set_blob(
            handle,
            key.as_ptr(),
            data.as_ptr() as *const core::ffi::c_void,
            data.len(),
        )
    };
    let c = if r == 0 { unsafe { sys::nvs_commit(handle) } } else { r };
    // SAFETY: raw ESP-IDF NVS handle close (safe even after a failed op).
    unsafe { sys::nvs_close(handle) };
    if c != 0 {
        return Err(format!("flash_write(addr=0x{addr:08x}) err={c}"));
    }
    Ok(())
}

fn flash_read_device(addr: u32, len: usize) -> Result<Vec<u8>, String> {
    let handle = nvs_open_rw()?;
    let key = nvs_key(addr);
    // First query the stored length (out_value=NULL, length=0).
    let mut required: usize = 0;
    // SAFETY: raw ESP-IDF NVS blob length query; returns an `esp_err_t`.
    let q = unsafe {
        sys::nvs_get_blob(handle, key.as_ptr(), std::ptr::null_mut(), &mut required)
    };
    if q == sys::ESP_ERR_NVS_NOT_FOUND {
        // SAFETY: raw ESP-IDF NVS handle close.
        unsafe { sys::nvs_close(handle) };
        return Err(format!("flash_read(addr=0x{addr:08x}): key not found"));
    }
    if q != 0 {
        // SAFETY: raw ESP-IDF NVS handle close.
        unsafe { sys::nvs_close(handle) };
        return Err(format!("flash_read(addr=0x{addr:08x}) query err={q}"));
    }
    if required == 0 {
        // SAFETY: raw ESP-IDF NVS handle close.
        unsafe { sys::nvs_close(handle) };
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; len.min(required)];
    let mut out_len = buf.len();
    // SAFETY: raw ESP-IDF NVS blob read; returns an `esp_err_t`.
    let r = unsafe {
        sys::nvs_get_blob(
            handle,
            key.as_ptr(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut out_len,
        )
    };
    // SAFETY: raw ESP-IDF NVS handle close.
    unsafe { sys::nvs_close(handle) };
    if r != 0 {
        return Err(format!("flash_read(addr=0x{addr:08x}) err={r}"));
    }
    buf.truncate(out_len);
    Ok(buf)
}

fn flash_erase_sector_device(addr: u32) -> Result<(), String> {
    let handle = nvs_open_rw()?;
    let key = nvs_key(addr);
    // SAFETY: raw ESP-IDF NVS key erase; returns an `esp_err_t`.
    let r = unsafe { sys::nvs_erase_key(handle, key.as_ptr()) };
    let c = if r == 0 { unsafe { sys::nvs_commit(handle) } } else { r };
    // SAFETY: raw ESP-IDF NVS handle close.
    unsafe { sys::nvs_close(handle) };
    if c != 0 {
        return Err(format!("flash_erase_sector(addr=0x{addr:08x}) err={c}"));
    }
    Ok(())
}

impl HardwareBackend for Esp32Hardware {
    fn gpio_write(&mut self, pin: u8, level: u8) -> std::result::Result<(), String> {
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
                    sys::temperature_sensor_get_celsius(handle, &mut celsius);
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
        // ADC1 oneshot (GPIO1..=10), 12-bit, DB_11; see `adc_read_pin`.
        adc_read_pin(pin)
    }

    fn pwm_set(&mut self, pin: u8, duty: u8) -> std::result::Result<(), String> {
        // LEDC PWM, 8-bit resolution, duty 0..=100 %. See `pwm_set_duty`.
        pwm_set_duty(pin, duty)
    }

    // `data` is consumed only in the `ble`-feature branch.
    #[cfg_attr(not(feature = "ble"), allow(unused_variables))]
    fn ble_send(&mut self, data: &[u8]) -> std::result::Result<(), String> {
        // Push to the connected BLE client on SYS_RSP via the firmware's GATT
        // server (`ble_config`). Only available when built with `--features ble`.
        #[cfg(feature = "ble")]
        {
            return crate::ble_config::ble_send_payload(data);
        }
        #[cfg(not(feature = "ble"))]
        {
            Err("ble_send: firmware built without the `ble` feature".into())
        }
    }

    fn power_set(&mut self, _profile: u8) -> std::result::Result<(), String> {
        // ESP32 power states are managed by the firmware / ESP-IDF; no-op.
        Ok(())
    }
}

/// Build and run the Lua application runtime on a dedicated thread.
///
/// The non-`Send` `AppRuntime` (it owns the sandboxed `mlua::Lua`) is
/// constructed **inside** the spawned closure, so the closure only captures
/// `Send` values and `std::thread` accepts it. Everything is created on the
/// new thread and never moved across it.
pub fn start_lua_task() {
    let r = crate::core_affinity::spawn_thread(
        "lua-thread",
        32 * 1024,
        crate::core_affinity::ThreadProfile::REALTIME_AGENT,
        move || {
            // NOTE (audit-2026-08): the Lua app host is intentionally NOT
            // subscribed to the RT watchdog. Its event loop (`AppRuntime::
            // run_until_stop`) is a long-lived black box that never calls
            // `esp_task_wdt_reset()`, so subscribing it here would make even a
            // *normal* long-running Lua script false-trip the 18s watchdog and
            // reboot the board. The watchdog protects the critical real-time
            // paths (agent + ingress); the Lua host is best-effort and its
            // stalls don't affect those. Its LLM/fetch_web feeds are no-ops.
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
                if let (Some(model), Some(key)) = (
                    crate::nvs_load_string(crate::NVS_KEY_LLM_MODEL),
                    crate::nvs_load_string(crate::NVS_KEY_LLM_API_KEY),
                ) {
                    if !model.is_empty() && !key.is_empty() {
                        log::info!(
                            "[lua] installing DeepSeek LLM backend via Core-0 worker (model={model})"
                        );
                        // P1 (REQ-SCHED-001): route the blocking DeepSeek call to
                        // a Core-0 worker so the Lua app host (Core 1) never blocks
                        // on TLS/HTTP. See llm.rs `ChannelLlmBackend`.
                        let (llm_tx, llm_rx) = std::sync::mpsc::channel::<crate::llm::LlmRequest>();
                        let worker = crate::core_affinity::spawn_thread(
                            "llm-worker",
                            24 * 1024,
                            crate::core_affinity::ThreadProfile::IO_NETWORK,
                            move || {
                                crate::llm::run_llm_worker(
                                    llm_rx,
                                    crate::llm::Esp32DeepSeekBackend::new(&model, &key),
                                );
                            },
                        );
                        if worker.is_err() {
                            log::warn!("[lua] LLM worker spawn failed — Lua uses local heuristic");
                        } else {
                            let backend: &'static mut crate::llm::ChannelLlmBackend =
                                Box::leak(Box::new(crate::llm::ChannelLlmBackend::new(llm_tx)));
                            // HARDENING (audit-2026-08 H9): register the leaked
                            // pointer for duplicate-leak detection, mirroring the
                            // main agent boot path.
                            if !crate::leaked_boxes().insert(backend as *mut _ as usize) {
                                log::error!(
                                    "[lua] leaking a duplicate LLM backend (same pointer \
                                     as a previous leak); refactor leak site"
                                );
                            }
                            agent.set_llm_backend(backend);
                        }
                    }
                }
            }
            let agent = Arc::new(Mutex::new(agent));
            let vm = PiccoloVm::new(hardware.clone(), agent);
            let mut app = AppRuntime::new(vm, hardware);
            // Prefer the operator's NVS copy of `main.lua`; fall back to the
            // embedded bootstrap script.
            let app_src = load_app_source();
            if let Err(e) = app.boot(&app_src) {
                log::error!("[lua] boot main.lua: {e}");
                return;
            }
            log::info!("[lua] AppRuntime booted; entering event loop");

            // Drive the loop until a supervisor sets the stop flag (e.g. on
            // OTA/reboot). Per-tick errors are contained by the runtime.
            app.run_until_stop(Duration::from_millis(50), None);
            log::info!("[lua] loop stopped");
        },
    );
    match r {
        Ok(_) => log::info!("[lua] thread started"),
        Err(e) => log::error!("[lua] thread spawn failed: {e}"),
    }
}
