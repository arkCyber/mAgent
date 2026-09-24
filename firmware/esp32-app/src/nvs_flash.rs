//! NVS-backed "flash" emulation, shared by the Lua runtime and the agent tools.
//!
//! There is no memory-mapped NOR on the ESP32 that user code may write freely,
//! so both the Lua `hardware.flash_*` API and the agent's `flash_read` /
//! `flash_write` tools map an *address* onto an ESP-IDF NVS blob keyed
//! `flash_<addr:08x>`. NVS keys are at most 15 chars (`flash_` + 8 hex digits
//! + NUL = 15), which is exactly what fits.
//!
//! The namespace is `magent_lua`, kept verbatim from the original Lua-only
//! implementation so blobs written by earlier firmware remain readable. NVS is
//! already initialised by `main::init_default_nvs()` before any caller runs, so
//! this module only opens the namespace — calling `nvs_flash_init` here would
//! conflict with the `esp-idf-svc` wrapper that already owns it.

use esp_idf_sys as sys;

/// NVS namespace holding every emulated flash blob.
const NAMESPACE: &[u8] = b"magent_lua\0";

/// Open [`NAMESPACE`] read-write.
pub fn open_rw() -> Result<sys::nvs_handle_t, String> {
    let mut handle: sys::nvs_handle_t = 0;
    // SAFETY: raw ESP-IDF NVS open; `NAMESPACE` is a NUL-terminated C string.
    let r = unsafe {
        sys::nvs_open(
            NAMESPACE.as_ptr() as *const core::ffi::c_char,
            sys::nvs_open_mode_t_NVS_READWRITE,
            &mut handle,
        )
    };
    if r != 0 {
        return Err(format!("nvs_open err={r}"));
    }
    Ok(handle)
}

/// NVS key for a flash address: `flash_%08x` (14 chars + NUL).
pub fn key(addr: u32) -> [core::ffi::c_char; 15] {
    // `c_char` is `u8` on Xtensa/ESP-IDF and `i8` on x86_64 — stay portable.
    let mut key = [0 as core::ffi::c_char; 15];
    let s = format!("flash_{addr:08x}");
    for (dst, b) in key.iter_mut().zip(s.as_bytes()) {
        *dst = *b as core::ffi::c_char;
    }
    key
}

/// Store `data` at `addr`, replacing anything already there.
pub fn write(addr: u32, data: &[u8]) -> Result<(), String> {
    let handle = open_rw()?;
    let k = key(addr);
    // SAFETY: raw ESP-IDF NVS blob set; returns an `esp_err_t`.
    let r = unsafe {
        sys::nvs_set_blob(
            handle,
            k.as_ptr(),
            data.as_ptr() as *const core::ffi::c_void,
            data.len(),
        )
    };
    let c = if r == 0 {
        unsafe { sys::nvs_commit(handle) }
    } else {
        r
    };
    // SAFETY: raw ESP-IDF NVS handle close (safe even after a failed op).
    unsafe { sys::nvs_close(handle) };
    if c != 0 {
        return Err(format!("flash_write(addr=0x{addr:08x}) err={c}"));
    }
    Ok(())
}

/// Read up to `len` bytes stored at `addr`.
///
/// Errors when nothing is stored there — a caller that asked for a specific
/// address must not be handed an empty buffer as if the read had succeeded.
pub fn read(addr: u32, len: usize) -> Result<Vec<u8>, String> {
    let handle = open_rw()?;
    let k = key(addr);
    // First query the stored length (out_value=NULL, length=0).
    let mut required: usize = 0;
    // SAFETY: raw ESP-IDF NVS blob length query; returns an `esp_err_t`.
    let q = unsafe { sys::nvs_get_blob(handle, k.as_ptr(), std::ptr::null_mut(), &mut required) };
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
            k.as_ptr(),
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

/// Remove the blob stored at `addr`.
///
/// Only the Lua runtime's `flash_erase_sector` calls this — the agent's tool
/// set has no erase — so it is dead when built without the `lua` feature.
#[allow(dead_code)]
pub fn erase(addr: u32) -> Result<(), String> {
    let handle = open_rw()?;
    let k = key(addr);
    // SAFETY: raw ESP-IDF NVS key erase; returns an `esp_err_t`.
    let r = unsafe { sys::nvs_erase_key(handle, k.as_ptr()) };
    let c = if r == 0 {
        unsafe { sys::nvs_commit(handle) }
    } else {
        r
    };
    // SAFETY: raw ESP-IDF NVS handle close.
    unsafe { sys::nvs_close(handle) };
    if c != 0 {
        return Err(format!("flash_erase_sector(addr=0x{addr:08x}) err={c}"));
    }
    Ok(())
}
