//! ESP32 NVS Wallet Storage
//!
//! Provides secure storage of encrypted wallets in ESP32's NVS (Non-Volatile Storage).
//!
//! ## Security Model
//!
//! - Wallet secrets are stored as encrypted keystore JSON
//! - Encryption uses the device-bound key (BTDK1) derived from eFuse
//! - NVS itself is not encrypted; secrets are encrypted before storage
//! - This mirrors the existing `wifi_pass_seal_v2` pattern used for Wi-Fi passwords
//!
//! ## Storage Format
//!
//! ```text
//! NVS Key: "wallet_{index}"
//! Value: BTDK1:{hex(keystore_blob)}
//! ```
//!
//! The stored string is the hex-encoded output of
//! [`crate::web3::wallet::keystore::Keystore::to_hex`] (Argon2id +
//! AES-256-GCM, self-describing blob). It is *not* plaintext JSON and it
//! is *not* the Ethereum keystore v3 JSON format — those are historical
//! misdescriptions; the actual wire format is the opaque hex blob the
//! keystore produces. Callers must pass `Keystore::to_hex()` output here
//! and feed `load_wallet` output back through `Keystore::from_hex`.
//!
//! ## Limits
//!
//! - Maximum wallet size: ~512 bytes (hex-encoded keystore blob)
//! - Maximum wallets: 10 (configurable)
//! - Namespace: "wallet_store"

use alloc::string::{String, ToString};
use heapless::String as HeaplessString;

/// NVS key prefix for wallets
pub const WALLET_KEY_PREFIX: &str = "wallet_";

/// Maximum number of wallets that can be stored
pub const MAX_WALLETS: usize = 10;

/// Maximum keystore JSON size
pub const MAX_KEYSTORE_SIZE: usize = 512;

/// NVS namespace for wallet storage
pub const WALLET_NAMESPACE: &str = "wallet_store";

/// Maximum key name length
pub const MAX_WALLET_NAME_LEN: usize = 32;

/// Wallet storage entry metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletEntry {
    /// Wallet name (unique identifier)
    pub name: HeaplessString<MAX_WALLET_NAME_LEN>,
    /// Creation timestamp (Unix seconds)
    pub created_at: u64,
    /// Last access timestamp
    pub last_accessed: u64,
    /// Whether this wallet is the default
    pub is_default: bool,
}

/// Wallet store index (stored separately)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletStoreIndex {
    /// Number of wallets
    pub count: usize,
    /// Wallet entries
    pub wallets: [Option<WalletEntry>; MAX_WALLETS],
    /// Default wallet index
    pub default_index: Option<usize>,
}

impl Default for WalletStoreIndex {
    fn default() -> Self {
        Self {
            count: 0,
            wallets: [const { None }; MAX_WALLETS],
            default_index: None,
        }
    }
}

/// ESP32-specific wallet storage errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletStorageError {
    /// Wallet not found
    NotFound,
    /// Storage is full
    StorageFull,
    /// Invalid wallet name
    InvalidName,
    /// Encryption failed
    EncryptionFailed(String),
    /// Decryption failed
    DecryptionFailed(String),
    /// NVS error
    NvsError(String),
    /// Serialization error
    SerializationError,
    /// Wallet index key was present but failed to parse.
    ///
    /// `CorruptedIndex` is intentionally distinct from `SerializationError`
    /// because the appropriate recovery differs: serialization is typically
    /// raised on the *write* path with caller-provided bytes, whereas a
    /// corrupted index read at boot indicates NVS wear or partial-flash —
    /// and the operator should NOT have `store_wallet` silently overwrite
    /// the (possibly recoverable) index with a fresh empty one.
    /// Audit-2026-08.
    CorruptedIndex(String),
}

impl core::fmt::Display for WalletStorageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WalletStorageError::NotFound => write!(f, "wallet not found"),
            WalletStorageError::StorageFull => write!(f, "wallet storage is full"),
            WalletStorageError::InvalidName => write!(f, "invalid wallet name"),
            WalletStorageError::EncryptionFailed(s) => write!(f, "encryption failed: {}", s),
            WalletStorageError::DecryptionFailed(s) => write!(f, "decryption failed: {}", s),
            WalletStorageError::NvsError(s) => write!(f, "NVS error: {}", s),
            WalletStorageError::SerializationError => write!(f, "serialization error"),
            WalletStorageError::CorruptedIndex(s) => write!(f, "wallet index corrupted: {}", s),
        }
    }
}

