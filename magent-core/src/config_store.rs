//! Configuration store with versioning, validation, and NVS-backed persistence.
//!
//! The [`ConfigStore`] manages a set of named, versioned configuration entries.
//! Each entry is serialised/deserialised via `postcard` (the project's existing
//! no_std serialisation layer) and validated before commit. The store itself is
//! chip-agnostic — the caller supplies a [`StorageBackend`] that wraps the real
//! NVS (ESP32 `nvs_flash` / nRF `FlashStorage`) or a host-side mock.
//!
//! ## Aerospace-grade guarantees
//!
//! * **Bounded buffers** — all internal storage uses heapless `Vec<u8, N>` and
//!   `String<N>` with compile-time capacity limits.
//! * **No half-written values** — a rejected write leaves the store unchanged
//!   (the in-memory backend replaces a slot in one assignment; an NVS backend
//!   must provide the same guarantee).
//! * **Versioned schemas** — every entry carries a `version: u16`; reading an
//!   entry from a *newer* schema fails instead of silently misreading it.
//! * **Validation at every boundary** — no value is written without passing
//!   through the entry- or store-level validator.
//! * **Honest accounting** — [`ConfigStore::update_count`] counts mutations in
//!   this process only. It is not persisted and is not a wear bound.

use crate::config::AgentConfig;
use crate::error::{AgentError, ConfigError, Result};
use heapless::Vec;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of named entries the store can hold.
pub const MAX_STORE_ENTRIES: usize = 16;

/// Maximum key length (bytes).
pub const MAX_KEY_LENGTH: usize = 32;

/// Maximum serialised entry payload (bytes). This is deliberately generous —
/// `postcard::to_vec<AgentConfig>` produces ~100 bytes for the current struct.
pub const MAX_ENTRY_SIZE: usize = 512;

/// Maximum serialised `StoreEntry` **envelope**, which is what a
/// [`StorageBackend`] stores and returns.
///
/// The envelope wraps the payload with postcard's `u16` version and the
/// `heapless::Vec` length prefix. Keeping it distinct from
/// [`MAX_ENTRY_SIZE`] means a payload that is itself exactly at the cap still
/// fits — sharing one bound silently rejected the last few bytes of the
/// advertised range.
pub const MAX_STORED_VALUE: usize = MAX_ENTRY_SIZE + 8;

/// Current store format version (bumped on breaking changes).
///
/// `get_agent_config` refuses an entry written by a *newer* store version:
/// this build cannot know which fields a future schema added, and handing
/// the caller a half-understood config is worse than reporting the mismatch.
const STORE_VERSION: u16 = 1;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A validated, versioned store entry.
///
/// The `payload` is the postcard-serialised config (or other typed config).
/// `version` tracks the schema version so a future migration can detect stale
/// entries and upgrade them on first read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoreEntry {
    /// Schema version (bumped by the caller on migration).
    pub version: u16,
    /// Postcard-serialised configuration bytes, `<= MAX_ENTRY_SIZE`.
    pub payload: Vec<u8, MAX_ENTRY_SIZE>,
}

impl StoreEntry {
    /// Create a new entry from a `postcard`-serialisable value.
    pub fn from_value<T: serde::Serialize>(value: &T) -> Result<Self> {
        let raw = postcard::to_vec::<T, MAX_ENTRY_SIZE>(value).map_err(|_| {
            AgentError::ConfigurationError {
                field: "store_entry.serialize",
                reason: ConfigError::TooLong,
            }
        })?;
        let mut payload: Vec<u8, MAX_ENTRY_SIZE> = Vec::new();
        payload.extend_from_slice(&raw).map_err(|_| {
            AgentError::ConfigurationError {
                field: "store_entry.payload",
                reason: ConfigError::TooLong,
            }
        })?;
        Ok(Self {
            version: STORE_VERSION,
            payload,
        })
    }

