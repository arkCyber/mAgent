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

/// Largest scan buffer any KV path uses.
///
/// `MAX_VALUE_LEN` caps an entry at 293 bytes, so 512 covers an entry plus
/// its pad; a program unit wider than this cannot be served at all and is
/// refused rather than overflowing a bounded buffer.
const MAX_SCAN_BUF: usize = 512;

/// Maximum value length `set` will accept. Enforced on write and on
/// header parse, so the *raw* entry size can never exceed
/// `3 + 32 + 256 + 2 = 293`, and the padded slot (see [`slot_size`]) stays
/// inside the 512-byte scan buffers every reader uses.
const MAX_VALUE_LEN: usize = 256;

/// Round a raw entry size up to a whole number of program units.
///
/// `embedded_storage`'s `NorFlash::write` contract is that the offset and the
/// length are both multiples of `WRITE_SIZE`; a real NOR part programs in
/// words, and the ESP-IDF encrypted partition view advertises 16. The KV
/// wire format is `3 + key_len + value_len + 2` bytes — almost never a
/// multiple of 4 or 16 — so writing it through `FlashStorage::write`
/// verbatim was rejected on every part whose `WRITE_SIZE` exceeded 1. The
/// mock flash used by the tests reports `WRITE_SIZE = 1`, where the
/// alignment guards are vacuous, which is why this stayed latent.
///
/// Padding (rather than changing the wire format) keeps already-written
/// entries readable: the header is unchanged, readers stop after
/// `entry_size` bytes, and the pad is erased flash (`0xFF`) that the next
/// entry's start offset skips over.
const fn slot_size(entry_size: usize, write_size: usize) -> usize {
    // `WRITE_SIZE` is a compile-time constant of the part; treat a
    // nonsensical zero as "byte-programmable" rather than dividing by it.
    if write_size <= 1 {
        return entry_size;
    }
    let rem = entry_size % write_size;
    if rem == 0 {
        entry_size
    } else {
        entry_size + (write_size - rem)
    }
}

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
/// Callers that *rewrite* the store (compaction) must go through
/// [`read_entry_header`] instead, which distinguishes "erased" from
/// "corrupt" — collapsing the two let a damaged header in the middle of the
/// region read as end-of-store, and compaction then erased everything past
/// that point without rewriting it.
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
#[cfg(test)]
fn parse_kv_header(buf: &[u8]) -> Option<KvHeader> {
    parse_kv_header_within(buf, KV_HEADER_MAX)
}

/// Decode a header from `buf`, treating the entry as valid only if it lies
/// entirely within `avail` bytes.
///
/// `buf` may be shorter than [`KV_HEADER_MAX`] when the region's tail is
/// short; only the bytes the declared header actually needs are read. A
/// header declaring an entry bigger than `avail` is corruption.
fn parse_kv_header_within(buf: &[u8], avail: usize) -> Option<KvHeader> {
    let key_len = *buf.first()? as usize;
    if key_len == 0 || key_len > 32 {
        return None; // erased (0xFF), tombstoned/corrupt (0), or corrupt
    }
    let header_end = 1 + key_len + 2;
    if buf.len() < header_end {
        return None;
    }
    let value_len = u16::from_le_bytes([buf[1 + key_len], buf[2 + key_len]]) as usize;
    if value_len > MAX_VALUE_LEN {
        return None; // corrupt value length (writers cap at MAX_VALUE_LEN)
    }
    let entry_size = 3 + key_len + value_len + 2;
    if entry_size > avail {
        return None; // declares more bytes than the region holds
    }
    Some(KvHeader {
        key_len,
        value_len,
        entry_size,
    })
}

/// What a header slot holds.
///
/// [`parse_kv_header`] folds `Erased` and `Corrupt` into `None`, which is
/// correct for a read-only scan (both end the useful prefix) but wrong for
/// compaction: treating a corrupt header mid-region as end-of-store made
/// `collect_live` stop there, after which `erase_all` destroyed every live
/// entry beyond it.
enum HeaderSlot {
    Erased,
    Corrupt,
    Live(KvHeader),
}

