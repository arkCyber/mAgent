//! NVS-backed persistence for the core `AgentConfig`.
//!
//! [`magent_core::config_store::ConfigStore`] owns the schema version,
//! validation and envelope encoding; this module supplies the ESP-IDF NVS
//! implementation of its [`StorageBackend`] and the boot-time helpers the
//! agent thread and `AT+AGENTCFG` share.
//!
//! The store lives in its own namespace (`magent_cfg`) so it cannot collide
//! with the Wi-Fi/AT keys in `magent` / `mag_at`, the Lua flash emulation in
//! `magent_lua`, or the device identity. Keys are the store's own names
//! (`agent_config` today), which fit NVS's 15-character key limit — the
//! backend rejects anything longer instead of truncating it into a different
//! key.
//!
//! # Failure policy
//!
//! * A **missing** entry is not an error: the board defaults apply.
//! * A **corrupt / unreadable** entry is reported by [`load`]; the boot path
//!   ([`load_or_default`]) logs and falls back to the board defaults so a bad
//!   blob cannot brick the device. `AT+AGENTCFG?` surfaces the error instead
//!   of pretending the defaults are what was stored.
//! * A **write** is committed before the handle closes; a failed
//!   `nvs_set_blob` / `nvs_commit` surfaces as `Err`, never as success.

use esp_idf_sys as sys;
use heapless::Vec;
use magent_core::config::AgentConfig;
use magent_core::config_store::{ConfigStore, StorageBackend, MAX_STORED_VALUE};
use magent_core::error::{AgentError, ConfigError, Result, StorageError};

/// NVS namespace holding the persisted agent config.
const NAMESPACE: &[u8] = b"magent_cfg\0";

/// NVS caps key names at 15 bytes (excluding the NUL terminator).
const NVS_KEY_MAX: usize = 15;

/// Board display name used when no config has been persisted.
///
/// One `cfg!` chain rather than two `#[cfg]` constants: `board-s3` is sometimes
/// enabled *on top of* the default `board-c61` (the S3 build script passes
/// `--no-default-features`, but a bare `--features board-s3` does not), and two
/// same-named constants would fail to compile in that combination. S3 wins,
/// matching the precedence of the other board switches (`reserved_pin_reason`).
const BOARD_AGENT_NAME: &str = if cfg!(feature = "board-s3") {
    "mAgent-ESP32-S3"
} else if cfg!(feature = "board-c61") {
    "mAgent-ESP32-C61"
} else {
    "mAgent-ESP32"
};

/// [`StorageBackend`] over the ESP-IDF default NVS partition.
#[derive(Debug, Clone, Copy, Default)]
pub struct EspNvsBackend;

impl EspNvsBackend {
    /// Create the backend. It holds no handle: each operation opens and closes
    /// the namespace, so no NVS handle is leaked if a write fails.
    pub const fn new() -> Self {
        Self
    }
}

fn config_error(field: &'static str, reason: ConfigError) -> AgentError {
    AgentError::ConfigurationError { field, reason }
}

fn read_error() -> AgentError {
    AgentError::StorageReadFailed {
        address: 0,
        reason: StorageError::ReadError,
    }
}

fn write_error() -> AgentError {
    AgentError::StorageWriteFailed {
        address: 0,
        reason: StorageError::WriteError,
    }
}

fn open() -> Result<sys::nvs_handle_t> {
    let mut handle: sys::nvs_handle_t = 0;
    // SAFETY: `NAMESPACE` is NUL-terminated; `handle` is written only when the
    // call succeeds, and it is closed by the caller on every path.
    let r = unsafe {
        sys::nvs_open(
            NAMESPACE.as_ptr() as *const core::ffi::c_char,
            sys::nvs_open_mode_t_NVS_READWRITE,
            &mut handle,
        )
    };
    if r != sys::ESP_OK {
        log::error!("[nvs] opening namespace magent_cfg failed: 0x{r:x}");
        return Err(config_error("config_nvs.open", ConfigError::NotConfigured));
    }
    Ok(handle)
}

fn close(handle: sys::nvs_handle_t) {
    // SAFETY: `handle` came from a successful `nvs_open` and is closed once.
    unsafe { sys::nvs_close(handle) };
}

fn c_key(key: &str) -> Result<[core::ffi::c_char; NVS_KEY_MAX + 1]> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > NVS_KEY_MAX {
        return Err(config_error("config_nvs.key", ConfigError::TooLong));
    }
    let mut out = [0 as core::ffi::c_char; NVS_KEY_MAX + 1];
    for (dst, &b) in out.iter_mut().zip(bytes) {
        *dst = b as core::ffi::c_char;
    }
    Ok(out)
}

impl StorageBackend for EspNvsBackend {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8, MAX_STORED_VALUE>>> {
        let ck = c_key(key)?;
        let handle = open()?;

        // First query the stored length (out_value = NULL, length = 0).
        let mut required: usize = 0;
        // SAFETY: read-only length query; `required` is written by NVS.
        let q =
            unsafe { sys::nvs_get_blob(handle, ck.as_ptr(), core::ptr::null_mut(), &mut required) };
        if q == sys::ESP_ERR_NVS_NOT_FOUND {
            close(handle);
            return Ok(None);
        }
        if q != sys::ESP_OK {
            log::error!("[nvs] reading '{}' length failed: 0x{q:x}", key);
            close(handle);
            return Err(read_error());
        }
        if required > MAX_STORED_VALUE {
            // A blob larger than the schema can represent is corruption, not
            // a config this build should try to read.
            close(handle);
            return Err(config_error("config_nvs.value", ConfigError::TooLong));
        }

