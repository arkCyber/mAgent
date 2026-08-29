//! Over-The-Air firmware update (REQ-FW-005).
//!
//! Streams a new application image from an HTTP(S) URL into the *inactive* OTA
//! partition slot, asks ESP-IDF to verify it, marks it as the next boot target,
//! and reboots. If the download or image verification fails, the OTA is aborted
//! and the current firmware keeps running untouched; if a later boot of the new
//! image does not call `esp_ota_mark_app_valid_cancel_rollback`, ESP-IDF rolls
//! back to the previous slot (when anti-rollback is enabled).
//!
//! ## Safety & production notes
//! - A bounded HTTP timeout prevents a dead server from hanging the caller.
//! - The image is written in small chunks (no large heap buffer), so it works
//!   within the RAM-limited C61 task-stack budget.
//! - Every failure path either aborts the OTA handle (freeing ESP-IDF's OTA
//!   state) or returns without touching the boot partition, so a failed update
//!   can never brick the running firmware.
//! - Requires an OTA-capable partition table (`ota_0`/`ota_1` + `otadata`). The
//!   stock `partitions.csv` provides this; an OTA-only table is required for
//!   anti-rollback (see `docs/BACKLOG.md`).

use std::time::Duration;

use embedded_svc::http::client::Client as HttpClient;
use embedded_svc::http::Method;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

/// Per-`esp_ota_write` chunk size. Small enough for the ingress thread stack,
/// large enough that a 2 MB image doesn't take forever.
const OTA_CHUNK: usize = 2048;
/// Bounded HTTP request timeout (seconds).
const OTA_TIMEOUT_S: u64 = 30;

/// Stream the firmware image at `url` into the inactive OTA slot and reboot
/// into it. Returns `Err(reason)` on any failure without changing the boot
/// target (the running firmware continues).
pub fn perform_ota(url: &str) -> Result<(), String> {
    // 1) Confirm the running app is valid so anti-rollback accepts the next
    //    update. Ignore errors — this is a best-effort precondition.
    // SAFETY: `esp_ota_mark_app_valid_cancel_rollback` takes no pointers.
    unsafe { esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback() };

    // 2) Find the next (inactive) OTA app partition.
    // SAFETY: passing a null `start_from` asks ESP-IDF to auto-select the next
    // update partition; the returned pointer is valid for the lifetime of the
    // partition table.
    let partition = unsafe { esp_idf_sys::esp_ota_get_next_update_partition(core::ptr::null()) };
    if partition.is_null() {
        return Err("OTA: no next-update partition (need ota_0/ota_1 in partition table)".into());
    }

    // 3) Begin the OTA write. `OTA_SIZE_UNKNOWN` erases the whole target slot.
    let mut handle: esp_idf_sys::esp_ota_handle_t = 0;
    // SAFETY: `partition` is a valid `esp_partition_t*`; `handle` is written by
    // the callee and valid after `esp_ota_begin` returns ESP_OK.
    let rc = unsafe {
        esp_idf_sys::esp_ota_begin(
            partition,
            esp_idf_sys::OTA_SIZE_UNKNOWN as usize,
            &mut handle,
        )
    };
    if rc != esp_idf_sys::ESP_OK {
        return Err(format!("OTA: esp_ota_begin failed (0x{:x})", rc));
    }

    // 4) Stream the image over HTTP and write it chunk-by-chunk.
    let cfg = HttpConfig {
        timeout: Some(Duration::from_secs(OTA_TIMEOUT_S)),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let conn = EspHttpConnection::new(&cfg).map_err(|e| format!("OTA: conn: {e}"))?;
    let mut client = HttpClient::wrap(conn);
    let request = client
        .request(Method::Get, url, &[])
        .map_err(|e| format!("OTA: request: {e}"))?;
    let mut response = request.submit().map_err(|e| format!("OTA: submit: {e}"))?;
    if response.status() != 200 {
        // SAFETY: handle was successfully created above; abort frees its state.
        let _ = unsafe { esp_idf_sys::esp_ota_abort(handle) };
        return Err(format!("OTA: HTTP status {}", response.status()));
    }

    let mut chunk = [0u8; OTA_CHUNK];
    loop {
        let n = response.read(&mut chunk).map_err(|e| {
            // SAFETY: abort a live handle on read failure; ignore abort's result.
            let _ = unsafe { esp_idf_sys::esp_ota_abort(handle) };
            format!("OTA: read: {e}")
        })?;
        if n == 0 {
            break;
        }
        // SAFETY: `chunk` is a valid buffer for the call; `handle` is live.
        let rc = unsafe {
            esp_idf_sys::esp_ota_write(handle, chunk.as_ptr() as *const core::ffi::c_void, n)
        };
        if rc != esp_idf_sys::ESP_OK {
            // SAFETY: abort a live handle on write failure.
            let _ = unsafe { esp_idf_sys::esp_ota_abort(handle) };
            return Err(format!("OTA: esp_ota_write failed (0x{:x})", rc));
        }
    }

    // 5) Finalise — `esp_ota_end` verifies the image (magic + checksums).
    // SAFETY: `handle` is live; `esp_ota_end` consumes it (do not reuse).
    let rc = unsafe { esp_idf_sys::esp_ota_end(handle) };
    if rc != esp_idf_sys::ESP_OK {
        return Err(format!("OTA: image verification failed (0x{:x})", rc));
    }

    // 6) Select the new partition as boot target and reboot.
    // SAFETY: `partition` is the slot we just wrote and verified.
    let rc = unsafe { esp_idf_sys::esp_ota_set_boot_partition(partition) };
    if rc != esp_idf_sys::ESP_OK {
        return Err(format!("OTA: set_boot_partition failed (0x{:x})", rc));
    }
    log::info!("[ota] image verified; rebooting into OTA slot");
    // SAFETY: `esp_restart` never returns, so this is the end of the function
    // (the never type coerces to `Result<(), String>`).
    unsafe { esp_idf_sys::esp_restart() };
}