/// Read the header at `offset`, bounded by the region end, and classify it.
///
/// A KV region is not required to be a whole number of entries: the last entry
/// may sit closer than [`KV_HEADER_MAX`] bytes to `capacity`. Reading a fixed
/// 35-byte header there asks the flash for bytes the region does not own, and
/// both the mock and real parts reject that as an I/O error — so a perfectly
/// valid short tail entry turned a plain lookup into `StorageReadFailed`
/// ("hardware fault") instead of being parsed. Only the bytes that are left
/// are read here.
///
/// The three-way answer matters: a read-only scan treats both `Erased` and
/// `Corrupt` as end-of-store, but compaction must not — it erases the region
/// afterwards, so silently stopping at a damaged header would delete every
/// entry past it. `Erased` also covers a tail too short to hold the header the
/// slot begins, because that is the end of the used region rather than damage.
fn read_entry_header<F: NorFlash>(
    storage: &mut FlashStorage<F>,
    base_address: u32,
    offset: u32,
    capacity: u32,
    buf: &mut [u8; 512],
) -> Result<HeaderSlot> {
    let remaining = capacity.saturating_sub(offset) as usize;
    if remaining == 0 {
        return Ok(HeaderSlot::Erased);
    }
    // Read one byte first: `key_len` alone tells us how much of the header
    // this entry actually declares, so a short *tail* entry is read exactly
    // and never needs the full `KV_HEADER_MAX`.
    storage
        .read(base_address + offset, &mut buf[..1])
        .map_err(|e| AgentError::StorageReadFailed {
            address: base_address + offset,
            reason: e.into_storage_error(),
        })?;

    // Erased slot: all 0xFF (checked over whatever remains, since a trailing
    // run of erased bytes is empty space regardless of its length).
    if buf[0] == 0xFF {
        let erased_len = remaining.min(KV_HEADER_MAX);
        storage
            .read(base_address + offset, &mut buf[..erased_len])
            .map_err(|e| AgentError::StorageReadFailed {
                address: base_address + offset,
                reason: e.into_storage_error(),
            })?;
        return Ok(if buf[..erased_len].iter().all(|&b| b == 0xFF) {
            HeaderSlot::Erased
        } else {
            // A torn write: byte 0 erased, later bytes written.
            HeaderSlot::Corrupt
        });
    }

    let key_len = buf[0] as usize;
    if key_len == 0 || key_len > 32 {
        return Ok(HeaderSlot::Corrupt);
    }
    // `value_len` sits at `1 + key_len`, so the header needs
    // `1 + key_len + 2` bytes. If the region ends before that, the entry
    // cannot be here at all — that is the end of the used region, not damage.
    let header_len = 1 + key_len + 2;
    if remaining < header_len {
        return Ok(HeaderSlot::Erased);
    }
    storage
        .read(base_address + offset, &mut buf[..header_len])
        .map_err(|e| AgentError::StorageReadFailed {
            address: base_address + offset,
            reason: e.into_storage_error(),
        })?;
    // One decoder for every caller: a header that decodes here is the same
    // header `get`/`collect_live`/`get_stats` see.
    match parse_kv_header_within(&buf[..header_len], remaining) {
        Some(hdr) => Ok(HeaderSlot::Live(hdr)),
        // The header itself parsed far enough to know `key_len`, so a failure
        // here means the declared lengths are impossible — damage, not empty
        // space. (A tail too short to hold the header returned above.)
        None => Ok(HeaderSlot::Corrupt),
    }
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
    /// `compact` reclaims space with `erase_all`, which erases whole
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
        // `set` appends; it never rewrites an existing key. Lookup therefore
        // returns the *last* matching entry (the newest write), not the first
        // — otherwise `set("k","new")` after `set("k","old")` stored the new
        // value but `get("k")` kept answering "old" forever.
        let mut found: Option<Vec<u8, 256>> = None;

        while offset < capacity {
            // A read error is propagated, not absorbed as "key absent"
            // (audit-2026-08 H1): callers must be able to tell a hardware
            // fault from a legitimate miss. The header read is bounded by the
            // region end so a short tail entry is not mistaken for one (see
            // [`read_header_bounded`]).
            let hdr = match read_entry_header(
                &mut self.storage,
                self.base_address,
                offset,
                capacity,
                &mut buf,
            )? {
                // Empty space is the end of the used region.
                HeaderSlot::Erased => break,
                // Damage is *not* the end of the region: a torn write at this
                // slot left every earlier entry intact and says nothing about
                // the entries after it. Stopping here made all of them
                // unreachable (`get` returned `None` for keys that were
                // plainly on flash) — the same "damage reads as end-of-store"
                // hazard the tri-state was introduced to close, still present
                // on the read path.
                //
                // `parse_kv_header` reported `key_len` before failing, but the
                // failure means the entry's extent is unknown, so the scan
                // cannot simply step over it. Report the damage: a lookup must
                // not silently invent "key absent" for a store whose layout is
                // unreadable.
                HeaderSlot::Corrupt => {
                    return Err(AgentError::StorageReadFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    })
                }
                HeaderSlot::Live(hdr) => hdr,
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
                found = Some(result);
            }

            offset += slot_size(entry_size, self.storage.page_size()) as u32;
        }

        Ok(found)
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
        // The slot is what the flash actually consumes: the entry rounded up
        // to a whole program unit (see [`slot_size`]).
        let slot = slot_size(entry_size, self.storage.page_size());

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

        // A slot wider than the scan buffer cannot be built or read at all.
        // The old `while entry.len() < slot { push(0xFF) }` discarded the
        // push error, so `WRITE_SIZE > 512` spun forever at 100 % CPU instead
        // of failing — the exact failure mode the "bounded data structures"
        // rule exists to prevent. Refuse up front.
        if slot > MAX_SCAN_BUF {
            return Err(AgentError::StorageWriteFailed {
                address: self.base_address,
                reason: StorageError::OutOfSpace,
            });
        }
        // Pad the remaining program-unit bytes with erased-flash (`0xFF`).
        // Readers stop at `entry_size`, so the pad is never interpreted; it
        // exists only so `NorFlash::write` receives an aligned length.
        while entry.len() < slot {
            let _ = entry.push(0xFF);
        }

        // Find free space in flash. Each slot is classified by the *same*
        // decoder every read path uses, so "can I write here" and "can I read
        // this" cannot disagree. A hand-rolled probe here read a fixed-width
        // header and, near the region end, handed a short buffer to a parser
        // that requires the full header — reporting a legal tail slot as
        // `CorruptedData`.
        let mut offset: u32 = 0;
        let mut buf = [0u8; MAX_SCAN_BUF];
        let capacity = self.storage.capacity() as u32;
        let mut found = false;
        while offset + slot as u32 <= capacity {
            match read_entry_header(
                &mut self.storage,
                self.base_address,
                offset,
                capacity,
                &mut buf,
            )? {
                // Empty space: this slot can be programmed directly.
                HeaderSlot::Erased => {
                    found = true;
                    break;
                }
                // An occupied slot the decoder cannot make sense of is real
                // damage; writing past it would hide it.
                HeaderSlot::Corrupt => {
                    return Err(AgentError::StorageReadFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    });
                }
                HeaderSlot::Live(hdr) => {
                    let advance =
                        slot_size(hdr.entry_size, self.storage.page_size()) as u32;
                    // A zero advance would spin forever; an entry always has
                    // at least four bytes, so this only guards corruption.
                    if advance == 0 {
                        return Err(AgentError::StorageReadFailed {
                            address: self.base_address + offset,
                            reason: StorageError::CorruptedData,
                        });
                    }
                    offset += advance;
                }
            }
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
    /// `base_address + offset`. Returns the next free offset (the slot after
    /// this entry, i.e. already rounded up to a program unit).
    fn write_entry_at(&mut self, offset: u32, key: &[u8], value: &[u8]) -> Result<u32> {
        let key_len = key.len() as u8;
        let value_len = value.len() as u16;
        let entry_size = 3 + key_len as usize + value_len as usize + 2;
        let slot = slot_size(entry_size, self.storage.page_size());
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
        // A slot wider than the buffer cannot be built; see the same guard in
        // `set`. Without it the pad loop below never terminates.
        if slot > MAX_SCAN_BUF {
            return Err(AgentError::StorageWriteFailed {
                address: self.base_address + offset,
                reason: StorageError::OutOfSpace,
            });
        }
        // Pad to the program unit so the write is aligned (see [`slot_size`]).
        while entry.len() < slot {
            let _ = entry.push(0xFF);
        }
        self.storage.write(self.base_address + offset, &entry)?;
        Ok(offset + slot as u32)
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
    ///
    /// Each entry's CRC is verified before it is collected. Compaction
    /// rewrites the collected values with a freshly computed CRC, so a
    /// corrupted value that was copied through would come back wearing a
    /// *valid* checksum — the damage would become permanently undetectable.
    /// A bad entry is therefore an error, not something to carry forward.
    ///
    /// A slot that decodes as neither erased nor live is also an error rather
    /// than end-of-store: compaction erases the whole region afterwards, so
    /// silently stopping at a damaged header would delete everything beyond
    /// it. The caller gets `CorruptedData` and the flash is left untouched.
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
            // Bound the header read by the region end (see
            // [`read_entry_header`]); a short tail entry must read cleanly.
            let hdr = match read_entry_header(
                &mut self.storage,
                self.base_address,
                offset,
                capacity,
                &mut buf,
            )? {
                // End of the used region: everything past here is untouched.
                HeaderSlot::Erased => break,
                // Damage is an error here: compaction erases the region
                // afterwards, so stopping quietly would delete everything
                // beyond the damaged header.
                HeaderSlot::Corrupt => {
                    return Err(AgentError::StorageReadFailed {
                        address: self.base_address + offset,
                        reason: StorageError::CorruptedData,
                    })
                }
                HeaderSlot::Live(hdr) => hdr,
            };
            let entry_size = hdr.entry_size;
            self.storage
                .read(self.base_address + offset, &mut buf[..entry_size])
                .map_err(|e| AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: e.into_storage_error(),
                })?;

            // Verify the entry before trusting its bytes enough to rewrite
            // them. The trailer is `[crc_lo, crc_hi]` over everything before
            // it; an entry whose CRC does not match is refused.
            let crc_offset = entry_size - 2;
            let stored_crc = u16::from_le_bytes([buf[crc_offset], buf[crc_offset + 1]]);
            if crc16_ccitt(&buf[..crc_offset]) != stored_crc {
                return Err(AgentError::StorageReadFailed {
                    address: self.base_address + offset,
                    reason: StorageError::CorruptedData,
                });
            }

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

            offset += slot_size(entry_size, self.storage.page_size()) as u32;
        }
        Ok(live)
    }

    /// Erase the whole store and rewrite the surviving live entries.
    /// Returns the number of entries rewritten. `skip_key` entries are
    /// physically removed (this is how delete works — no tombstones left
    /// behind, so the store never accumulates dead space).
    fn compact(&mut self, skip_key: Option<&str>) -> Result<usize> {
        let live = self.collect_live(skip_key)?;

        // Prove the survivors fit *before* erasing anything.
        //
        // `erase_all` destroyed the only copy, and `write_entry_at` had no
        // capacity check, so a survivor whose padded slot did not fit made
        // compaction erase the region and then fail: every entry lost, with
        // only an I/O error to show for it. This is reachable with an image
        // written under a narrower program unit (a 9-byte entry read through a
        // `WRITE_SIZE = 16` view pads to 16), and cheap to rule out because
        // `live` is already in RAM.
        let page = self.storage.page_size();
        // `write_entry_at` refuses a slot wider than the scan buffer, so the
        // preflight must apply the *same* predicate: summing slot sizes and
        // comparing against capacity alone let a `WRITE_SIZE > 512` part pass
        // here, erase the region, and then fail on every write — the data-loss
        // path this preflight exists to close.
        let oversized = live
            .iter()
            .any(|(k, v)| slot_size(3 + k.len() + v.len() + 2, page) > MAX_SCAN_BUF);
        let needed: u64 = live
            .iter()
            .map(|(k, v)| slot_size(3 + k.len() + v.len() + 2, page) as u64)
            .sum();
        if oversized || needed > u64::from(self.storage.capacity() as u32) {
            log::error!(
                "[storage] compaction refused: {} surviving entries need {needed} bytes \
                 (program unit {page}); the region has {} and the scan buffer is {MAX_SCAN_BUF}",
                live.len(),
                self.storage.capacity()
            );
            return Err(AgentError::StorageWriteFailed {
                address: self.base_address,
                reason: StorageError::OutOfSpace,
            });
        }

        self.erase_all()?;
        let mut offset = 0u32;
        let mut written = 0usize;
        let mut failed = None;
        for (k, v) in &live {
            match self.write_entry_at(offset, k, v) {
                Ok(next) => {
                    offset = next;
                    written += 1;
                }
                Err(e) => {
                    // The store is now partially rewritten. `live` is still in
                    // RAM, so a transient flash fault can be recovered by
                    // erasing and redoing the whole pass once (option (a) in
                    // the BACKLOG entry). A second failure is reported *after*
                    // logging that the store may be incomplete — the returned
                    // `StorageWriteFailed` alone would not say that.
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(first) = failed {
            log::warn!("[storage] compaction write failed ({first:?}); erasing and redoing the pass once");
            self.erase_all()?;
            offset = 0;
            written = 0;
            for (k, v) in &live {
                match self.write_entry_at(offset, k, v) {
                    Ok(next) => {
                        offset = next;
                        written += 1;
                    }
                    Err(e) => {
                        log::error!(
                            "[storage] compaction failed after retry: {e:?}; \
                             the store may be incomplete ({}/{} entries rewritten)",
                            written,
                            live.len()
                        );
                        return Err(e);
                    }
                }
            }
        }
        // `written == live.len()` on both success paths; returning the
        // counter also keeps the retry accounting honest.
        Ok(written)
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
        // Cleared when the scan stops at a damaged slot, because everything
        // past it — including a second damaged slot — cannot be counted.
        let mut cleaned = true;
        // As in `get`: scan only the region the store owns, not a hardcoded
        // 64 KiB — otherwise a store whose capacity is smaller faults once it
        // fills, and the reported figures are taken over bytes it does not own.
        let capacity = self.storage.capacity() as u32;

        while offset < capacity {
            // Read failures propagate (audit-2026-08 H1), and the header read
            // is bounded by the region end so a short tail entry is not
            // mistaken for a fault (see [`read_entry_header`]).
            let hdr = match read_entry_header(
                &mut self.storage,
                self.base_address,
                offset,
                capacity,
                &mut buf,
            )? {
                HeaderSlot::Erased => break,
                // A damaged slot is *counted*, not treated as end-of-store.
                //
                // The extent of a damaged entry is unknown, so the scan cannot
                // step over it — it has to stop. It stops after counting, and
                // the `clean` flag below says the figures are partial: a
                // second damaged slot past this one is unknowable, and
                // reporting the count as if it were total would be its own
                // small lie.
                HeaderSlot::Corrupt => {
                    corrupted_entries += 1;
                    cleaned = false;
                    break;
                }
                HeaderSlot::Live(hdr) => hdr,
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
                // Count the whole slot: the pad bytes are consumed flash and
                // `free_space` (below) must not offer them again.
                used_space += slot_size(entry_size, self.storage.page_size());
            } else {
                corrupted_entries += 1;
            }

            offset += slot_size(entry_size, self.storage.page_size()) as u32;
        }

        Ok(KvStoreStats {
            total_entries,
            valid_entries,
            // Compaction physically removes deleted entries, so there are
            // never tombstones left behind to count.
            deleted_entries: 0,
            corrupted_entries,
            scan_complete: cleaned,
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
        /// Address whose *next* write fails, then clears (transient fault).
        fail_once_at: Option<u32>,
    }

    impl MockFlash {
        fn new(capacity: usize) -> Self {
            Self {
                data: vec![0xFFu8; capacity],
                fail_from: None,
                fail_once_at: None,
            }
        }

        fn fail_from(&mut self, addr: u32) {
            self.fail_from = Some(addr);
        }

        /// Fail exactly one write at `addr`; later writes/erases succeed.
        fn fail_next_write_at(&mut self, addr: u32) {
            self.fail_once_at = Some(addr);
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
            if self.fail_once_at == Some(address) {
                self.fail_once_at = None;
                return Err(MockErr);
            }
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

    /// A flash view whose `WRITE_SIZE` is 4. `MockFlash` reports a
    /// `WRITE_SIZE` of 1, so `is_multiple_of(page_size)` is always true and
    /// the page-alignment guards in `FlashStorage::{read, write}` are
    /// unreachable through it. This wrapper delegates to the same backing
    /// store but advertises a 4-byte program unit, exactly like the real
    /// NOR parts, so those guards can be exercised.
    struct AlignedFlash(MockFlash);

    impl embedded_storage::nor_flash::ErrorType for AlignedFlash {
        type Error = MockErr;
    }

    impl embedded_storage::nor_flash::ReadNorFlash for AlignedFlash {
        const READ_SIZE: usize = 1;

        fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::read(&mut self.0, address, buf)
        }

        fn capacity(&self) -> usize {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::capacity(&self.0)
        }
    }

    impl embedded_storage::nor_flash::NorFlash for AlignedFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 4096;

        fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::NorFlash>::write(&mut self.0, address, data)
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::NorFlash>::erase(&mut self.0, from, to)
        }
    }

    fn aligned_storage() -> FlashStorage<AlignedFlash> {
        FlashStorage::new(AlignedFlash(MockFlash::new(4096)))
    }

    #[test]
    fn flash_storage_rejects_a_misaligned_read() {
        let mut storage = aligned_storage();
        let mut buf = [0u8; 4];
        // offset 3 is not a multiple of the 4-byte write/page size.
        match storage.read(3, &mut buf) {
            Err(AgentError::StorageReadFailed {
                address: 3,
                reason: StorageError::BadAddress,
            }) => {}
            other => panic!("expected StorageReadFailed@3/BadAddress, got {other:?}"),
        }
    }

    #[test]
    fn flash_storage_rejects_a_misaligned_write() {
        let mut storage = aligned_storage();
        match storage.write(2, &[0u8; 4]) {
            Err(AgentError::StorageWriteFailed {
                address: 2,
                reason: StorageError::BadAddress,
            }) => {}
            other => panic!("expected StorageWriteFailed@2/BadAddress, got {other:?}"),
        }
    }

    #[test]
    fn flash_storage_rejects_a_write_of_the_wrong_size() {
        let mut storage = aligned_storage();
        // Aligned offset, but 3 bytes is not a whole number of pages.
        match storage.write(0, &[0u8; 3]) {
            Err(AgentError::StorageWriteFailed {
                address: 0,
                reason: StorageError::BadAddress,
            }) => {}
            other => panic!("expected StorageWriteFailed@0/BadAddress, got {other:?}"),
        }
    }

    #[test]
    fn flash_storage_surfaces_a_program_fault() {
        let mut storage = aligned_storage();
        storage.flash.0.fail_from(0);
        match storage.write(0, &[0u8; 4]) {
            Err(AgentError::StorageWriteFailed {
                address: 0,
                reason: StorageError::WriteProtected,
            }) => {}
            other => panic!("expected StorageWriteFailed/WriteProtected, got {other:?}"),
        }
    }

    #[test]
    fn flash_storage_surfaces_an_erase_fault() {
        let mut storage = aligned_storage();
        storage.flash.0.fail_from(0);
        match storage.erase(0) {
            Err(AgentError::StorageWriteFailed {
                reason: StorageError::EraseError,
                ..
            }) => {}
            other => panic!("expected StorageWriteFailed/EraseError, got {other:?}"),
        }
    }

    #[test]
    fn flash_storage_reports_its_geometry() {
        let storage = aligned_storage();
        assert_eq!(storage.page_size(), 4);
        assert_eq!(storage.sector_size(), 4096);
        assert_eq!(storage.capacity(), 4096);
    }

    #[test]
    fn parse_kv_header_rejects_a_short_buffer() {
        // Anything shorter than the maximal header cannot hold value_len.
        assert!(parse_kv_header(&[]).is_none());
        assert!(parse_kv_header(&[1, b'k']).is_none());
        assert!(parse_kv_header(&[0u8; KV_HEADER_MAX - 1]).is_none());
    }

    /// `slot_size` must round up to a program unit and be a no-op below it.
    #[test]
    fn slot_size_rounds_up_to_a_program_unit() {
        assert_eq!(slot_size(9, 1), 9, "byte-programmable parts are unchanged");
        assert_eq!(slot_size(9, 0), 9, "a zero WRITE_SIZE must not divide");
        assert_eq!(slot_size(9, 4), 12);
        assert_eq!(slot_size(12, 4), 12, "an aligned entry is not grown");
        assert_eq!(slot_size(9, 16), 16);
        // The largest legal entry still fits the 512-byte scan buffers.
        assert!(slot_size(3 + 32 + MAX_VALUE_LEN + 2, 16) <= 512);
    }

    /// A store whose region ends with fewer than 35 bytes left must still be
    /// scannable.
    ///
    /// Every scan walks `while offset < capacity` and then reads a fixed
    /// `KV_HEADER_MAX` (35) byte header. With fewer than 35 bytes remaining,
    /// that read asks the flash for bytes past the region end; a real part
    /// rejects it, and `get` reported `StorageReadFailed` — a hardware fault —
    /// for a perfectly healthy store that simply had a short last entry (or a
    /// short erased tail). Here a 40-byte region holds one 7-byte entry, so
    /// only 33 bytes remain when the scan reaches the tail.
    #[test]
    fn a_region_ending_mid_header_is_a_clean_end_of_store() {
        let mut kv = KvStore::new(FlashStorage::new(MockFlash::new(40)), 0);
        kv.set("z", b"9").expect("a 7-byte entry fits a 40-byte region");

        // 7 + 7 = 14 used, 26 erased. The scan must stop cleanly.
        assert_eq!(kv.get("z").unwrap(), Some({
            let mut v = heapless::Vec::<u8, 256>::new();
            let _ = v.push(b'9');
            v
        }));
        assert_eq!(kv.get("absent").expect("tail must not fault"), None);

        // The same tail must not make the stats scan or a compaction fault.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.valid_entries, 1);
        assert_eq!(stats.used_space, slot_size(7, 1));
        kv.delete("absent").expect("compaction over a short tail");
        assert_eq!(kv.get("z").unwrap().map(|v| v.len()), Some(1));
    }

    /// A torn slot must be reported, not read as "end of store".
    ///
    /// A power loss can leave byte 0 erased while later bytes were
    /// programmed. `read_entry_header` calls that `Corrupt`; the scan used to
    /// `break` on it, so every entry written *after* the torn slot became
    /// invisible (`get` answered `None` for keys plainly present) and `set`
    /// refused forever — the store was bricked with no way to tell.
    #[test]
    fn a_torn_slot_is_reported_not_read_as_end_of_store() {
        // Lay the image out by hand: a good entry, a torn slot, then a good
        // entry. Writing through `set` cannot build this (it refuses to
        // write past damage), which is itself the correct behaviour.
        let mut flash = MockFlash::new(256);
        let first = raw_entry(b"first", b"1");
        let third = raw_entry(b"third", b"3");
        let torn_at = first.len();
        flash.data[..first.len()].copy_from_slice(&first);
        // Torn: byte 0 erased, later bytes programmed.
        flash.data[torn_at] = 0xFF;
        flash.data[torn_at + 1] = 0x42;
        flash.data[torn_at + 2] = 0x43;
        let third_at = torn_at + 8;
        flash.data[third_at..third_at + third.len()].copy_from_slice(&third);
        let mut kv = KvStore::new(FlashStorage::new(flash), 0);

        match kv.get("third") {
            Err(AgentError::StorageReadFailed {
                reason: StorageError::CorruptedData,
                ..
            }) => {}
            other => panic!(
                "a torn slot must be reported as corruption, not answered as \
                 a miss: {other:?}"
            ),
        }

        // The stats path counts the damage instead of calling the store
        // clean.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.corrupted_entries, 1, "stats must count the torn slot");
    }

    /// Compaction's preflight must refuse an oversized program unit too.
    ///
    /// `write_entry_at` refuses a slot wider than `MAX_SCAN_BUF`, but the
    /// preflight only compared the total against capacity. A
    /// `WRITE_SIZE = 1024` part therefore passed the check, erased the region,
    /// and then failed every write — the data loss the preflight exists to
    /// prevent.
    #[test]
    fn compaction_refuses_an_oversized_program_unit_before_erasing() {
        let mut flash = MockFlash::new(4096);
        let good = raw_entry(b"k", b"v");
        flash.data[..good.len()].copy_from_slice(&good);
        let mut kv = KvStore::new(FlashStorage::new(HugeFlash(flash)), 0);

        let err = kv.garbage_collect().expect_err("must refuse, not erase");
        assert!(
            matches!(
                err,
                AgentError::StorageWriteFailed {
                    reason: StorageError::OutOfSpace,
                    ..
                }
            ),
            "expected OutOfSpace, got {err:?}"
        );
        assert_eq!(
            &kv.storage.flash.0.data[..good.len()],
            &good[..],
            "the region must be untouched"
        );
    }

    /// `get_stats` must say when its figures are partial.
    ///
    /// A damaged slot's extent is unknown, so the scan stops there; reporting
    /// the counts as totals would under-report a second damaged slot past it.
    #[test]
    fn stats_flag_a_partial_scan() {
        let mut kv = empty_kv();
        kv.set("a", b"1").unwrap();
        let second = slot_size(3 + 1 + 1 + 2, 1);
        // Torn slot at the second position.
        kv.storage.flash.data[second] = 0xFF;
        kv.storage.flash.data[second + 1] = 0x42;

        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.corrupted_entries, 1);
        assert!(
            !stats.scan_complete,
            "counts taken past a damaged slot are lower bounds, not totals"
        );
    }

    /// Compaction must not erase the region unless the survivors fit.
    ///
    /// `erase_all` destroyed the only copy and `write_entry_at` had no
    /// capacity check, so a survivor whose padded slot did not fit made
    /// compaction erase everything and then fail — total data loss for an
    /// I/O error. Reachable with an image written under a narrower program
    /// unit.
    #[test]
    fn compaction_refuses_before_erasing_when_survivors_do_not_fit() {
        // 20-byte region seen through a 16-byte program unit: an 18-byte
        // legacy entry lives at 0 and pads to 32, which cannot fit.
        let mut flash = MockFlash::new(20);
        let legacy = raw_entry(b"q", &[0x51u8; 13]); // 1+1+2+13+2 = 19
        assert_eq!(legacy.len(), 19);
        flash.data[..19].copy_from_slice(&legacy);
        let mut kv = KvStore::new(FlashStorage::new(WideFlash::new_from(flash, 16)), 0);

        let err = kv.garbage_collect().expect_err("must refuse, not erase");
        assert!(
            matches!(
                err,
                AgentError::StorageWriteFailed {
                    reason: StorageError::OutOfSpace,
                    ..
                }
            ),
            "expected OutOfSpace, got {err:?}"
        );
        // The bytes are still there: nothing was erased.
        assert_eq!(&kv.storage.flash.0.data[..19], &legacy[..]);
    }

    /// The last slot of a padded store must be usable, and a full store must
    /// say "out of space" rather than "hardware fault".
    ///
    /// `set`'s free-slot probe read a fixed 35-byte header while only
    /// guaranteeing `slot` bytes were available. With a 16-byte program unit
    /// the final slot (16..34 bytes remaining) therefore crossed `capacity`
    /// and every write past it failed as `StorageReadFailed{ReadError}` — with
    /// `get_stats` still advertising the space as free. Measured before the
    /// fix: 8 of 10 entries fit in a 160-byte region, `free_space: 32`.
    #[test]
    fn the_last_slot_of_a_padded_store_is_usable() {
        let mut kv = KvStore::new(FlashStorage::new(WideFlash::new(160, 16)), 0);
        let value = [0x41u8; 4];
        let mut written = 0;
        for i in 0..10u8 {
            let key = alloc::format!("k{i}");
            match kv.set(&key, &value) {
                Ok(()) => written += 1,
                // Running out of room is correct; a read fault is not.
                Err(AgentError::StorageWriteFailed {
                    reason: StorageError::OutOfSpace,
                    ..
                }) => break,
                Err(e) => panic!("set({key}) failed as {e:?}, not OutOfSpace"),
            }
        }
        assert_eq!(written, 10, "a 160-byte region holds ten 16-byte slots");
        assert_eq!(kv.get_stats().unwrap().free_space, 0);
        for i in 0..10u8 {
            let key = alloc::format!("k{i}");
            assert!(kv.get(&key).unwrap().is_some(), "{key} must read back");
        }
    }

    /// A live entry in the final bytes of the region must not be deleted by
    /// compaction.
    ///
    /// `read_entry_header` classified "fewer than 35 bytes left" as
    /// `Erased`, so a valid short entry at the tail was invisible to `get`
    /// and then permanently erased by `garbage_collect`. The whole point of
    /// the tri-state was to stop compaction treating damage as end-of-store;
    /// this branch reopened it for the tail.
    #[test]
    fn compaction_keeps_a_live_tail_entry() {
        // 43-byte region (one erase unit), a 34-byte entry then a 7-byte one.
        let mut flash = MockFlash::new(43);
        let big = raw_entry(b"x", &[0x41u8; 28]);
        let small = raw_entry(b"y", b"v");
        assert_eq!(big.len(), 34);
        assert_eq!(small.len(), 7);
        flash.data[..34].copy_from_slice(&big);
        flash.data[34..41].copy_from_slice(&small);

        let mut kv = KvStore::new(FlashStorage::new(flash), 0);
        assert!(
            kv.get("y").unwrap().is_some(),
            "a tail entry must be readable before compaction"
        );
        kv.garbage_collect().unwrap();
        assert!(
            kv.get("y").unwrap().is_some(),
            "compaction erased a live tail entry"
        );
        assert!(kv.get("x").unwrap().is_some());
    }

    /// A header near the region end that declares more bytes than remain is
    /// damage, and must not brick the store.
    ///
    /// It used to decode as `Live`, after which every read faulted and
    /// compaction refused forever with no way to recover.
    #[test]
    fn an_entry_that_overruns_the_region_is_corruption_not_a_live_entry() {
        let mut flash = MockFlash::new(64);
        // key_len = 1, key "k", value_len = 256 (legal but far too large).
        flash.data[16] = 1;
        flash.data[17] = b'k';
        flash.data[18] = 0;
        flash.data[19] = 1;
        let mut kv = KvStore::new(FlashStorage::new(flash), 0);
        match kv.garbage_collect() {
            Err(AgentError::StorageReadFailed {
                reason: StorageError::CorruptedData,
                ..
            }) => {}
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    /// A program unit wider than the scan buffer must be refused, not spin.
    ///
    /// The pad loop `while entry.len() < slot { push(0xFF) }` discarded the
    /// push error, so with `WRITE_SIZE = 1024` the bounded `Vec<u8, 512>`
    /// never grew and `set` hung at 100 % CPU forever.
    #[test]
    fn a_program_unit_wider_than_the_buffer_is_refused_not_hung() {
        let mut kv = KvStore::new(FlashStorage::new(HugeFlash(MockFlash::new(4096))), 0);
        match kv.set("k", b"v") {
            Err(AgentError::StorageWriteFailed { .. }) => {}
            Ok(()) => panic!("a 1024-byte program unit cannot be served"),
            Err(e) => panic!("expected StorageWriteFailed, got {e:?}"),
        }
    }

    /// Serialise a KV entry exactly as the writer does (header, body, CRC).
    fn raw_entry(key: &[u8], value: &[u8]) -> std::vec::Vec<u8> {
        let mut e = std::vec::Vec::new();
        e.push(key.len() as u8);
        e.extend_from_slice(key);
        e.push((value.len() & 0xFF) as u8);
        e.push((value.len() >> 8) as u8);
        e.extend_from_slice(value);
        let crc = crc16_ccitt(&e);
        e.push((crc & 0xFF) as u8);
        e.push((crc >> 8) as u8);
        e
    }

    /// A mock flash with a configurable program unit.
    struct WideFlash(MockFlash, usize);

    impl WideFlash {
        fn new(capacity: usize, write_size: usize) -> Self {
            Self(MockFlash::new(capacity), write_size)
        }

        /// Wrap an existing backing store (so a test can pre-seed the
        /// flash bytes) in a view that reports `write_size`.
        fn new_from(flash: MockFlash, write_size: usize) -> Self {
            Self(flash, write_size)
        }
    }

    impl embedded_storage::nor_flash::ErrorType for WideFlash {
        type Error = MockErr;
    }

    impl embedded_storage::nor_flash::ReadNorFlash for WideFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::read(&mut self.0, address, buf)
        }
        fn capacity(&self) -> usize {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::capacity(&self.0)
        }
    }

    impl embedded_storage::nor_flash::NorFlash for WideFlash {
        // Rust does not allow a per-instance associated const, so the
        // program unit is fixed at 16 here (`self.1` re-checks it at runtime);
        // the wider case uses `HugeFlash` below.
        const WRITE_SIZE: usize = 16;
        const ERASE_SIZE: usize = 4096;

        fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
            // The delegate has WRITE_SIZE 1, so enforce the modelled unit here.
            if !(address as usize).is_multiple_of(self.1) || !data.len().is_multiple_of(self.1) {
                return Err(MockErr);
            }
            <MockFlash as embedded_storage::nor_flash::NorFlash>::write(&mut self.0, address, data)
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::NorFlash>::erase(&mut self.0, from, to)
        }
    }

    /// A flash whose program unit is 1024 bytes (wider than `MAX_SCAN_BUF`).
    struct HugeFlash(MockFlash);

    impl embedded_storage::nor_flash::ErrorType for HugeFlash {
        type Error = MockErr;
    }

    impl embedded_storage::nor_flash::ReadNorFlash for HugeFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::read(&mut self.0, address, buf)
        }
        fn capacity(&self) -> usize {
            <MockFlash as embedded_storage::nor_flash::ReadNorFlash>::capacity(&self.0)
        }
    }

    impl embedded_storage::nor_flash::NorFlash for HugeFlash {
        const WRITE_SIZE: usize = 1024;
        const ERASE_SIZE: usize = 4096;

        fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::NorFlash>::write(&mut self.0, address, data)
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            <MockFlash as embedded_storage::nor_flash::NorFlash>::erase(&mut self.0, from, to)
        }
    }

    /// A store on a part whose program unit is 4 bytes must actually work.
    ///
    /// `AlignedFlash` advertises `WRITE_SIZE = 4`; the raw KV entry is
    /// `3 + key + value + 2`, which for `("k", "v")` is 6 — not a multiple of
    /// 4. Before `slot_size` padded entries, `FlashStorage::write` rejected
    /// that length with `BadAddress`, so **every** `set` failed on such a
    /// part. `MockFlash`'s `WRITE_SIZE = 1` made all existing KV tests blind
    /// to it.
    #[test]
    fn kv_store_works_on_a_four_byte_program_unit() {
        let mut kv = KvStore::new(aligned_storage(), 0);
        kv.set("k", b"v").expect("a 6-byte entry must be padded to 8");

        assert_eq!(kv.get("k").expect("get must not fault"), Some({
            let mut v = heapless::Vec::<u8, 256>::new();
            let _ = v.push(b'v');
            v
        }));

        // A second entry must land after the padded slot, not overlap it.
        kv.set("second", b"value").expect("second entry");
        assert_eq!(kv.get("k").unwrap().map(|v| v.len()), Some(1));
        assert_eq!(kv.get("second").unwrap().map(|v| v.len()), Some(5));

        // Delete (which is compaction: erase + rewrite) must round-trip too,
        // since that path writes through `write_entry_at`.
        kv.delete("k").unwrap();
        assert_eq!(kv.get("k").unwrap(), None);
        assert_eq!(kv.get("second").unwrap().map(|v| v.len()), Some(5));

        // The stats scan walks the same padded slots.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.valid_entries, 1);
        assert_eq!(stats.corrupted_entries, 0);
        assert_eq!(stats.used_space, slot_size(3 + 6 + 5 + 2, 4));
    }

    #[test]
    fn get_reports_corruption_when_the_crc_does_not_match() {
        let mut kv = empty_kv();
        kv.set("k", b"v").unwrap();
        // entry = [key_len=1]['k'][val_len=1][0]['v'][crc_lo][crc_hi]; the
        // value byte sits at 3 + key_len = 4. Flip a bit there so the stored
        // CRC no longer matches the recomputed one.
        kv.storage.flash.data[4] ^= 0x01;
        match kv.get("k") {
            Err(AgentError::StorageReadFailed {
                reason: StorageError::CorruptedData,
                ..
            }) => {}
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    /// A transient write fault during compaction must not lose the entries
    /// that were not yet rewritten: the retry pass erases and rewrites the
    /// whole live set, so both keys survive.
    #[test]
    fn compact_recovers_from_a_transient_write_fault() {
        let mut kv = empty_kv();
        kv.set("a", b"1").unwrap();
        kv.set("b", b"2").unwrap();

        // Fail exactly the first write after the first erase; the retry's
        // erase + writes succeed.
        kv.storage.flash.fail_next_write_at(0);

        let survivors = kv.garbage_collect().unwrap();
        assert_eq!(survivors, 2);
        assert_eq!(kv.get("a").unwrap().unwrap().as_slice(), b"1");
        assert_eq!(kv.get("b").unwrap().unwrap().as_slice(), b"2");
    }

    #[test]
    fn get_stats_counts_a_corrupted_entry() {
        let mut kv = empty_kv();
        kv.set("k", b"v").unwrap();
        kv.storage.flash.data[4] ^= 0x01; // corrupt the value → CRC mismatch
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.valid_entries, 0);
        assert_eq!(stats.corrupted_entries, 1);
        assert_eq!(stats.used_space, 0);
    }

    #[test]
    fn set_reports_out_of_space_when_the_store_is_full() {
        let mut kv = empty_kv();
        // 3 + 1 + 250 + 2 = 256 bytes each ⇒ exactly 16 fill the 4096 region.
        let value = [0x41u8; 250];
        for i in 0..16u8 {
            let key = alloc::format!("{}", (b'a' + i) as char);
            kv.set(&key, &value).unwrap();
        }
        let r = kv.set("z", b"x");
        assert!(
            matches!(
                r,
                Err(AgentError::StorageWriteFailed {
                    reason: StorageError::OutOfSpace,
                    ..
                })
            ),
            "expected OutOfSpace, got {r:?}"
        );
    }

    #[test]
    fn set_stops_on_a_corrupt_header_during_the_slot_scan() {
        let mut kv = empty_kv();
        write_corrupt_value_len(&mut kv, 0);
        let r = kv.set("new", b"v");
        assert!(
            matches!(
                r,
                Err(AgentError::StorageReadFailed {
                    reason: StorageError::CorruptedData,
                    ..
                })
            ),
            "expected CorruptedData, got {r:?}"
        );
    }

    #[test]
    fn delete_accepts_an_empty_key_as_a_no_op() {
        let mut kv = empty_kv();
        kv.set("keep", b"v").unwrap();
        kv.delete("").unwrap();
        // The unrelated entry is untouched.
        assert_eq!(&kv.get("keep").unwrap().unwrap()[..], b"v");
    }

    #[test]
    fn delete_rejects_a_key_over_32() {
        let mut kv = empty_kv();
        let key = "k".repeat(33);
        assert!(matches!(
            kv.delete(&key),
            Err(AgentError::InputValidationFailed { .. })
        ));
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

    /// `set` appends rather than rewriting; lookup must therefore prefer the
    /// newest entry. The old first-match scan made updates permanently
    /// invisible (`get` answered the original value forever).
    #[test]
    fn setting_an_existing_key_makes_the_new_value_visible() {
        let mut kv = empty_kv();
        kv.set("k", b"old").unwrap();
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
    fn get_stats_counts_a_corrupt_value_len_without_panicking() {
        let mut kv = empty_kv();
        write_corrupt_value_len(&mut kv, 0);
        // A corrupt header is *reported*, not silently read as an empty
        // store: `corrupted_entries` is the field that exists to say so.
        let stats = kv.get_stats().unwrap();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(
            stats.corrupted_entries, 1,
            "a damaged slot must be counted, not reported as a clean empty store"
        );
    }

    #[test]
    fn collect_live_refuses_a_corrupt_value_len_instead_of_erasing_past_it() {
        let mut kv = empty_kv();
        write_corrupt_value_len(&mut kv, 0);
        // `delete` runs `collect_live` over the whole store. A corrupt header
        // in the middle of the used region used to read as end-of-store, so
        // compaction erased the whole sector and rewrote only the entries
        // before the damage — silently deleting every live entry after it.
        // It must refuse instead, leaving the flash untouched.
        let err = kv.delete("k").unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::StorageReadFailed {
                    reason: StorageError::CorruptedData,
                    ..
                }
            ),
            "expected CorruptedData, got {err:?}"
        );
        assert!(
            !kv.storage.flash.data.iter().all(|&b| b == 0xFF),
            "a refused compaction must not have erased the store"
        );
    }

    /// Compaction must verify each entry's CRC before rewriting it. Recomputing
    /// the CRC over a corrupted value would give the damage a *valid* checksum,
    /// making it permanently undetectable.
    #[test]
    fn compaction_refuses_to_launder_a_corrupted_value() {
        let mut kv = empty_kv();
        kv.set("good", b"1").unwrap();
        kv.set("bad", b"value").unwrap();
        // Flip a bit in the `bad` entry's value byte. Entry layout is
        // `[key_len][key][val_len_lo][val_len_hi][value…][crc_lo][crc_hi]`;
        // "good" occupies 3+4+1+2 = 10 bytes, so "bad"'s value starts at
        // 10 + 3 + 3 = 16.
        let bad_value_offset = 10 + 3 + 3;
        kv.storage.flash.data[bad_value_offset] ^= 0x01;

        let err = kv.garbage_collect().unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::StorageReadFailed {
                    reason: StorageError::CorruptedData,
                    ..
                }
            ),
            "a corrupt entry must block compaction, got {err:?}"
        );
        // Nothing was rewritten: the corruption is still detected on read.
        assert!(
            kv.get("bad").is_err(),
            "the corrupted entry must not have been re-signed"
        );
        assert_eq!(kv.get("good").unwrap().unwrap(), b"1");
    }

    #[test]
    fn parse_kv_header_rejects_value_len_above_max() {
        let mut buf = [0u8; KV_HEADER_MAX];
        buf[0] = 1;
        buf[1] = b'k';
        buf[2] = 0xFF;
        buf[3] = 0xFF;
        assert!(parse_kv_header(&buf).is_none());

        // A boundary value of exactly MAX_VALUE_LEN is still accepted, given
        // a region big enough to hold it (`parse_kv_header` supplies
        // `KV_HEADER_MAX`, so use the `within` form with the real extent).
        let max = MAX_VALUE_LEN as u16;
        buf[2] = (max & 0xFF) as u8;
        buf[3] = (max >> 8) as u8;
        let entry_size = 3 + 1 + MAX_VALUE_LEN + 2;
        let hdr = parse_kv_header_within(&buf, entry_size).expect("MAX_VALUE_LEN must parse");
        assert_eq!(hdr.value_len, MAX_VALUE_LEN);
        assert_eq!(hdr.entry_size, entry_size);

        // …but not when the region cannot hold that entry.
        assert!(parse_kv_header_within(&buf, entry_size - 1).is_none());
    }

    /// A header may be parsed from a buffer shorter than `KV_HEADER_MAX` when
    /// only the declared fields are read.
    #[test]
    fn a_short_header_buffer_parses_when_the_entry_fits() {
        // key_len = 1, key "y", value_len = 1, value, crc: 7 bytes total.
        let buf = [1u8, b'y', 1, 0, b'v', 0, 0];
        let hdr = parse_kv_header_within(&buf, 7).expect("a 7-byte header");
        assert_eq!(hdr.entry_size, 7);
        // The same bytes cannot describe an 8-byte region entry.
        assert!(parse_kv_header_within(&buf, 6).is_none());
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
    ///
    /// A damaged slot's extent is unknown, so the scan stops there and the
    /// figures below describe only the prefix it could read. Check
    /// [`Self::scan_complete`] before treating them as totals.
    pub corrupted_entries: usize,
    /// Whether the scan reached the end of the used region.
    ///
    /// `false` means it stopped at a damaged slot: `total_entries`,
    /// `used_space` and `corrupted_entries` are lower bounds, not totals.
    pub scan_complete: bool,
    /// Bytes of flash occupied by all live slots.
    pub used_space: usize,
    /// Remaining bytes available for new writes (`capacity - used_space`).
    pub free_space: usize,
}
