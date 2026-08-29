//! A small persistent key-value store on top of the flash backend — the
//! "survives a reboot" configuration store an enterprise App needs.
//!
//! Layout (`NVRAM_BASE`..`NVRAM_BASE + NVRAM_CAPACITY`):
//!
//! ```text
//! [key_len:u8][value_len:u8][key bytes][value bytes]  (repeated)
//! 0xFF   ← erased byte, signals end-of-store
//! ```
//!
//! `set`/`remove` rebuild the whole region (erase + write), which is correct
//! and simple; a device with hardware NVS would map `get`/`set` onto it.
//! Uninitialised / corrupt headers are surfaced as errors, never a panic.

use crate::hardware::HardwareBackend;

/// First address of the NVRAM region.
pub const NVRAM_BASE: u32 = 0x4000;
/// Size of the NVRAM region (a single NrfFlash sector).
pub const NVRAM_CAPACITY: usize = 4096;
/// Maximum key length in bytes.
pub const MAX_KEY_LEN: usize = 32;
/// Maximum value length in bytes.
pub const MAX_VALUE_LEN: usize = 128;
/// Erased flash byte; doubles as the end-of-store marker.
const END: u8 = 0xFF;

/// Read the value stored for `key`, or `None` if absent.
pub fn get(hw: &mut dyn HardwareBackend, key: &str) -> std::result::Result<Option<String>, String> {
    validate_key(key)?;
    let mut off = 0usize;
    loop {
        let hdr = hw.flash_read(NVRAM_BASE + off as u32, 2)?;
        let (klen, vlen) = (hdr[0], hdr[1]);
        if klen == END {
            return Ok(None);
        }
        let entry_len = entry_len(klen, vlen, off)?;
        let body = hw.flash_read(NVRAM_BASE + off as u32 + 2, entry_len - 2)?;
        if body[..klen as usize] == key.as_bytes()[..] {
            let value = String::from_utf8(body[klen as usize..].to_vec())
                .map_err(|_| "nvram: value is not UTF-8".to_string())?;
            return Ok(Some(value));
        }
        off += entry_len;
    }
}

/// Store `value` for `key`, overwriting any previous value.
pub fn set(
    hw: &mut dyn HardwareBackend,
    key: &str,
    value: &str,
) -> std::result::Result<(), String> {
    validate_key(key)?;
    if value.len() > MAX_VALUE_LEN {
        return Err(format!(
            "nvram: value too long ({} > {MAX_VALUE_LEN})",
            value.len()
        ));
    }
    let mut entries = read_all(hw)?;
    entries.retain(|(k, _)| k != key);
    entries.push((key.to_string(), value.to_string()));
    rebuild(hw, &entries)
}

/// Remove `key` if present. Absence is not an error.
pub fn remove(hw: &mut dyn HardwareBackend, key: &str) -> std::result::Result<(), String> {
    validate_key(key)?;
    let mut entries = read_all(hw)?;
    entries.retain(|(k, _)| k != key);
    rebuild(hw, &entries)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn validate_key(key: &str) -> std::result::Result<(), String> {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return Err(format!("nvram: invalid key length {}", key.len()));
    }
    Ok(())
}

/// Compute the on-flash length of an entry, bounds-checked against capacity.
fn entry_len(klen: u8, vlen: u8, off: usize) -> std::result::Result<usize, String> {
    if klen == 0 || klen as usize > MAX_KEY_LEN {
        return Err(format!("nvram: corrupt key length {klen} at offset {off}"));
    }
    if vlen as usize > MAX_VALUE_LEN {
        return Err(format!(
            "nvram: corrupt value length {vlen} at offset {off}"
        ));
    }
    let len = 2 + klen as usize + vlen as usize;
    if off + len > NVRAM_CAPACITY {
        return Err(format!("nvram: region overflow at offset {off}"));
    }
    Ok(len)
}

/// Scan the region into a list of (key, value) pairs.
fn read_all(hw: &mut dyn HardwareBackend) -> std::result::Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut off = 0usize;
    loop {
        let hdr = hw.flash_read(NVRAM_BASE + off as u32, 2)?;
        let (klen, vlen) = (hdr[0], hdr[1]);
        if klen == END {
            break;
        }
        let len = entry_len(klen, vlen, off)?;
        let body = hw.flash_read(NVRAM_BASE + off as u32 + 2, len - 2)?;
        let k = String::from_utf8(body[..klen as usize].to_vec())
            .map_err(|_| "nvram: key is not UTF-8".to_string())?;
        let v = String::from_utf8(body[klen as usize..].to_vec())
            .map_err(|_| "nvram: value is not UTF-8".to_string())?;
        out.push((k, v));
        off += len;
    }
    Ok(out)
}

/// Erase the region and write `entries` back followed by the end marker.
fn rebuild(
    hw: &mut dyn HardwareBackend,
    entries: &[(String, String)],
) -> std::result::Result<(), String> {
    // Bounds-check the serialised size before erasing, so a config that does
    // not fit does not destroy the existing store.
    let needed: usize = entries
        .iter()
        .map(|(k, v)| 2 + k.len() + v.len())
        .sum::<usize>()
        + 1; // + terminator
    if needed > NVRAM_CAPACITY {
        return Err(format!("nvram: store full ({needed} > {NVRAM_CAPACITY})"));
    }

    hw.flash_erase_sector(NVRAM_BASE)?;
    let mut off = 0usize;
    for (k, v) in entries {
        let mut buf = Vec::with_capacity(2 + k.len() + v.len());
        buf.push(k.len() as u8);
        buf.push(v.len() as u8);
        buf.extend_from_slice(k.as_bytes());
        buf.extend_from_slice(v.as_bytes());
        hw.flash_write(NVRAM_BASE + off as u32, &buf)?;
        off += buf.len();
    }
    hw.flash_write(NVRAM_BASE + off as u32, &[END])
}