impl From<WalletStorageError> for crate::web3::wallet::error::WalletError {
    fn from(e: WalletStorageError) -> Self {
        match e {
            WalletStorageError::NotFound => {
                crate::web3::wallet::error::WalletError::KeystoreError("wallet not found".into())
            }
            WalletStorageError::StorageFull => {
                crate::web3::wallet::error::WalletError::KeystoreError("storage full".into())
            }
            WalletStorageError::InvalidName => {
                crate::web3::wallet::error::WalletError::KeystoreError("invalid name".into())
            }
            WalletStorageError::EncryptionFailed(s) => {
                crate::web3::wallet::error::WalletError::EncryptionFailed(s)
            }
            WalletStorageError::DecryptionFailed(s) => {
                crate::web3::wallet::error::WalletError::DecryptionFailed(s)
            }
            WalletStorageError::NvsError(s) => {
                crate::web3::wallet::error::WalletError::KeystoreError(s)
            }
            WalletStorageError::SerializationError => {
                crate::web3::wallet::error::WalletError::KeystoreError(
                    "serialization failed".into(),
                )
            }
            WalletStorageError::CorruptedIndex(s) => {
                // Use `format!` through `alloc` only on the `std`-enabled
                // build to keep the `no_std + alloc` variant free of the
                // dependency. The `WalletError::KeystoreError` variant is
                // already `String`-backed.
                crate::web3::wallet::error::WalletError::KeystoreError(alloc::format!(
                    "wallet index corrupted: {s}"
                ))
            }
        }
    }
}

/// Store wallet keystore to NVS.
///
/// # Security
///
/// The `keystore_hex` value is stored **as-is** — NVS provides no
/// confidentiality. It MUST be the hex output of [`Keystore::to_hex`]
/// (Argon2id + AES-256-GCM, self-describing blob) and ideally further
/// sealed with the device-bound key (BTDK1, mirroring `wifi_pass_seal_v2`).
/// Never pass a plaintext private key or mnemonic here.
///
/// # Arguments
/// * `nvs` - The NVS handle
/// * `name` - Wallet name
/// * `keystore_hex` - The hex-encoded (encrypted) keystore blob from
///   [`Keystore::to_hex`]
///
/// # Returns
/// * `Ok(slot)` - The slot number where the wallet was stored
/// * `Err(WalletStorageError)` - If storage failed
#[cfg(feature = "esp32_nvs")]
pub fn store_wallet<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
    name: &str,
    keystore_hex: &str,
) -> Result<usize, WalletStorageError> {
    use crate::web3::wallet::error::WalletError;

    // Validate name
    if name.is_empty() || name.len() > MAX_WALLET_NAME_LEN {
        return Err(WalletStorageError::InvalidName);
    }

    // Load or create index. We now distinguish:
    //   * index absent (first boot) -> start from `default()`
    //   * index present + parseable -> use it
    //   * index present + UNPARSEABLE -> refuse to proceed; otherwise
    //     `store_wallet` would silently overwrite every other wallet on
    //     the device. (Audit-2026-08 H6)
    let mut index = match load_index(nvs)? {
        Some(idx) => idx,
        None => WalletStoreIndex::default(),
    };

    // Find existing slot or empty slot
    let slot =
        find_slot_by_name(&index, name).or_else(|| index.wallets.iter().position(|w| w.is_none()));

    let slot = match slot {
        Some(s) => s,
        None => return Err(WalletStorageError::StorageFull),
    };

    // Create entry
    // NOTE: `esp_timer_get_time()` is microseconds *since boot*, not the
    // Unix epoch. Until the device has synced wall-clock time this is only
    // a relative "boot order" marker; replace with `TimeSync::now_unix`
    // output once SNTP has run. We keep the division so the stored value
    // is in seconds and fits the `u64` field.
    let now = (esp_timer_get_time() / 1_000_000) as u64;
    let entry = WalletEntry {
        name: HeaplessString::from(name).map_err(|_| WalletStorageError::InvalidName)?,
        created_at: now,
        last_accessed: now,
        is_default: index.default_index.is_none(),
    };

    index.wallets[slot] = Some(entry);
    if index.default_index.is_none() {
        index.default_index = Some(slot);
    }
    index.count = index.wallets.iter().filter(|w| w.is_some()).count();

    // Save index first
    save_index(nvs, &index).map_err(|e| WalletStorageError::NvsError(e.to_string()))?;

    // Save keystore to slot
    let key = format!("{}{}", WALLET_KEY_PREFIX, slot);
    let value = keystore_hex;

    nvs.set_str(&key, value)
        .map_err(|e| WalletStorageError::NvsError(e.to_string()))?;

    Ok(slot)
}

/// Load wallet keystore from NVS
#[cfg(feature = "esp32_nvs")]
pub fn load_wallet<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
    name: &str,
) -> Result<String, WalletStorageError> {
    // Load index
    let index = load_index(nvs).ok_or(WalletStorageError::NotFound)?;

    // Find slot
    let slot = find_slot_by_name(&index, name).ok_or(WalletStorageError::NotFound)?;

    // Update last accessed
    if let Some(ref mut entry) = index.wallets[slot] {
        entry.last_accessed = (esp_timer_get_time() / 1_000_000) as u64;
        let _ = save_index(nvs, &index);
    }

    // Load keystore
    let key = format!("{}{}", WALLET_KEY_PREFIX, slot);
    let mut buf = [0u8; MAX_KEYSTORE_SIZE];
    nvs.get_str(&key, &mut buf)
        .map_err(|e| WalletStorageError::NvsError(e.to_string()))?
        .ok_or(WalletStorageError::NotFound)?;

    // Convert to String
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec())
        .map_err(|_| WalletStorageError::NvsError("invalid UTF-8".into()))
}