    /// Deserialise the payload back into `T`.
    pub fn to_value<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        postcard::from_bytes(&self.payload).map_err(|_| {
            AgentError::ConfigurationError {
                field: "store_entry.deserialize",
                reason: ConfigError::TypeMismatch,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Storage backend trait
// ---------------------------------------------------------------------------

/// Abstraction over the underlying persistent storage (NVS, file, mock).
///
/// The trait is deliberately simple — read/write/delete — so it compiles on
/// both host (`std`) and embedded (`no_std`) targets. The store handles
/// versioning, validation, and corruption detection at its own layer.
pub trait StorageBackend {
    /// Read `key` contents. Returns `Ok(Some(bytes))` on hit,
    /// `Ok(None)` if the key does not exist, `Err` on device-level failure.
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8, MAX_STORED_VALUE>>>;

    /// Write `data` to `key`, overwriting any existing value atomically
    /// (as far as the backend allows).
    fn write(&mut self, key: &str, data: &[u8]) -> Result<()>;

    /// Remove `key` from the backend.
    fn delete(&mut self, key: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// In-memory storage backend (host-side testing / default)
// ---------------------------------------------------------------------------

/// Volatile, in-memory storage backend. Data is lost when the store is dropped.
/// Use for host-side testing or as a working store when no NVS is available.
///
/// Writes are real: an entry written here reads back for the lifetime of the
/// backend, and the slot array is bounded by [`MAX_STORE_ENTRIES`].
#[derive(Debug, Default)]
pub struct MemoryBackend {
    entries: [Option<(heapless::String<MAX_KEY_LENGTH>, heapless::Vec<u8, MAX_STORED_VALUE>)>;
        MAX_STORE_ENTRIES],
}

impl MemoryBackend {
    /// Create a new, empty `MemoryBackend`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for MemoryBackend {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8, MAX_STORED_VALUE>>> {
        for (k, v) in self.entries.iter().flatten() {
            if k.as_str() == key {
                return Ok(Some(v.clone()));
            }
        }
        Ok(None)
    }

    fn write(&mut self, key: &str, data: &[u8]) -> Result<()> {
        let key_h = heapless::String::try_from(key).map_err(|_| {
            AgentError::ConfigurationError {
                field: "store.backend",
                reason: ConfigError::TooLong,
            }
        })?;
        let mut data_v: heapless::Vec<u8, MAX_STORED_VALUE> = heapless::Vec::new();
        data_v.extend_from_slice(data).map_err(|_| {
            AgentError::ConfigurationError {
                field: "store.backend",
                reason: ConfigError::TooLong,
            }
        })?;

        // Overwrite the existing key first, in place. Only then take the
        // first free slot. The old single-pass loop wrote a re-set key into
        // the first *hole* while the original entry remained later, so a
        // subsequent `delete` removed the new copy and `read` resurrected the
        // stale one.
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|slot| matches!(slot, Some((k, _)) if k.as_str() == key))
        {
            *slot = Some((key_h, data_v));
            return Ok(());
        }
        if let Some(slot) = self.entries.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((key_h, data_v));
            return Ok(());
        }
        Err(AgentError::ConfigurationError {
            field: "store.backend",
            reason: ConfigError::InvalidValue,
        })
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        for slot in self.entries.iter_mut() {
            if let Some((k, _)) = slot {
                if k.as_str() == key {
                    *slot = None;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// Former name for [`MemoryBackend`], kept so existing call sites keep
/// compiling.
///
/// The old split — a `MemoryBackend` whose writes were silent no-ops and a
/// `MutableMemoryBackend` that actually stored — was a trap: the trait takes
/// `&mut self`, so the "immutable" variant had no reason to exist, and a
/// caller who picked it by name lost every write while `set_agent_config`
/// returned `Ok(())`. One honest in-memory backend is enough.
#[deprecated(note = "use MemoryBackend; it now stores writes")]
pub type MutableMemoryBackend = MemoryBackend;

// ---------------------------------------------------------------------------
// ConfigStore
// ---------------------------------------------------------------------------

/// Centralised configuration store with versioning and validation.
///
/// ## Example
///
/// ```ignore
/// use magent_core::config_store::{ConfigStore, MemoryBackend};
///
/// let mut backend = MemoryBackend::new();
/// let mut store = ConfigStore::new(backend);
///
/// let cfg = AgentConfig::default().with_name("prod-node").unwrap();
/// store.set_agent_config(&cfg).unwrap();
///
/// let loaded = store.get_agent_config().unwrap().unwrap();
/// assert_eq!(loaded.name.as_str(), "prod-node");
/// ```
pub struct ConfigStore<B: StorageBackend> {
    backend: B,
    update_counter: u32,
}

impl<B: StorageBackend> ConfigStore<B> {
    /// Create a new store backed by `backend`.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            update_counter: 0,
        }
    }

    /// Read and deserialise the agent configuration. Returns `Ok(None)` if no
    /// config has been stored yet.
    pub fn get_agent_config(&mut self) -> Result<Option<AgentConfig>> {
        let raw = self.backend.read("agent_config")?;
        match raw {
            Some(bytes) => {
                let entry: StoreEntry = postcard::from_bytes(&bytes).map_err(|_| {
                    AgentError::ConfigurationError {
                        field: "agent_config.deserialize",
                        reason: ConfigError::TypeMismatch,
                    }
                })?;
                // `version` was written from day one but never read: an
                // entry from a future schema would have been deserialised
                // against this build's field set (or failed with a generic
                // type error). Refuse it explicitly instead — a caller can
                // then upgrade rather than run on a misread config. Older
                // versions stay readable; a migration hook can be added when
                // there is a v1-vs-v2 difference to migrate.
                if entry.version > STORE_VERSION {
                    return Err(AgentError::ConfigurationError {
                        field: "agent_config.version",
                        reason: ConfigError::InvalidValue,
                    });
                }
                let cfg: AgentConfig = entry.to_value()?;
                Ok(Some(cfg))
            }
            None => Ok(None),
        }
    }

    /// Validate and persist the agent configuration.
    pub fn set_agent_config(&mut self, config: &AgentConfig) -> Result<()> {
        // Validate before writing — aerospace-grade: fail closed.
        config.validate()?;

        let entry = StoreEntry::from_value(config)?;
        let mut buf: Vec<u8, MAX_STORED_VALUE> = Vec::new();
        let raw = postcard::to_vec::<StoreEntry, MAX_STORED_VALUE>(&entry).map_err(|_| {
            AgentError::ConfigurationError {
                field: "agent_config.serialize_entry",
                reason: ConfigError::TooLong,
            }
        })?;
        buf.extend_from_slice(&raw).map_err(|_| {
            AgentError::ConfigurationError {
                field: "agent_config.buffer",
                reason: ConfigError::TooLong,
            }
        })?;

        self.backend.write("agent_config", &buf)?;
        self.update_counter = self.update_counter.wrapping_add(1);
        Ok(())
    }

    /// Delete the stored agent configuration.
    pub fn delete_agent_config(&mut self) -> Result<()> {
        self.backend.delete("agent_config")?;
        self.update_counter = self.update_counter.wrapping_add(1);
        Ok(())
    }

    /// Reify an `AgentConfig` from a JSON string (import).
    ///
    /// The JSON is validated against the struct schema; if deserialisation
    /// succeeds, the result is validated with `AgentConfig::validate()` and
    /// stored.
    pub fn import_json(&mut self, json: &str) -> Result<AgentConfig> {
        let cfg: AgentConfig = serde_json::from_str(json).map_err(|_| {
            AgentError::ConfigurationError {
                field: "import",
                reason: ConfigError::TypeMismatch,
            }
        })?;
        // Validate the deserialised config before writing.
        cfg.validate()?;
        self.set_agent_config(&cfg)?;
        Ok(cfg)
    }

    /// Export the current agent config as a pretty-printed JSON string.
    pub fn export_json(&mut self) -> Result<heapless::String<2048>> {
        let cfg = self
            .get_agent_config()?
            .ok_or(AgentError::ConfigurationError {
                field: "export",
                reason: ConfigError::Empty,
            })?;
        let json_str = serde_json::to_string_pretty(&cfg).map_err(|_| {
            AgentError::ConfigurationError {
                field: "export.serialize",
                reason: ConfigError::TypeMismatch,
            }
        })?;
        // Fail rather than truncate: a cut mid-JSON is not a shorter
        // document, it is an undecodable one. (`AgentConfig`'s current
        // fields fit easily; the bound exists for the no_std target, not
        // as a lie about the exported value.)
        heapless::String::<2048>::try_from(json_str.as_str()).map_err(|_| {
            AgentError::ConfigurationError {
                field: "export.size",
                reason: ConfigError::TooLong,
            }
        })
    }

    /// Mutations performed through this store instance (wrapping counter).
    ///
    /// It is **not persisted**, so it restarts at zero with the process: it
    /// measures activity since construction, not lifetime flash wear. A
    /// backend that needs a wear bound must keep its own persistent count.
    pub fn update_count(&self) -> u32 {
        self.update_counter
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_round_trips_agent_config() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        // No config yet.
        assert!(store.get_agent_config().unwrap().is_none());

        let cfg = AgentConfig::default()
            .with_name("test-node")
            .unwrap()
            .with_max_iterations(50)
            .unwrap();
        store.set_agent_config(&cfg).unwrap();

        let loaded = store.get_agent_config().unwrap().unwrap();
        assert_eq!(loaded.name.as_str(), "test-node");
        assert_eq!(loaded.max_iterations, 50);
        assert_eq!(loaded.max_memory, crate::MAX_MEMORY_BUDGET as u32);
    }

    #[test]
    fn store_rejects_invalid_config() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        let bad = AgentConfig {
            name: heapless::String::try_from("").unwrap(),
            ..AgentConfig::default()
        };
        let err = store.set_agent_config(&bad).unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "name",
                ..
            }
        ));
        // Write must not have happened.
        assert!(store.get_agent_config().unwrap().is_none());
    }

    #[test]
    fn store_import_json() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        let json = r#"{
            "name": "imported-node",
            "max_iterations": 100,
            "max_memory": 65536,
            "watchdog_timeout_secs": 30,
            "ble_timeout_secs": 60,
            "skills_enabled": true,
            "max_skills": 10,
            "debug_enabled": false
        }"#;

        let cfg = store.import_json(json).unwrap();
        assert_eq!(cfg.name.as_str(), "imported-node");
        assert_eq!(cfg.max_iterations, 100);

        // Persisted.
        let loaded = store.get_agent_config().unwrap().unwrap();
        assert_eq!(loaded.name.as_str(), "imported-node");
    }

