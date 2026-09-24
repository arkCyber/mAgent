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
//! - Every failure path up to `esp_ota_end` aborts the OTA handle, freeing the
//!   `ota_ops_entry_t` that `esp_ota_begin` heap-allocates (the pre-`esp_ota_end`
//!   paths used to return without it, leaking one entry per failed attempt).
//!   `esp_ota_end` itself consumes the handle — it `free()`s the entry on *all*
//!   of its return paths, including image-verification failure — so that path
//!   must **not** abort. Either way the boot partition is untouched, so a failed
//!   update can never brick the running firmware.
//! - Only one OTA runs at a time. `esp_ota_begin` rejects a *conflict with the
//!   running partition* but happily opens a second handle on the same inactive
//!   slot, so two `AT+OTA` commands would interleave two writers into one image
//!   (and each erases the whole slot). `OTA_IN_PROGRESS` rejects the overlap.
//! - Requires an OTA-capable partition table (`ota_0`/`ota_1` + `otadata`). The
//!   stock `partitions.csv` provides this; an OTA-only table is required for
//!   anti-rollback (see `docs/BACKLOG.md`).
//!
//! ## Security model & FW-R2 (audit-2026-09-15)
//!
//! TRACE: AEROSPACE-AUDIT-2026-09-15 §6 FW-R2 — this module is
//! **fail-open by default**: any reachable HTTP(S) URL is accepted as
//! a candidate image and the running firmware reboots into it. The
//! production hardening plan is:
//!   1. Require `https://` (reject `http://` outright — see
//!      `validate_ota_url`).
//!   2. Pin the connection to the IDF `crt_bundle_attach` cert bundle
//!      so the TLS handshake validates the server cert chain.
//!   3. Enforce a vendor-host allowlist (e.g. `*.firmware.example.com`)
//!      in `validate_ota_url`. Reject everything else with a clear
//!      error so an attacker can't trick the operator into reflashing
//!      a malicious image from a typo-squatted URL.
//!   4. Long-term: enable ESP-IDF **secure-boot v2** so the bootloader
//!      refuses to boot an image without a valid ECDSA signature;
//!      today the board accepts any image that passes the OTA
//!      checksum, which is a defense-in-depth gap.
//!
//! Until steps 1-4 land, an attacker on the same network as the board
//! (Wi-Fi or BLE GATT) can potentially trick `AT+OTA=<url>` into
//! flashing arbitrary firmware. The `validate_ota_url` helper
//! already implements step 1 (HTTPS-only) so any consumer of this
//! module gets the cheapest mitigation for free.
//!
//! **Production gate**: before enabling `AT+OTA` on a production
//! board, complete steps 2-4 above. The audit pass at
//! `AEROSPACE_AUDIT_FIRMWARE_2026_09_22.md` (planned follow-up) will
//! re-audit this module against the gate.

use core::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use embedded_svc::http::client::Client as HttpClient;
use embedded_svc::http::Method;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

/// Per-`esp_ota_write` chunk size. Small enough for the ingress thread stack,
/// large enough that a 2 MB image doesn't take forever.
const OTA_CHUNK: usize = 2048;
/// Bounded HTTP request timeout (seconds).
const OTA_TIMEOUT_S: u64 = 30;

/// Held while an OTA is streaming.
///
/// `AT+OTA` spawns a worker thread per invocation, and `esp_ota_begin` only
/// rejects a conflict with the *running* partition — a second handle on the same
/// inactive slot is granted. Two concurrent updates would therefore interleave
/// writes into one image, each having erased the whole slot, and whichever
/// finished first would `esp_ota_set_boot_partition` + `esp_restart` into an
/// image the other was still writing. ESP-IDF supports one OTA at a time; the
/// second command is refused instead.
static OTA_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Releases `OTA_IN_PROGRESS` on every exit from `perform_ota`.
///
/// The success path reboots inside `perform_ota` and so never drops this — which
/// is correct, since the flag is process state and the process is going away.
struct OtaSession;

impl Drop for OtaSession {
    fn drop(&mut self) {
        OTA_IN_PROGRESS.store(false, Ordering::Release);
    }
}