/// Delete wallet from NVS
#[cfg(feature = "esp32_nvs")]
pub fn delete_wallet<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
    name: &str,
) -> Result<(), WalletStorageError> {
    // Load index
    let mut index = load_index(nvs).ok_or(WalletStorageError::NotFound)?;

    // Find slot
    let slot = find_slot_by_name(&index, name).ok_or(WalletStorageError::NotFound)?;

    // Remove entry
    index.wallets[slot] = None;
    index.count = index.wallets.iter().filter(|w| w.is_some()).count();

    // Update default if needed
    if index.default_index == Some(slot) {
        index.default_index = index.wallets.iter().position(|w| w.is_some());
    }

    // Save index
    save_index(nvs, &index).map_err(|e| WalletStorageError::NvsError(e.to_string()))?;

    // Clear keystore
    let key = format!("{}{}", WALLET_KEY_PREFIX, slot);
    nvs.remove(&key)
        .map_err(|e| WalletStorageError::NvsError(e.to_string()))?;

    Ok(())
}

/// List all wallets
#[cfg(feature = "esp32_nvs")]
pub fn list_wallets<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
) -> Result<Vec<WalletEntry>, WalletStorageError> {
    let index = match load_index(nvs)? {
        Some(idx) => idx,
        None => WalletStoreIndex::default(),
    };
    Ok(index.wallets.into_iter().filter_map(|w| w).collect())
}

/// Get wallet count
#[cfg(feature = "esp32_nvs")]
pub fn wallet_count<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
) -> Result<usize, WalletStorageError> {
    let index = match load_index(nvs)? {
        Some(idx) => idx,
        None => WalletStoreIndex::default(),
    };
    Ok(index.count)
}

// ============================================================================
// Internal helpers
// ============================================================================

const INDEX_KEY: &str = "wallet_index";

/// Internal helper: load the wallet-store index from NVS.
///
/// Distinguishes three outcomes (audit-2026-08):
/// * `Ok(Some(idx))` — index was present and parsed cleanly.
/// * `Ok(None)` — index key absent (normal first-boot state).
/// * `Err(StorageCorrupted)` — index key *present* but failed to
///   parse, indicating NVS wear, partial write, or schema drift.
///
/// The previous version collapsed all three into `Option`, which
/// caused `store_wallet` to overwrite a corrupted index with a
/// fresh empty one — silently destroying every other wallet on
/// the device.
#[cfg(feature = "esp32_nvs")]
fn load_index<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
) -> Result<Option<WalletStoreIndex>, WalletStorageError> {
    let mut buf = [0u8; 256];
    let json_opt: Option<&str> = nvs
        .get_str(INDEX_KEY, &mut buf)
        .map_err(|e| WalletStorageError::NvsError(e.to_string()))?;
    let Some(json) = json_opt else {
        return Ok(None);
    };
    serde_json_core::from_str(json)
        .map(Some)
        .map_err(|e| WalletStorageError::CorruptedIndex(e.to_string()))
}

#[cfg(feature = "esp32_nvs")]
fn save_index<N: esp_idf_svc::nvs::NvsDefault>(
    nvs: &esp_idf_svc::nvs::EspNvs<N>,
    index: &WalletStoreIndex,
) -> Result<(), WalletStorageError> {
    let json =
        serde_json_core::to_string(index).map_err(|_| WalletStorageError::SerializationError)?;
    nvs.set_str(INDEX_KEY, &json)
        .map_err(|e| WalletStorageError::NvsError(e.to_string()))
}

fn find_slot_by_name(index: &WalletStoreIndex, name: &str) -> Option<usize> {
    index
        .wallets
        .iter()
        .position(|w| w.as_ref().map_or(false, |e| e.name.as_str() == name))
}

/// Get current Unix timestamp (ESP32)
#[cfg(feature = "esp32_nvs")]
fn esp_timer_get_time() -> i64 {
    // Safety: esp_timer_get_time is a safe FFI function
    unsafe { esp_idf_sys::esp_timer_get_time() }
}

// ============================================================================
// Non-ESP32 stubs (for testing)
// ============================================================================

#[cfg(not(feature = "esp32_nvs"))]
impl WalletStorageError {
    pub fn not_implemented() -> Self {
        WalletStorageError::NvsError("esp32_nvs feature not enabled".into())
    }
}