    #[test]
    fn store_rejects_invalid_json() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        let err = store.import_json("{invalid json}").unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "import",
                ..
            }
        ));
    }

    #[test]
    fn store_export_json() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        // Export with no config → error.
        assert!(store.export_json().is_err());

        let cfg = AgentConfig::default().with_name("export-test").unwrap();
        store.set_agent_config(&cfg).unwrap();

        let exported = store.export_json().unwrap();
        assert!(exported.contains("export-test"));
        assert!(exported.contains("max_iterations"));
    }

    #[test]
    fn store_delete_agent_config() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        let cfg = AgentConfig::default().with_name("delete-me").unwrap();
        store.set_agent_config(&cfg).unwrap();
        assert!(store.get_agent_config().unwrap().is_some());

        store.delete_agent_config().unwrap();
        assert!(store.get_agent_config().unwrap().is_none());
    }

    #[test]
    fn store_update_counter_increments() {
        let backend = MemoryBackend::new();
        let mut store = ConfigStore::new(backend);

        assert_eq!(store.update_count(), 0);

        let cfg = AgentConfig::default();
        store.set_agent_config(&cfg).unwrap();
        assert_eq!(store.update_count(), 1);

        store.set_agent_config(&cfg).unwrap();
        assert_eq!(store.update_count(), 2);

        store.delete_agent_config().unwrap();
        assert_eq!(store.update_count(), 3);
    }

    #[test]
    fn store_entry_serialize_round_trip() {
        let cfg = AgentConfig::default().with_name("entry-test").unwrap();
        let entry = StoreEntry::from_value(&cfg).unwrap();
        assert_eq!(entry.version, STORE_VERSION);

        let restored: AgentConfig = entry.to_value().unwrap();
        assert_eq!(restored.name.as_str(), "entry-test");
    }

    #[test]
    fn store_memory_backend_read_missing() {
        let mut backend = MemoryBackend::new();
        assert!(backend.read("nonexistent").unwrap().is_none());
    }

    #[test]
    fn store_memory_backend_overwrite() {
        let mut backend = MemoryBackend::new();

        backend.write("key1", b"value1").unwrap();
        let v = backend.read("key1").unwrap().unwrap();
        assert_eq!(v.as_slice(), b"value1");

        // Overwrite
        backend.write("key1", b"value2").unwrap();
        let v = backend.read("key1").unwrap().unwrap();
        assert_eq!(v.as_slice(), b"value2");

        // Delete
        backend.delete("key1").unwrap();
        assert!(backend.read("key1").unwrap().is_none());
    }

    #[test]
    fn store_overflow_key_rejected() {
        let mut backend = MemoryBackend::new();
        let long_key = "k".repeat(MAX_KEY_LENGTH + 1);
        let err = backend.write(&long_key, b"data").unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                reason: ConfigError::TooLong,
                ..
            }
        ));
    }

    #[test]
    fn store_entry_rejects_oversized_payload() {
        // Use a very long string whose postcard encoding exceeds MAX_ENTRY_SIZE.
        // A 600-byte string serializes as LEB128 length (2 bytes) + UTF-8 data
        // (600 bytes) = 602 bytes total, which overflows the 512-byte buffer.
        let long: alloc::string::String = core::iter::repeat_n('x', 600).collect();
        let err = StoreEntry::from_value(&long);
        assert!(err.is_err());
    }

    /// A payload exactly at `MAX_ENTRY_SIZE` must still be storable: the
    /// backend stores the *envelope*, which is a few bytes larger. Sharing one
    /// bound for both silently rejected the top of the advertised range.
    #[test]
    fn store_envelope_fits_a_full_size_payload() {
        let mut payload = heapless::Vec::<u8, MAX_ENTRY_SIZE>::new();
        payload
            .extend_from_slice(&[0xAB; MAX_ENTRY_SIZE])
            .expect("payload cap is MAX_ENTRY_SIZE");
        let entry = StoreEntry {
            version: STORE_VERSION,
            payload,
        };

        let bytes = postcard::to_vec::<StoreEntry, MAX_STORED_VALUE>(&entry).unwrap();
        assert!(
            bytes.len() > MAX_ENTRY_SIZE,
            "the envelope must be larger than its payload"
        );

        let mut backend = MemoryBackend::new();
        backend.write("k", &bytes).unwrap();
        let read = backend.read("k").unwrap().unwrap();
        let restored: StoreEntry = postcard::from_bytes(&read).unwrap();
        assert_eq!(restored.payload.len(), MAX_ENTRY_SIZE);
    }

    /// `get_agent_config` must report a *deserialise* error (not an empty
    /// config) when the stored bytes are not a valid `StoreEntry` — corruption
    /// has to surface as an error, never as a silently-defaulted agent.
    #[test]
    fn store_reports_a_deserialise_error_on_corruption() {
        let mut backend = MemoryBackend::new();
        backend.write("agent_config", b"\xff\xff\xff").unwrap();
        let mut store = ConfigStore::new(backend);

        let err = store.get_agent_config().unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "agent_config.deserialize",
                ..
            }
        ));
    }

    /// An entry written by a newer schema must be refused, not deserialised
    /// against this build's fields. `version` was stored but never read.
    #[test]
    fn store_rejects_a_newer_schema_version() {
        let mut backend = MemoryBackend::new();
        let cfg = AgentConfig::default();
        let entry = StoreEntry {
            version: STORE_VERSION + 1,
            payload: StoreEntry::from_value(&cfg).unwrap().payload,
        };
        let bytes = postcard::to_vec::<StoreEntry, MAX_STORED_VALUE>(&entry).unwrap();
        backend.write("agent_config", &bytes).unwrap();
        let mut store = ConfigStore::new(backend);

        let err = store.get_agent_config().unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "agent_config.version",
                ..
            }
        ));
    }

    /// A `StoreEntry` whose payload bytes were replaced with garbage must fail
    /// `to_value` rather than hand back a half-built config.
    #[test]
    fn store_entry_to_value_rejects_a_corrupt_payload() {
        let entry = StoreEntry {
            version: STORE_VERSION,
            payload: heapless::Vec::try_from([0xff_u8, 0xff, 0xff].as_slice()).unwrap(),
        };
        let err = entry.to_value::<AgentConfig>().unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "store_entry.deserialize",
                ..
            }
        ));
    }

    /// `import_json` validates the parsed config; a syntactically-valid JSON
    /// with an empty `name` must be rejected by `validate()` (and nothing
    /// written), not stored.
    #[test]
    fn store_import_json_rejects_a_config_that_fails_validation() {
        let mut store = ConfigStore::new(MemoryBackend::new());
        let err = store
            .import_json(r#"{"name":"","max_iterations":10,"max_memory":65536,"watchdog_timeout_secs":30,"ble_timeout_secs":60,"skills_enabled":true,"max_skills":10,"debug_enabled":false}"#)
            .unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError { field: "name", .. }
        ));
        assert!(store.get_agent_config().unwrap().is_none());
    }

    /// The in-memory backend has a fixed slot count; filling every slot and
    /// then inserting one more key must fail closed with `InvalidValue`
    /// instead of overwriting an existing entry.
    #[test]
    fn store_memory_backend_reports_full_when_out_of_slots() {
        let mut backend = MemoryBackend::new();
        for i in 0..MAX_STORE_ENTRIES {
            backend
                .write(&format!("ke{i}"), b"v")
                .expect("a slot is free");
        }
        let err = backend.write("one-too-many", b"v").unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                reason: ConfigError::InvalidValue,
                ..
            }
        ));
        // The overflow attempt did not clobber an existing key.
        assert!(backend.read("ke0").unwrap().is_some());
    }

    /// A stored-then-deleted key reads `None`, and a second delete is a no-op
    /// rather than an error.
    #[test]
    fn store_delete_is_idempotent() {
        let mut backend = MemoryBackend::new();
        backend.write("k", b"v").unwrap();
        backend.delete("k").unwrap();
        backend.delete("k").unwrap(); // second delete is a no-op, not an error
        assert!(backend.read("k").unwrap().is_none());
    }

    /// `MemoryBackend` writes are real. It used to return `Ok(())` from a
    /// no-op `write`, so `ConfigStore::set_agent_config` reported success and
    /// the value was gone on the next read.
    #[test]
    fn store_memory_backend_write_persists_until_deleted() {
        let mut backend = MemoryBackend::new();
        backend.write("k", b"v").unwrap();
        assert_eq!(backend.read("k").unwrap().unwrap().as_slice(), b"v");
        backend.delete("k").unwrap();
        assert!(backend.read("k").unwrap().is_none());
    }

    /// Re-writing a key that sits after a hole must overwrite that entry, not
    /// land in the hole and leave the old copy to "resurrect" after delete.
    #[test]
    fn store_memory_backend_overwrite_does_not_resurrect_deleted_values() {
        let mut backend = MemoryBackend::new();
        backend.write("a", b"a").unwrap();
        backend.write("b", b"b").unwrap();
        backend.write("k", b"old").unwrap();
        backend.delete("a").unwrap();
        backend.delete("b").unwrap();

        // `k` still lives in slot 2; this must overwrite it in place.
        backend.write("k", b"new").unwrap();
        backend.delete("k").unwrap();
        assert!(
            backend.read("k").unwrap().is_none(),
            "a deleted key must not resurface from a stale slot"
        );
    }

    /// The in-memory backend rejects oversized *data* as well as oversized
    /// keys — an envelope past `MAX_STORED_VALUE` cannot be represented in a
    /// slot.
    #[test]
    fn store_memory_backend_rejects_oversized_data() {
        let mut backend = MemoryBackend::new();
        let big = [0u8; MAX_STORED_VALUE + 1];
        let err = backend.write("k", &big).unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "store.backend",
                reason: ConfigError::TooLong,
            }
        ));
    }
}