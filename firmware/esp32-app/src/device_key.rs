//! Device-bound seal key plumbing for the ESP32-C61 firmware.
//!
//! This module is the firmware-side glue between three concerns:
//!
//!   1. **Hardware-bound material** — MAC + eFuse BLOCK0 + chip
//!      revision, read at boot from ESP32 silicon.
//!   2. **Boot-time-derived key (BTDK1)** — Keccak256 mixing done
//!      in `magent_core::boot_key`.
//!   3. **Sealed storage of `dev_identity`** — the Ed25519 seed
//!      that used to be plaintext in NVS is now wrapped with
//!      BTDK1 so a flash-only attacker cannot recover it.
//!
//! The chicken-and-egg layering is:
//!
//! ```text
//!   eFuse / chip revision
//!     │
//!     ▼  Keccak256
//!   BTDK1 key (transient, re-derived each boot)
//!     │
//!     ▼  DBO1 XOR-stream
//!   sealed dev_identity (NVS)
//!     │
//!     ▼  opens to raw 32-byte seed
//!   seal key for wifi_pass, api tokens, …
//! ```
//!
//! Every transition is fail-closed: if hardware material cannot be
//! read, or the BTDK1 derivation fails, or the stored blob cannot
//! be opened, callers receive an `Err` and refuse to operate. We
//! never substitute an empty / zero / constant key, because that
//! would silently break the entire seal chain.
//!
//! Host testability note: this module calls `esp_idf_sys::esp_efuse_*`
//! FFI directly. It only compiles for the ESP32 target; it has no
//! host-side test path (the cryptographic mixing is exercised
//! via `magent_core::boot_key` instead).

use magent_core::wifi_pass_seal;
use heapless::String as HeaplessString;

/// Wire-format prefix used by the sealed form of `dev_identity`.
/// Mirrors the convention `wifi_pass_seal` uses (`DBO1:` for its
/// sealed form) so a generic "sealed vs legacy" detector can
/// recognise both without algorithm-specific logic.
pub const BTDK1_PREFIX: &str = "BTDK1:";

/// Read hardware-bound material for the boot-time key derivation.
///
/// Returns up to [`magent_core::boot_key::MAX_MATERIAL_LEN`] bytes
/// composed of:
///   1. Factory MAC address (6 bytes, from eFuse)
///   2. eFuse BLOCK0 first 32 bytes (raw register contents; on a
///      stock C61 this contains the silicon-unique id, wafer
///      coordinates, lot info, etc.)
///   3. Package version (4 bytes, from eFuse)
///
/// If BLOCK0 read fails (e.g. older silicon with BLOCK0 read
/// protection), we degrade gracefully to MAC + pkg_ver (10 bytes
/// total) — still enough unique material for a per-device key,
/// just with weaker collision resistance than the BLOCK0 case.
pub fn read_btdk_material()
-> Result<heapless::Vec<u8, { magent_core::boot_key::MAX_MATERIAL_LEN }>, &'static str> {
    use magent_core::boot_key::MAX_MATERIAL_LEN;
    let mut mat: heapless::Vec<u8, MAX_MATERIAL_LEN> = heapless::Vec::new();

    // 1. Factory MAC (6 bytes). Always succeeds on a functional chip.
    let mut mac = [0u8; 6];
    let rc = unsafe { esp_idf_sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if rc != 0 {
        return Err("btdk:mac_read_failed");
    }
    mat.extend_from_slice(&mac).map_err(|_| "btdk:material_full")?;

    // 2. eFuse BLOCK0 raw register bytes (32 bytes). Optional;
    // degrades gracefully if the chip blocks the read.
    let mut blk0 = [0u8; 32];
    let rc = unsafe {
        esp_idf_sys::esp_efuse_read_block(
            esp_idf_sys::esp_efuse_block_t_EFUSE_BLK0,
            blk0.as_mut_ptr() as *mut core::ffi::c_void,
            0,    // offset_bits
            256,  // size_bits = 32 bytes
        )
    };
    if rc == 0 {
        mat.extend_from_slice(&blk0).map_err(|_| "btdk:material_full")?;
    } else {
        log::warn!(
            "[magent] BTDK1: BLOCK0 read failed (rc={rc}); falling back to MAC-only material"
        );
    }

    // 3. Package version (4 bytes). Always succeeds on a functional SoC.
    let pkg_ver = unsafe { esp_idf_sys::esp_efuse_get_pkg_ver() } as u32;
    mat.extend_from_slice(&pkg_ver.to_le_bytes())
        .map_err(|_| "btdk:material_full")?;

    if mat.is_empty() {
        return Err("btdk:material_empty");
    }
    log::info!(
        "[magent] BTDK1 material: {} bytes (MAC + BLOCK0 + pkg_ver)",
        mat.len()
    );
    Ok(mat)
}