        let mut buf: Vec<u8, MAX_STORED_VALUE> = Vec::new();
        if buf.resize(required, 0u8).is_err() {
            close(handle);
            return Err(config_error("config_nvs.value", ConfigError::TooLong));
        }
        let mut out_len = required;
        // SAFETY: `buf` has `required` initialised bytes, which is what the
        // preceding query reported; `out_len` is updated by NVS.
        let r = unsafe {
            sys::nvs_get_blob(
                handle,
                ck.as_ptr(),
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut out_len,
            )
        };
        close(handle);
        if r != sys::ESP_OK {
            log::error!("[nvs] reading '{}' failed: 0x{r:x}", key);
            return Err(read_error());
        }
        // NVS may report a shorter value if it changed between the two calls;
        // never expose uninitialised tail bytes.
        buf.truncate(out_len.min(required));
        Ok(Some(buf))
    }

    fn write(&mut self, key: &str, data: &[u8]) -> Result<()> {
        if data.len() > MAX_STORED_VALUE {
            return Err(config_error("config_nvs.value", ConfigError::TooLong));
        }
        let ck = c_key(key)?;
        let handle = open()?;
        // SAFETY: `data` is a live slice of `data.len()` bytes; NVS copies it
        // before returning, and the handle is closed on every path.
        let r = unsafe {
            sys::nvs_set_blob(
                handle,
                ck.as_ptr(),
                data.as_ptr() as *const core::ffi::c_void,
                data.len(),
            )
        };
        let committed = if r == sys::ESP_OK {
            // SAFETY: raw ESP-IDF NVS commit on an open handle.
            unsafe { sys::nvs_commit(handle) }
        } else {
            r
        };
        close(handle);
        if committed != sys::ESP_OK {
            log::error!("[nvs] writing '{}' failed: 0x{committed:x}", key);
            return Err(write_error());
        }
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        let ck = c_key(key)?;
        let handle = open()?;
        // SAFETY: raw ESP-IDF NVS key erase on an open handle.
        let r = unsafe { sys::nvs_erase_key(handle, ck.as_ptr()) };
        if r == sys::ESP_ERR_NVS_NOT_FOUND {
            // Deleting a missing key is a no-op, not an error.
            close(handle);
            return Ok(());
        }
        let committed = if r == sys::ESP_OK {
            // SAFETY: raw ESP-IDF NVS commit on an open handle.
            unsafe { sys::nvs_commit(handle) }
        } else {
            r
        };
        close(handle);
        if committed != sys::ESP_OK {
            log::error!("[nvs] erasing '{}' failed: 0x{committed:x}", key);
            return Err(write_error());
        }
        Ok(())
    }
}

/// Board defaults used when NVS holds no config.
///
/// 20 iterations and the board's dynamic-context ceiling. Note the memory
/// *limit* only binds if something calls `BudgetEnforcer::consume_memory`,
/// which no production path does: the real device context bound is the
/// heapless conversation (`MAX_CONVERSATION_MESSAGES` × `MAX_BUFFER_SIZE`
/// ≈ 40 KiB), so this is a configured ceiling, not an enforced one.
///
/// A constant failing validation is a build-time bug; log it and fall back to
/// the library defaults rather than panicking the agent thread.
pub fn default_agent_config() -> AgentConfig {
    let built = AgentConfig::new()
        .with_name(BOARD_AGENT_NAME)
        .and_then(|c| c.with_max_iterations(20))
        .and_then(|c| c.with_max_memory(magent_core::MAX_DYNAMIC_CONTEXT_BYTES as u32));
    match built {
        Ok(config) => config,
        Err(e) => {
            log::error!("[agent] board default config rejected ({e}); using library defaults");
            AgentConfig::default()
        }
    }
}

fn store() -> ConfigStore<EspNvsBackend> {
    ConfigStore::new(EspNvsBackend::new())
}

/// Read the persisted config. `Ok(None)` means nothing has been stored yet;
/// `Err` means the stored blob exists but is unreadable/corrupt.
pub fn load() -> Result<Option<AgentConfig>> {
    let mut store = store();
    store.get_agent_config()
}

/// Validate and persist `config`. Takes effect on the next boot.
pub fn save(config: &AgentConfig) -> Result<()> {
    let mut store = store();
    store.set_agent_config(config)
}

/// Delete the persisted config, so the next boot uses board defaults.
pub fn reset() -> Result<()> {
    let mut store = store();
    store.delete_agent_config()
}

/// Boot path: persisted config if present and readable, board defaults
/// otherwise. A corrupt entry is logged and does not brick the boot — an
/// operator can still `AT+AGENTCFG=RESET` over the serial line.
pub fn load_or_default() -> AgentConfig {
    match load() {
        Ok(Some(config)) => {
            log::info!(
                "[agent] loaded persisted config: name={} iterations={} max_memory={}",
                config.name.as_str(),
                config.max_iterations,
                config.max_memory
            );
            config
        }
        Ok(None) => default_agent_config(),
        Err(e) => {
            log::warn!("[agent] persisted config unreadable ({e}); using board defaults");
            default_agent_config()
        }
    }
}
