//! Flash storage layer for mAgent
//!
//! Provides embedded storage using Flash memory with wear leveling
//! and error correction for aerospace-grade reliability.

use crate::error::{AgentError, IntoStorageError, Result, StorageError};
use embedded_storage::nor_flash::NorFlash;
use heapless::Vec;

/// CRC-16/CCITT-FALSE over `data`: polynomial `0x1021`, init `0xFFFF`,
/// no input/output reflection and no final XOR.
///
/// This is the **single** integrity function for the KV wire format
/// (`[key_len][key][value_len LE][value][crc16 LE]`). It replaces the
/// historical `crc ^= byte; crc = crc.wrapping_mul(0x1021)` expression,
/// which — despite the `0x1021` constant — was a multiplication-based
/// checksum rather than a polynomial CRC and detected far fewer error
/// patterns (e.g. two corruptions of the same byte value cancel out).
/// Because both the write and *both* read paths now route through this
/// function, entries validate consistently.
///
/// Test vectors, per the CRC catalogue:
/// - `b""`          → `0xFFFF`
/// - `b"123456789"` → `0x29B1`
pub(crate) fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Flash storage wrapper
pub struct FlashStorage<F> {
    flash: F,
    sector_size: usize,
    page_size: usize,
}

impl<F> FlashStorage<F>
where
    F: NorFlash,
{
    /// Create a new flash storage
    pub fn new(flash: F) -> Self {
        let sector_size = F::ERASE_SIZE;
        let page_size = F::WRITE_SIZE;

        Self {
            flash,
            sector_size,
            page_size,
        }
    }

    /// Read data from flash
    pub fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<()> {
        let address = offset as usize;

        // Validate address
        if !address.is_multiple_of(self.page_size) {
            return Err(AgentError::StorageReadFailed {
                address: offset,
                reason: StorageError::BadAddress,
            });
        }

        self.flash
            .read(address as u32, buf)
            .map_err(|_| AgentError::StorageReadFailed {
                address: offset,
                reason: StorageError::ReadError,
            })?;

        Ok(())
    }

    /// Write data to flash
    pub fn write(&mut self, offset: u32, data: &[u8]) -> Result<()> {
        let address = offset as usize;

        // Validate address alignment
        if !address.is_multiple_of(self.page_size) {
            return Err(AgentError::StorageWriteFailed {
                address: offset,
                reason: StorageError::BadAddress,
            });
        }

        // Validate data size
        if !data.len().is_multiple_of(self.page_size) {
            return Err(AgentError::StorageWriteFailed {
                address: offset,
                reason: StorageError::BadAddress,
            });
        }

        // NOTE (audit-2026-08): we do NOT erase the sector here. Erasing
        // the whole sector on every program would destroy every other entry
        // sharing that sector, which is why the KV store never survived a
        // second `set` in the same sector. NOR flash program is a 1→0
        // operation: callers write to already-erased (0xFF) space, and
        // explicitly `erase()` when reclaiming. This makes multi-entry
        // sectors (and compaction's erase-then-rewrite) work.

        // Write data
        self.flash
            .write(address as u32, data)
            .map_err(|_| AgentError::StorageWriteFailed {
                address: offset,
                reason: StorageError::WriteProtected,
            })?;

        Ok(())
    }

    /// Erase a sector
    pub fn erase(&mut self, sector: u32) -> Result<()> {
        let sector_start = (sector as usize) * self.sector_size;
        let sector_end = sector_start + self.sector_size;

        self.flash
            .erase(sector_start as u32, sector_end as u32)
            .map_err(|_| AgentError::StorageWriteFailed {
                address: sector * self.sector_size as u32,
                reason: StorageError::EraseError,
            })?;

        Ok(())
    }

    /// Get sector size
    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    /// Get page size
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Total capacity of the underlying flash, in bytes.
    pub fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

/// Simple key-value store in flash
pub struct KvStore<F> {
    storage: FlashStorage<F>,
    base_address: u32,
}

/// Maximum number of live entries buffered during a compaction pass.
const MAX_COMPACT_ENTRIES: usize = 256;

/// Largest possible KV entry header, i.e. `[key_len:u8][key:1..=32]
/// [value_len:u16]`. Readers read this many bytes up front so they can
/// locate `value_len` at `1 + key_len` regardless of key size.
const KV_HEADER_MAX: usize = 1 + 32 + 2;

/// Maximum value length `set` will accept. Enforced on write and on
/// header parse, so `entry_size` can never exceed `3 + 32 + 256 + 2 = 293`
/// — comfortably inside the 512-byte scan buffers every reader uses.
const MAX_VALUE_LEN: usize = 256;

/// Decoded header of a single KV entry.
///
/// The wire format is `[key_len:u8][key: key_len][value_len:u16 LE]
/// [value: value_len][crc:u16 LE]` — i.e. `value_len` sits **after** the
/// key, at byte offset `1 + key_len`. (A previous revision read it from
/// bytes 1–2, which collided with the first two key bytes and silently
/// corrupted the scan of every non-empty key.)
#[derive(Clone, Copy, Debug)]
struct KvHeader {
    /// Key length.
    key_len: usize,
    /// Value length.
    value_len: usize,
    /// Total bytes the entry occupies (`3 + key_len + value_len + 2`).
    entry_size: usize,
}

/// Decode a KV entry header from a buffer of at least [`KV_HEADER_MAX`]
/// bytes. Returns `None` for end-of-store (`buf[0] == 0xFF`, i.e. erased)
/// or a corrupt length (`0` or `> 32`), signalling the caller to stop
/// scanning.
///
/// `value_len` must also be rejected above [`MAX_VALUE_LEN`]. It is a
/// full `u16` read straight out of flash, so without this check a
/// corrupt-but-plausible header (valid key, garbage value length) yields
/// an `entry_size` up to `3 + 32 + 65535 + 2 = 65572`. Callers size
/// their read buffers at 512 bytes and slice with the header's lengths,
/// so such a header would panic with an out-of-range slice instead of
/// surfacing `StorageError::CorruptedData`. Every writer enforces the
/// 256-byte cap in `set`, so an over-long `value_len` is always
/// corruption — same policy as the `key_len` check above.
fn parse_kv_header(buf: &[u8]) -> Option<KvHeader> {
    if buf.len() < KV_HEADER_MAX {
        return None;
    }
    let key_len = buf[0];
    if key_len == 0 || key_len > 32 {
        return None; // erased (0xFF), tombstoned/corrupt (0), or corrupt
    }
    let value_off = 1 + key_len as usize;
    let value_len = u16::from_le_bytes([buf[value_off], buf[value_off + 1]]) as usize;
    if value_len > MAX_VALUE_LEN {
        return None; // corrupt value length (writers cap at MAX_VALUE_LEN)
    }
    Some(KvHeader {
        key_len: key_len as usize,
        value_len,
        entry_size: 3 + key_len as usize + value_len + 2,
    })
}

impl<F> KvStore<F>
where
    F: NorFlash,
{
    /// Create a new KV store
    ///
    /// Precondition: `base_address` must be **sector-aligned**, and the store's
    /// region must end on a sector boundary. Every scan here bounds itself by
    /// `FlashStorage::capacity()` treated as a *length* from `base_address`, and
    /// `compact` reclaims space with [`Self::erase_all`], which erases whole
    /// sectors: with an unaligned `base_address` the first erased sector starts
    /// *before* the store, taking unrelated flash with it. Every current caller
    /// passes 0, where both hold trivially — the constraint is recorded here
    /// because violating it destroys a neighbour's data rather than failing.
    pub fn new(storage: FlashStorage<F>, base_address: u32) -> Self {
        Self {
            storage,
            base_address,
        }
    }

    /// Get a value by key
    pub fn get(&mut self, key: &str) -> Result<Option<Vec<u8, 256>>> {
        // Validate key length
        if key.len() > 32 {
            return Err(AgentError::InputValidationFailed {
                field: "key",
                reason: crate::error::ValidationError::TooLong,
            });
        }

        // KV storage format:
        // [key_len: u8][key: key_len][value_len: u16][value: value_len][crc: u16]
        // Scan flash starting from base_address

        let mut offset: u32 = 0;
        let mut buf = [0u8; 512];
        // Bound the scan by the region the store actually owns. A hardcoded
        // 64 KiB (as this was) walks past the end of a smaller region once the
        // store fills it, so the next header read faults and a plain lookup on
        // a *full* store reports `StorageReadFailed` — a hardware fault — where
        // the answer is simply "key absent". `set` and `collect_live` already
        // bound themselves with `capacity`; this now matches them.
        let capacity = self.storage.capacity() as u32;

        while offset < capacity {
            // HARDENING (audit-2026-08 H1): previously a flash read
            // error here was silently absorbed (`break` -> return
            // `Ok(None)`), so a transient I/O fault on header byte 0
            // looked identical to "key not present" to callers. We now
            // propagate the underlying `AgentError` so callers can
            // distinguish hardware failure from a legitimate miss.
            self.storage
                .read(self.base_address + offset, &mut buf[..KV_HEADER_MAX])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // `buf[0] == 0xFF` (erased) or a corrupt length → end of store.
            let Some(hdr) = parse_kv_header(&buf[..KV_HEADER_MAX]) else {
                break;
            };
            let key_len = hdr.key_len;
            let value_len = hdr.value_len;
            let entry_size = hdr.entry_size;

            // Read full entry
            self.storage
                .read(self.base_address + offset, &mut buf[..entry_size])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // Check if key matches (key starts at byte 1, after key_len).
            let stored_key = &buf[1..1 + key_len];
            if stored_key == key.as_bytes() {
                // Extract value
                let value_start = 3 + key_len;
                let value_end = value_start + value_len;

                // Validate CRC (CRC-16/CCITT-FALSE over the header + body)
                let crc_offset = value_end;
                let stored_crc = u16::from_le_bytes([buf[crc_offset], buf[crc_offset + 1]]);
                let crc = crc16_ccitt(&buf[..crc_offset]);

                if crc != stored_crc {
                    // CRC mismatch, data corrupted
                    return Err(AgentError::StorageReadFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    });
                }

                let mut result = Vec::new();
                for i in 0..value_len {
                    // HARDENING (audit-2026-08 H1): previously a value
                    // overflow silently truncated the returned data;
                    // we now surface a typed error so callers know the
                    // value was too large for the bounded buffer.
                    result.push(buf[value_start + i]).map_err(|_| {
                        AgentError::InputValidationFailed {
                            field: "value",
                            reason: crate::error::ValidationError::TooLong,
                        }
                    })?;
                }
                return Ok(Some(result));
            }

            offset += entry_size as u32;
        }

        Ok(None)
    }

    /// Set a value by key
    pub fn set(&mut self, key: &str, value: &[u8]) -> Result<()> {
        // Validate key length
        if key.is_empty() {
            return Err(AgentError::InputValidationFailed {
                field: "key",
                reason: crate::error::ValidationError::Empty,
            });
        }
        if key.len() > 32 {
            return Err(AgentError::InputValidationFailed {
                field: "key",
                reason: crate::error::ValidationError::TooLong,
            });
        }

        if value.len() > MAX_VALUE_LEN {
            return Err(AgentError::InputValidationFailed {
                field: "value",
                reason: crate::error::ValidationError::TooLong,
            });
        }

        // KV storage format:
        // [key_len: u8][key: key_len][value_len: u16][value: value_len][crc: u16]

        let key_len = key.len() as u8;
        let value_len = value.len() as u16;

        // Calculate entry size
        let entry_size = 3 + key_len as usize + value_len as usize + 2;

        // Build entry buffer
        let mut entry = Vec::<u8, 512>::new();
        let _ = entry.push(key_len);
        for &byte in key.as_bytes() {
            let _ = entry.push(byte);
        }
        let _ = entry.push((value_len & 0xFF) as u8);
        let _ = entry.push(((value_len >> 8) & 0xFF) as u8);
        for &byte in value {
            let _ = entry.push(byte);
        }

        // CRC-16/CCITT-FALSE trailer over the header + body.
        let crc = crc16_ccitt(&entry);
        let _ = entry.push((crc & 0xFF) as u8);
        let _ = entry.push(((crc >> 8) & 0xFF) as u8);

        // Find free space in flash
        let mut offset: u32 = 0;
        let mut buf = [0u8; 512];

        // Walk live entries to find the first free slot that fits this entry
        // in its entirety. Each live entry is advanced by its *actual*
        // on-wire size (via `parse_kv_header`), not by this entry's size.
        let capacity = self.storage.capacity() as u32;
        let read_len = entry_size.max(KV_HEADER_MAX);
        let mut found = false;
        while offset + entry_size as u32 <= capacity {
            self.storage
                .read(self.base_address + offset, &mut buf[..read_len])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // Whole slot erased (0xFF) → free space.
            if buf[..entry_size].iter().all(|&b| b == 0xFF) {
                found = true;
                break;
            }

            // Live entry → advance by its real size.
            let Some(hdr) = parse_kv_header(&buf[..KV_HEADER_MAX]) else {
                return Err(AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: StorageError::CorruptedData,
                });
            };
            offset += hdr.entry_size as u32;
        }
        if !found {
            return Err(AgentError::StorageWriteFailed {
                address: self.base_address + offset,
                reason: StorageError::OutOfSpace,
            });
        }

        // Writing into erased (0xFF) space needs no erase. The old
        // "erase sector if crossing a boundary" block was removed: it
        // would have wiped unrelated live entries sharing that sector.

        // Write entry
        self.storage.write(self.base_address + offset, &entry)?;

        Ok(())
    }

    /// Serialise `key`/`value` into the wire format and write at
    /// `base_address + offset`. Returns the next free offset.
    fn write_entry_at(&mut self, offset: u32, key: &[u8], value: &[u8]) -> Result<u32> {
        let key_len = key.len() as u8;
        let value_len = value.len() as u16;
        let entry_size = 3 + key_len as usize + value_len as usize + 2;
        let mut entry = Vec::<u8, 512>::new();
        let _ = entry.push(key_len);
        for &b in key {
            let _ = entry.push(b);
        }
        let _ = entry.push((value_len & 0xFF) as u8);
        let _ = entry.push(((value_len >> 8) & 0xFF) as u8);
        for &b in value {
            let _ = entry.push(b);
        }
        // CRC-16/CCITT-FALSE trailer over the header + body.
        let crc = crc16_ccitt(&entry);
        let _ = entry.push((crc & 0xFF) as u8);
        let _ = entry.push(((crc >> 8) & 0xFF) as u8);
        self.storage.write(self.base_address + offset, &entry)?;
        Ok(offset + entry_size as u32)
    }

    /// Erase every sector the store spans, so a subsequent compaction can
    /// rewrite the surviving entries from a clean (0xFF) slate.
    fn erase_all(&mut self) -> Result<()> {
        let sector = self.storage.sector_size();
        let capacity = self.storage.capacity();
        let start_sector = (self.base_address as usize / sector) as u32;
        let end_byte = self.base_address as usize + capacity;
        let end_sector = end_byte.div_ceil(sector) as u32;
        let mut s = start_sector;
        while s < end_sector {
            self.storage.erase(s)?;
            s += 1;
        }
        Ok(())
    }

    /// Scan all live entries into a bounded buffer, optionally skipping the
    /// one whose key equals `skip_key`. Used by delete / compaction.
    fn collect_live(
        &mut self,
        skip_key: Option<&str>,
    ) -> Result<heapless::Vec<(heapless::Vec<u8, 32>, heapless::Vec<u8, 256>), MAX_COMPACT_ENTRIES>>
    {
        let mut live: heapless::Vec<_, MAX_COMPACT_ENTRIES> = heapless::Vec::new();
        let mut offset = 0u32;
        let mut buf = [0u8; 512];
        let capacity = self.storage.capacity() as u32;

        while offset < capacity {
            self.storage
                .read(self.base_address + offset, &mut buf[..KV_HEADER_MAX])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;
            let Some(hdr) = parse_kv_header(&buf[..KV_HEADER_MAX]) else {
                break; // end of store (erased / corrupt header)
            };
            let entry_size = hdr.entry_size;
            self.storage
                .read(self.base_address + offset, &mut buf[..entry_size])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            let key = &buf[1..1 + hdr.key_len];
            if !skip_key.is_some_and(|sk| key == sk.as_bytes()) {
                let mut k = heapless::Vec::<u8, 32>::new();
                let mut v = heapless::Vec::<u8, 256>::new();
                k.extend_from_slice(key)
                    .map_err(|_| AgentError::StorageWriteFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    })?;
                v.extend_from_slice(&buf[3 + hdr.key_len..3 + hdr.key_len + hdr.value_len])
                    .map_err(|_| AgentError::StorageWriteFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    })?;
                live.push((k, v))
                    .map_err(|_| AgentError::StorageWriteFailed {
                        address: self.base_address + offset,
                        reason: StorageError::OutOfSpace,
                    })?;
            }

            offset += entry_size as u32;
        }
        Ok(live)
    }

    /// Erase the whole store and rewrite the surviving live entries.
    /// Returns the number of entries rewritten. `skip_key` entries are
    /// physically removed (this is how delete works — no tombstones left
    /// behind, so the store never accumulates dead space).
    fn compact(&mut self, skip_key: Option<&str>) -> Result<usize> {
        let live = self.collect_live(skip_key)?;
        self.erase_all()?;
        let mut offset = 0u32;
        for (k, v) in &live {
            offset = self.write_entry_at(offset, k, v)?;
        }
        Ok(live.len())
    }

    /// Delete a key
    pub fn delete(&mut self, key: &str) -> Result<()> {
        // Validate key length
        if key.is_empty() {
            return Ok(()); // nothing to delete
        }
        if key.len() > 32 {
            return Err(AgentError::InputValidationFailed {
                field: "key",
                reason: crate::error::ValidationError::TooLong,
            });
        }
        // Deleting a missing key is idempotent: compaction rewrites the
        // store unchanged.
        self.compact(Some(key))?;
        Ok(())
    }

    /// Garbage collection: compact flash by rewriting only the live entries.
    /// Returns the number of surviving entries.
    pub fn garbage_collect(&mut self) -> Result<usize> {
        self.compact(None)
    }

    /// Get statistics about the KV store
    pub fn get_stats(&mut self) -> Result<KvStoreStats> {
        let mut offset: u32 = 0;
        let mut buf = [0u8; 512];
        let mut total_entries = 0;
        let mut valid_entries = 0;
        let mut corrupted_entries = 0;
        let mut used_space = 0;
        // As in `get`: scan only the region the store owns, not a hardcoded
        // 64 KiB — otherwise a store whose capacity is smaller faults once it
        // fills, and the reported figures are taken over bytes it does not own.
        let capacity = self.storage.capacity() as u32;

        while offset < capacity {
            // HARDENING (audit-2026-08 H1): propagate read failures
            // so `get_stats` reports incomplete data only on success
            // (with the partial-counts caveat) rather than silently
            // dropping faulted entries.
            self.storage
                .read(self.base_address + offset, &mut buf[..KV_HEADER_MAX])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // `buf[0] == 0xFF` (erased) or a corrupt length → end of store.
            let Some(hdr) = parse_kv_header(&buf[..KV_HEADER_MAX]) else {
                break;
            };
            let key_len = hdr.key_len;
            let value_len = hdr.value_len;
            let entry_size = hdr.entry_size;

            total_entries += 1;

            // Read full entry
            self.storage
                .read(self.base_address + offset, &mut buf[..entry_size])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // Validate CRC (CRC-16/CCITT-FALSE over the header + body)
            let crc_offset = 3 + key_len + value_len;
            let stored_crc = u16::from_le_bytes([buf[crc_offset], buf[crc_offset + 1]]);
            let crc = crc16_ccitt(&buf[..crc_offset]);

            if crc == stored_crc {
                valid_entries += 1;
                used_space += entry_size;
            } else {
                corrupted_entries += 1;
            }

            offset += entry_size as u32;
        }

        Ok(KvStoreStats {
            total_entries,
            valid_entries,
            // Compaction physically removes deleted entries, so there are
            // never tombstones left behind to count.
            deleted_entries: 0,
            corrupted_entries,
            used_space,
            // `capacity - used_space`, not `65536 - used_space`: the hardcoded
            // constant reported a nonsensical free space for any region that
            // was not exactly 64 KiB, and would underflow outright (panicking
            // under debug overflow checks) once `used_space` passed 64 KiB.
            free_space: (capacity as usize).saturating_sub(used_space),
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    //! These tests pin down the post-H1 contract of `KvStore` and
    //! `FlashStorage`: every flash error surfaces as a typed
    //! `AgentError::StorageReadFailed` (or `StorageWriteFailed`).
    //! The previous implementation silently collapsed every error to
    //! `Ok(None)` / `Ok(())`, which is exactly the regression these
    //! tests exist to catch.
    //!
    //! Only enabled with `--features std` because we need to construct
    //! an in-memory mock that implements `embedded_storage::nor_flash`.

    use super::*;

    // -----------------------------------------------------------------
    // CRC-16/CCITT-FALSE (KV entry integrity)
    // -----------------------------------------------------------------

    /// The canonical CRC-16/CCITT-FALSE check value: `"123456789"` → `0x29B1`.
    #[test]
    fn crc16_ccitt_known_vectors() {
        assert_eq!(crc16_ccitt(b""), 0xFFFF);
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        // Short-payload values, computed with the same bitwise algorithm,
        // pin the implementation against regressions.
        assert_eq!(crc16_ccitt(&[0x00]), 0xE1F0);
        assert_eq!(crc16_ccitt(&[0xFF]), 0xFF00);
        assert_eq!(crc16_ccitt(b"AB"), 0x4B74);
    }

    /// A polynomial CRC-16 must detect every single-bit flip, whereas the
    /// historical `crc ^= byte; crc *= 0x1021` checksum did not. This is the
    /// property that motivated the change, so assert it explicitly.
    #[test]
    fn crc16_ccitt_detects_every_single_bit_flip() {
        let base = b"config-key\0\x01\x02\x03payload-bytes";
        let baseline = crc16_ccitt(base);
        for byte_idx in 0..base.len() {
            for bit in 0..8 {
                let mut mutated = base.to_vec();
                mutated[byte_idx] ^= 1 << bit;
                assert_ne!(
                    crc16_ccitt(&mutated),
                    baseline,
                    "single-bit flip at byte {byte_idx} bit {bit} was not detected"
                );
            }
        }
    }

    /// Swapping two adjacent bytes must change the CRC (a classic weakness of
    /// additive/XOR checksums).
    #[test]
    fn crc16_ccitt_detects_adjacent_swap() {
        assert_ne!(
            crc16_ccitt(b"abcdef"),
            crc16_ccitt(b"abdcef"),
        );
    }

    // The trait impls below write `Result<(), Self::Error>`. The crate root
    // re-exports `Result<T>` as a 1-arg alias fixed to `AgentError`, which
    // would mis-resolve every embedded-storage trait signature. Shadow it
    // with the standard 2-arg `core::result::Result` inside this module.
    use core::result::Result;
    // `super::*` also brings in `heapless::Vec`; the mock's in-memory backing
    // store needs the standard growable `std::vec::Vec`.
    use std::vec::Vec;

    /// Mock flash that lets each test pin point read failures.
    #[derive(Debug)]
    struct MockFlash {
        data: Vec<u8>,
        fail_from: Option<u32>,
    }

    impl MockFlash {
        fn new(capacity: usize) -> Self {
            Self {
                data: vec![0xFFu8; capacity],
                fail_from: None,
            }
        }

        fn fail_from(&mut self, addr: u32) {
            self.fail_from = Some(addr);
        }

        fn should_fail(&self, addr: u32) -> bool {
            match self.fail_from {
                Some(base) => addr >= base,
                None => false,
            }
        }
    }

    #[derive(Debug)]
    struct MockErr;

    impl core::fmt::Display for MockErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "mock flash error")
        }
    }

    // `embedded_storage::nor_flash::NorFlashError` (0.3.x) requires a
    // `kind()` accessor; map the mock error to the generic `Other` kind.
    impl embedded_storage::nor_flash::NorFlashError for MockErr {
        fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
            embedded_storage::nor_flash::NorFlashErrorKind::Other
        }
    }

    impl embedded_storage::nor_flash::ErrorType for MockFlash {
        type Error = MockErr;
    }

    impl embedded_storage::nor_flash::ReadNorFlash for MockFlash {
        const READ_SIZE: usize = 1;

        fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            if self.should_fail(address) {
                return Err(MockErr);
            }
            let start = address as usize;
            // The previous revision clamped `end` to `data.len()` and *then*
            // tested `end > data.len()` — a guard that could never fire. An
            // out-of-range read therefore fell through to `copy_from_slice`
            // with a short source slice and panicked, instead of returning the
            // error the real HAL flash returns. Leaving `end` unclamped keeps
            // the double honest about the boundary it is meant to model.
            let Some(end) = start.checked_add(buf.len()) else {
                return Err(MockErr);
            };
            if end > self.data.len() {
                return Err(MockErr);
            }
            buf.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.data.len()
        }
    }

    impl embedded_storage::nor_flash::NorFlash for MockFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 4096;

        fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
            if self.should_fail(address) {
                return Err(MockErr);
            }
            let start = address as usize;
            let end = start + data.len();
            if end > self.data.len() {
                return Err(MockErr);
            }
            self.data[start..end].copy_from_slice(data);
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            if self.should_fail(from) {
                return Err(MockErr);
            }
            let s = from as usize;
            let e = (to as usize).min(self.data.len());
            for byte in &mut self.data[s..e] {
                *byte = 0xFF;
            }
            Ok(())
        }
    }

    impl embedded_storage::ReadStorage for MockFlash {
        type Error = MockErr;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            <Self as embedded_storage::nor_flash::ReadNorFlash>::read(self, offset, bytes)
        }

        fn capacity(&self) -> usize {
            <Self as embedded_storage::nor_flash::ReadNorFlash>::capacity(self)
        }
    }

    impl embedded_storage::Storage for MockFlash {
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            <Self as embedded_storage::nor_flash::NorFlash>::write(self, offset, bytes)
        }
    }

    fn empty_kv() -> KvStore<MockFlash> {
        let flash = MockFlash::new(4096);
        let storage = FlashStorage::new(flash);
        KvStore::new(storage, 0)
    }

    #[test]
    fn get_returns_none_on_clean_flash() {
        let mut kv = empty_kv();
        assert!(matches!(kv.get("missing"), Ok(None)));
    }

    #[test]
    fn get_round_trips_written_value() {
        let mut kv = empty_kv();
        kv.set("hello", b"world").unwrap();
        let v = kv.get("hello").unwrap().unwrap();
        assert_eq!(&v[..], b"world");
    }

    #[test]
    fn get_propagates_flash_read_error_h1() {
        // HARDENING (audit-2026-08 H1): a flash read error inside
        // `get` must surface as `StorageReadFailed`, never collapse
        // to `Ok(None)`.
        let mut kv = empty_kv();
        kv.storage.flash.fail_from(0);
        match kv.get("anything") {
            Err(AgentError::StorageReadFailed { address: 0, .. }) => {}
            other => panic!("expected StorageReadFailed@0, got {other:?}"),
        }
    }

    #[test]
    fn set_propagates_flash_read_error_h1() {
        let mut kv = empty_kv();
        kv.storage.flash.fail_from(0);
        let r = kv.set("k", b"v");
        assert!(
            matches!(r, Err(AgentError::StorageReadFailed { .. })),
            "expected StorageReadFailed, got {r:?}"
        );
    }

    #[test]
    fn delete_propagates_flash_read_error_h1() {
        let mut kv = empty_kv();
        kv.storage.flash.fail_from(0);
        let r = kv.delete("k");
        assert!(
            matches!(r, Err(AgentError::StorageReadFailed { .. })),
            "expected StorageReadFailed, got {r:?}"
        );
    }

    #[test]
    fn delete_removes_key() {
        let mut kv = empty_kv();
        kv.set("alpha", b"value-1").unwrap();
        assert_eq!(kv.get("alpha").unwrap().unwrap(), b"value-1");
        kv.delete("alpha").unwrap();
        assert!(
            matches!(kv.get("alpha"), Ok(None)),
            "key must be gone after delete"
        );
    }

    #[test]
    fn delete_then_reset_returns_new_value() {
        // delete() compacts, physically removing the old entry; a subsequent
        // set() with the same key writes a fresh entry.
        let mut kv = empty_kv();
        kv.set("k", b"old").unwrap();
        kv.delete("k").unwrap();
        kv.set("k", b"new").unwrap();
        assert_eq!(kv.get("k").unwrap().unwrap(), b"new");
    }

    #[test]
    fn get_stats_counts_entries_after_delete_compaction() {
        let mut kv = empty_kv();
        kv.set("a", b"1").unwrap();
        kv.set("b", b"2").unwrap();
        kv.set("c", b"3").unwrap();
        kv.delete("b").unwrap(); // compaction physically removes "b"

        let stats = kv.get_stats().unwrap();
        assert_eq!(
            stats.total_entries, 2,
            "deleted entry is physically compacted away"
        );
        assert_eq!(
            stats.deleted_entries, 0,
            "no tombstones remain after compaction"
        );
        assert_eq!(stats.valid_entries, 2);
    }

    #[test]
    fn lookup_on_a_full_store_is_a_clean_miss_not_a_read_fault() {
        // Each entry is 3 + 1 + 250 + 2 = 256 bytes, so exactly 16 fill the
        // 4096-byte mock region and the scan cursor lands exactly on capacity.
        let mut kv = empty_kv();
        let value = [0x41u8; 250];
        for i in 0..16u8 {
            let key = alloc::format!("{}", (b'a' + i) as char);
            kv.set(&key, &value).expect("entry fits");
        }

        // The store is full. The scan used to keep going to a hardcoded 64 KiB,
        // reading a header one byte past the end of the region: a lookup for an
        // absent key returned `StorageReadFailed` (a hardware fault) instead of
        // a clean miss.
        assert_eq!(kv.get("z").unwrap(), None);
        // The same hardcoded bound made the stats scan fault on a full store.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.valid_entries, 16);
        assert_eq!(stats.used_space, 4096);
        assert_eq!(stats.free_space, 0);
    }

    #[test]
    fn delete_of_missing_key_is_ok() {
        let mut kv = empty_kv();
        assert!(
            kv.delete("nope").is_ok(),
            "deleting a missing key should be idempotent"
        );
    }

    #[test]
    fn set_rejects_empty_key() {
        let mut kv = empty_kv();
        assert!(
            matches!(
                kv.set("", b"v"),
                Err(AgentError::InputValidationFailed { .. })
            ),
            "empty key must be rejected so key_len==0 stays unambiguous as end-of-store"
        );
    }

    #[test]
    fn garbage_collect_propagates_flash_read_error_h1() {
        let mut kv = empty_kv();
        kv.storage.flash.fail_from(0);
        let r = kv.garbage_collect();
        assert!(
            matches!(r, Err(AgentError::StorageReadFailed { .. })),
            "expected StorageReadFailed, got {r:?}"
        );
    }

    #[test]
    fn get_stats_propagates_flash_read_error_h1() {
        let mut kv = empty_kv();
        kv.storage.flash.fail_from(0);
        let r = kv.get_stats();
        assert!(
            matches!(r, Err(AgentError::StorageReadFailed { .. })),
            "expected StorageReadFailed, got {r:?}"
        );
    }

    #[test]
    fn set_rejects_oversize_value_via_validation() {
        let mut kv = empty_kv();
        let big = vec![0xA5u8; 300];
        let r = kv.set("k", &big);
        assert!(
            matches!(r, Err(AgentError::InputValidationFailed { .. })),
            "expected InputValidationFailed, got {r:?}"
        );
    }

    #[test]
    fn storage_error_write_error_variant_exists() {
        // Regression: the new `WriteError` variant added for H1 must
        // remain on the public `StorageError` enum so GC overflow can
        // report it distinctly from `WriteProtected`.
        let v = StorageError::WriteError;
        assert_eq!(v, StorageError::WriteError);
    }

    #[test]
    fn set_accepts_max_key_length() {
        let mut kv = empty_kv();
        let key = "k".repeat(32);
        kv.set(&key, b"v").unwrap();
        assert_eq!(&kv.get(&key).unwrap().unwrap()[..], b"v");
    }

    #[test]
    fn set_rejects_key_over_32() {
        let mut kv = empty_kv();
        let key = "k".repeat(33);
        let r = kv.set(&key, b"v");
        assert!(matches!(r, Err(AgentError::InputValidationFailed { .. })));
    }

    #[test]
    fn get_rejects_key_over_32() {
        let mut kv = empty_kv();
        let key = "k".repeat(33);
        let r = kv.get(&key);
        assert!(matches!(r, Err(AgentError::InputValidationFailed { .. })));
    }

    #[test]
    fn set_accepts_max_value_length() {
        let mut kv = empty_kv();
        // Exactly 256 bytes — the inclusive upper bound.
        let val: Vec<u8> = (0..256u16).map(|i| (i % 251) as u8).collect();
        kv.set("max", &val).unwrap();
        assert_eq!(&kv.get("max").unwrap().unwrap()[..], val.as_slice());
    }

    #[test]
    fn set_rejects_value_over_256() {
        let mut kv = empty_kv();
        let big = vec![0u8; 257];
        let r = kv.set("k", &big);
        assert!(matches!(r, Err(AgentError::InputValidationFailed { .. })));
    }

    #[test]
    fn binary_value_round_trips_all_byte_values() {
        // A value covering every possible byte (0..=255) must survive a
        // write→read round-trip intact, including 0xFF bytes that would be
        // ambiguous to a naive scanner but are length-prefixed here.
        let mut kv = empty_kv();
        let val: Vec<u8> = (0..=255u16).map(|i| (i & 0xFF) as u8).collect();
        kv.set("bin", &val).unwrap();
        assert_eq!(&kv.get("bin").unwrap().unwrap()[..], val.as_slice());
    }

    // -----------------------------------------------------------------
    // Corrupt `value_len` in a parsed header (audit-2026-09)
    // -----------------------------------------------------------------
    //
    // `value_len` is a full `u16` read out of flash. A header with a valid
    // key length but a garbage value length used to yield an `entry_size`
    // up to 65572, which the 512-byte scan buffers in `collect_live` and
    // `get_stats` sliced straight through — panicking with an
    // out-of-range slice on corrupt flash instead of surfacing the
    // module's typed "corrupted" outcome. Writers cap values at
    // `MAX_VALUE_LEN`, so an over-long `value_len` is always corruption.

    /// Build a corrupt entry at `addr`: a plausible 1-byte key `"k"` with
    /// `value_len = 0xFFFF`, i.e. an `entry_size` of 65541 — far past both
    /// the 512-byte buffer and any legitimate entry.
    fn write_corrupt_value_len(kv: &mut KvStore<MockFlash>, addr: usize) {
        kv.storage.flash.data[addr] = 1; // key_len = 1
        kv.storage.flash.data[addr + 1] = b'k';
        kv.storage.flash.data[addr + 2] = 0xFF; // value_len = 0xFFFF (LE)
        kv.storage.flash.data[addr + 3] = 0xFF;
    }

    #[test]
    fn get_stats_tolerates_corrupt_value_len_without_panicking() {
        let mut kv = empty_kv();
        write_corrupt_value_len(&mut kv, 0);
        // The corrupt header ends the scan (same policy as a corrupt
        // key_len), so the store reports empty rather than panicking.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.corrupted_entries, 0);
    }

    #[test]
    fn collect_live_tolerates_corrupt_value_len_without_panicking() {
        let mut kv = empty_kv();
        write_corrupt_value_len(&mut kv, 0);
        // `delete` runs `collect_live` over the whole store; the corrupt
        // header must stop the scan, not blow the buffer.
        kv.delete("k").unwrap();
        // No live entry survived, so nothing is rewritten.
        assert!(matches!(kv.get("k"), Ok(None)));
        assert_eq!(kv.get_stats().unwrap().total_entries, 0);
    }

    #[test]
    fn parse_kv_header_rejects_value_len_above_max() {
        let mut buf = [0u8; KV_HEADER_MAX];
        buf[0] = 1;
        buf[1] = b'k';
        buf[2] = 0xFF;
        buf[3] = 0xFF;
        assert!(parse_kv_header(&buf).is_none());

        // A boundary value of exactly MAX_VALUE_LEN is still accepted.
        let max = MAX_VALUE_LEN as u16;
        buf[2] = (max & 0xFF) as u8;
        buf[3] = (max >> 8) as u8;
        let hdr = parse_kv_header(&buf).expect("MAX_VALUE_LEN must parse");
        assert_eq!(hdr.value_len, MAX_VALUE_LEN);
        assert_eq!(hdr.entry_size, 3 + 1 + MAX_VALUE_LEN + 2);
    }
}

/// KV store statistics
#[derive(Debug, Clone)]
pub struct KvStoreStats {
    /// Total number of slot entries currently held (valid + deleted + corrupted).
    pub total_entries: usize,
    /// Number of entries that parse as a live record (key + value + checksum OK).
    pub valid_entries: usize,
    /// Reserved for API compatibility. Compaction physically removes deleted
    /// entries, so this is always 0.
    pub deleted_entries: usize,
    /// Number of slots whose checksum failed and are considered lost.
    pub corrupted_entries: usize,
    /// Bytes of flash occupied by all live slots.
    pub used_space: usize,
    /// Remaining bytes available for new writes (`capacity - used_space`).
    pub free_space: usize,
}