/// Derive the 32-byte boot-time seal key from hardware material.
pub fn derive_btdk() -> Result<[u8; 32], &'static str> {
    use magent_core::boot_key;
    let material = read_btdk_material()?;
    let key = boot_key::derive(&material).map_err(|e| match e {
        boot_key::BootKeyError::MaterialEmpty => "btdk:material_empty",
        boot_key::BootKeyError::MaterialTooLong => "btdk:material_too_long",
        // Feature not compiled in — the caller can't derive a key.
        boot_key::BootKeyError::FeatureDisabled => "btdk:feature_disabled",
    })?;
    Ok(*key.as_bytes())
}

// ---------------------------------------------------------------------------
// Sealed `dev_identity` storage
// ---------------------------------------------------------------------------
//
// Wire format:
//   "BTDK1:" || hex(sealed_seed)
//
// Legacy plaintext: 64 hex chars (32 raw bytes), no prefix.
// On first boot after this change, legacy values are migrated
// transparently to the BTDK1-sealed form so subsequent reads
// always go through the sealed path.

/// Maximum NVS storage length for `dev_identity` in BTDK1 form.
/// = `BTDK1_PREFIX.len()` + `wifi_pass_seal::MAX_ENCODED_LEN`.
///
/// NOTE: the prefix is `"BTDK1:"` which is **6** chars, not 5. The previous
/// `5 + MAX_ENCODED_LEN` was one byte short, so sealing a 32-byte seed
/// (`6 + 157 = 163 > 162`) overflowed and `seal_dev_identity` always failed
/// with `persist_full` — leaving `dev_identity` stuck as legacy plaintext.
pub const DEV_IDENTITY_STORED_MAX: usize = BTDK1_PREFIX.len() + wifi_pass_seal::MAX_ENCODED_LEN;

/// Detect the wire-format prefix of a `dev_identity` NVS value.
/// Returns true if the value starts with the BTDK1 prefix.
///
/// Not referenced by the boot path (which calls `open_dev_identity`
/// directly); it exists as a host-testable prefix detector for the
/// integration tests in `firmware/esp32-app/tests/`.
#[allow(dead_code)]
#[inline]
pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(BTDK1_PREFIX)
}

/// Open a `dev_identity` NVS value, returning the 32 raw seed
/// bytes regardless of whether it was stored as legacy plaintext
/// or as a BTDK1-sealed blob.
///
/// `stored` is the raw NVS string. The function:
///   - Detects `BTDK1:` prefix → sealed form → opens with BTDK1 key.
///   - Otherwise treats it as legacy 64-char hex → returns as-is.
///
/// On success with a legacy value, the function also re-seals
/// in place so the next boot goes through the BTDK1 path. The
/// re-seal is best-effort: a failure is logged but does not
/// affect the bytes returned to the caller.
pub fn open_dev_identity(stored: &str) -> Result<[u8; 32], &'static str> {
    if let Some(sealed_payload) = stored.strip_prefix(BTDK1_PREFIX) {
        // Sealed form. Open with the BTDK1 key.
        let key = derive_btdk()?;
        let mut out: heapless::Vec<u8, { wifi_pass_seal::MAX_PLAINTEXT }> = heapless::Vec::new();
        match wifi_pass_seal::open_sealed_bytes(sealed_payload, &key, &mut out) {
            Ok(wifi_pass_seal::OpenOutcome::DecodedBytes) => {
                // The sealed plaintext is the seed in one of two encodings:
                //   * 32 bytes  = the raw seed (current),
                //   * 64 hex    = the seed's hex encoding (legacy
                //                 `seal_dev_identity`).
                // Accept both so a sealed entry never gets stuck in a
                // regenerate loop.
                if out.len() == 32 {
                    let mut seed = [0u8; 32];
                    seed.copy_from_slice(&out);
                    Ok(seed)
                } else if out.len() == 64 {
                    let bytes = &out[..64];
                    let mut seed = [0u8; 32];
                    for i in 0..32 {
                        let hi = hex_nibble(bytes[i * 2]).ok_or("dev_identity non-hex")?;
                        let lo = hex_nibble(bytes[i * 2 + 1]).ok_or("dev_identity non-hex")?;
                        seed[i] = (hi << 4) | lo;
                    }
                    Ok(seed)
                } else {
                    Err("btdk:open_wrong_length")
                }
            }
            Ok(wifi_pass_seal::OpenOutcome::LegacyPlaintext(_)) => {
                // strip_prefix succeeded but open_sealed_bytes reports
                // legacy — corruption.
                Err("btdk:open_unexpected_legacy")
            }
            Err(e) => {
                log::error!("[magent] BTDK1 open of dev_identity failed: {e:?}");
                Err("btdk:open_failed")
            }
        }
    } else if stored.len() == 64 {
        // Legacy plaintext (64 hex chars = 32 bytes seed).
        let mut seed = [0u8; 32];
        let bytes = stored.as_bytes();
        for i in 0..32 {
            let hi = hex_nibble(bytes[i * 2]).ok_or("dev_identity non-hex")?;
            let lo = hex_nibble(bytes[i * 2 + 1]).ok_or("dev_identity non-hex")?;
            seed[i] = (hi << 4) | lo;
        }
        // Best-effort re-seal so future boots take the BTDK1 path.
        if let Err(e) = seal_and_store_dev_identity(&seed) {
            log::warn!("[magent] BTDK1 re-seal of legacy dev_identity failed: {e}");
        } else {
            log::info!("[magent] legacy plaintext dev_identity migrated to BTDK1 sealed form");
        }
        Ok(seed)
    } else {
        Err("dev_identity wrong length")
    }
}