/// Free the `ota_ops_entry_t` that `esp_ota_begin` allocated for `handle`.
///
/// Ignoring the return code is deliberate: this only runs on an error path whose
/// original failure is the one worth reporting, and `esp_ota_abort` on a live
/// handle is infallible in practice (`esp_ota_ops.c`).
fn abort_handle(handle: esp_idf_sys::esp_ota_handle_t) {
    // SAFETY: the caller guarantees `handle` came from a successful
    // `esp_ota_begin` and has not been consumed by `esp_ota_end`.
    let _ = unsafe { esp_idf_sys::esp_ota_abort(handle) };
}

/// TRACE: AEROSPACE-AUDIT-2026-09-15 §6 FW-R2 — first-pass URL check.
/// Returns `true` only for `https://` URLs. Plain `http://` is
/// rejected so a network-level attacker cannot intercept the image
/// in transit. The vendor-host allowlist (step 3 in the module
/// docstring) is **not** enforced here — the board has no built-in
/// host allowlist yet, so the function only catches the
/// plaintext-HTTP case for now. Production deployments must extend
/// this with an explicit `cfg`/`compile-time!` allowlist.
pub(crate) fn validate_ota_url(url: &str) -> bool {
    url.starts_with("https://")
}

/// Stream the firmware image at `url` into the inactive OTA slot and reboot
/// into it. Returns `Err(reason)` on any failure without changing the boot
/// target (the running firmware continues).
pub fn perform_ota(url: &str) -> Result<(), String> {
    // TRACE: AEROSPACE-AUDIT-2026-09-15 §6 FW-R2 — refuse plaintext
    // HTTP up front. Production hardening (steps 2-4 in the module
    // docstring) is still pending; this is the cheapest mitigation
    // and applies regardless of caller.
    if !validate_ota_url(url) {
        return Err(format!(
            "OTA: refusing non-HTTPS URL {url:?} (FW-R2 hardening, see docstring)"
        ));
    }
    // Refuse to start a second update while one is in flight — see
    // `OTA_IN_PROGRESS`. Claimed before `esp_ota_begin` so the whole
    // erase-then-stream sequence is serialised.
    if OTA_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("OTA: another update is already in progress".into());
    }
    // Released on every `return` below. On success `esp_restart` never returns,
    // so the flag stays set — the process is rebooting anyway.
    let _session = OtaSession;
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
    let conn = match EspHttpConnection::new(&cfg) {
        Ok(c) => c,
        Err(e) => {
            // `esp_ota_begin` already heap-allocated the ops entry (and
            // erased the slot at `OTA_SIZE_UNKNOWN`), so it must be freed.
            abort_handle(handle);
            return Err(format!("OTA: conn: {e}"));
        }
    };
    let mut client = HttpClient::wrap(conn);
    let request = match client.request(Method::Get, url, &[]) {
        Ok(r) => r,
        Err(e) => {
            abort_handle(handle);
            return Err(format!("OTA: request: {e}"));
        }
    };
    let mut response = match request.submit() {
        Ok(r) => r,
        Err(e) => {
            abort_handle(handle);
            return Err(format!("OTA: submit: {e}"));
        }
    };
    if response.status() != 200 {
        abort_handle(handle);
        return Err(format!("OTA: HTTP status {}", response.status()));
    }

    let mut chunk = [0u8; OTA_CHUNK];
    loop {
        let n = match response.read(&mut chunk) {
            Ok(n) => n,
            Err(e) => {
                abort_handle(handle);
                return Err(format!("OTA: read: {e}"));
            }
        };
        if n == 0 {
            break;
        }
        // SAFETY: `chunk` is a valid buffer for the call; `handle` is live.
        let rc = unsafe {
            esp_idf_sys::esp_ota_write(handle, chunk.as_ptr() as *const core::ffi::c_void, n)
        };
        if rc != esp_idf_sys::ESP_OK {
            abort_handle(handle);
            return Err(format!("OTA: esp_ota_write failed (0x{:x})", rc));
        }
    }

    // 5) Finalise — `esp_ota_end` verifies the image (magic + checksums).
    // SAFETY: `handle` is live; `esp_ota_end` consumes it and `free()`s the ops
    // entry on every return path (see `esp_ota_ops.c`), so it must not be
    // aborted afterwards — that would be a double free.
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