/// Seal a 32-byte identity seed with the BTDK1 key and return
/// the wire-format string to persist under `magent:dev_identity`.
/// (The persistence itself is the caller's responsibility — this
/// module is intentionally I/O-free so it stays host-testable as
/// much as the FFI permits.)
pub fn seal_dev_identity(seed: &[u8; 32]) -> Result<HeaplessString<DEV_IDENTITY_STORED_MAX>, &'static str> {

    let key = derive_btdk()?;
    let mut nonce = [0u8; wifi_pass_seal::NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|_| "trng_unavailable")?;

    // Reuse wifi_pass_seal::seal_str on the hex representation of
    // the seed. This keeps the seal algorithm identical across
    // secrets; only the key source differs.
    let mut plain_hex = [0u8; 64];
    let hex_chars = b"0123456789abcdef";
    for (i, &b) in seed.iter().enumerate() {
        plain_hex[i * 2] = hex_chars[(b >> 4) as usize];
        plain_hex[i * 2 + 1] = hex_chars[(b & 0xf) as usize];
    }
    let plain_str = core::str::from_utf8(&plain_hex).map_err(|_| "hex_utf8")?;

    let mut sealed: HeaplessString<{ wifi_pass_seal::MAX_ENCODED_LEN }> = HeaplessString::new();
    wifi_pass_seal::seal_str(plain_str, &key, &nonce, &mut sealed)
        .map_err(|_| "seal_failed")?;

    let mut persisted: HeaplessString<DEV_IDENTITY_STORED_MAX> = HeaplessString::new();
    persisted
        .push_str(BTDK1_PREFIX)
        .map_err(|_| "persist_prefix_too_long")?;
    persisted
        .push_str(sealed.as_str())
        .map_err(|_| "persist_full")?;
    Ok(persisted)
}

/// Convenience wrapper that seals `seed` AND persists it under
/// `magent:dev_identity` in one call. Used by `load_or_create_identity`
/// (boot path) and `ident_rot_dispatch` (AT path).
pub fn seal_and_store_dev_identity(seed: &[u8; 32]) -> Result<(), &'static str> {
    let persisted = seal_dev_identity(seed)?;
    crate::nvs_save_string(crate::NVS_KEY_IDENTITY, persisted.as_str())
        .map_err(|_| "nvs_save_failed")?;
    Ok(())
}

/// Convenience: load and open `magent:dev_identity`, returning the
/// raw 32-byte seed.
pub fn load_device_key_via_btdk() -> Result<[u8; 32], &'static str> {
    let stored = crate::nvs_load_string(crate::NVS_KEY_IDENTITY).ok_or("dev_identity missing")?;
    open_dev_identity(&stored)
}

#[inline]
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
//
// The FFI-backed helpers (`read_btdk_material`, `derive_btdk`,
// `seal_dev_identity`, `open_dev_identity`'s sealed branch) only
// run on the ESP32 target. Tests for them live in
// `firmware/esp32-app/tests/device_key_integration_tests.rs` which
// is exercised by the hardware-in-the-loop CI job.
//
// We CAN exercise the wire-format detection (`is_sealed`) and the
// error-class dispatch (`open_dev_identity` legacy failure modes)
// here, because those don't touch FFI.
