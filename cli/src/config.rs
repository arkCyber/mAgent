//! `magent config` subcommand — system-level configuration file.
//!
//! This is a separate file from the prompt store (`magent set-prompt`).
//! Where `set-prompt` manages **content** (what the LLM is told to act
//! like), `config` manages **plumbing** (which provider, which model,
//! which URLs, how aggressive the compression pipeline should be).
//!
//! The two together form a complete configuration surface:
//!
//! * `set-prompt` → JSON files with system prompts (auditable)
//! * `config`     → JSON file with runtime knobs (auditable)
//!
//! Both follow the same convention (XDG-style directory, schema
//! versioned JSON) so the audit pipeline is uniform.
//!
//! ## Storage
//!
//! The config file lives at:
//!
//! | Source                                | Path                                              |
//! | ------------------------------------- | ------------------------------------------------- |
//! | `$MAGENT_CONFIG_FILE` (explicit)      | the value of the env var                          |
//! | `$MAGENT_CONFIG_DIR/magent.json`      | per-user, XDG-compliant                           |
//! | macOS / Linux default                 | `~/.magent/config.json`                           |
//! | Windows default                       | `%APPDATA%\magent\config.json` (best-effort)      |
//!
//! If neither an explicit env var nor a writable home is available,
//! `config init` fails with a clear error rather than silently writing
//! to `/tmp`.
//!
//! ## Layering
//!
//! Effective configuration is computed by overlaying layers in this
//! order (later wins):
//!
//! 1. Built-in defaults baked into the binary.
//! 2. `~/.magent/config.json` (this file).
//! 3. Environment variables (`OLLAMA_HOST`, `DEEPSEEK_HOST`, …).
//! 4. CLI flags (`--provider`, `--model`, `--temperature`, …).
//!
//! Step 2 is what this module reads/writes. The other steps live in
//! [`crate::runner::build_runner`] and are not duplicated here.
//!
//! ## Schema
//!
//! v1 — the current shape. The top-level object is:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "provider": {
//!     "default": "ollama",
//!     "ollama": {
//!       "url": "http://localhost:11434",
//!       "model": "llama3.2",
//!       "api_key_env": "OLLAMA_API_KEY"
//!     },
//!     "deepseek": {
//!       "url": "https://api.deepseek.com/v1",
//!       "model": "deepseek-chat",
//!       "api_key_env": "DEEPSEEK_API_KEY"
//!     }
//!   },
//!   "sampling": {
//!     "temperature": 0.3,
//!     "num_predict": 512,
//!     "top_p": 1.0,
//!     "top_k": 40
//!   },
//!   "runner": {
//!     "max_iterations": 10,
//!     "max_tool_calls": 8,
//!     "probe_ollama_on_run": false
//!   },
//!   "compression": {
//!     "max_messages": 32,
//!     "tool_content_max_chars": 800
//!   },
//!   "io": {
//!     "no_color": false,
//!     "quiet_default": false,
//!     "json_default": false
//!   },
//!   "metadata": {
//!     "description": "…",
//!     "tags": ["..."]
//!   },
//!   "created_at": 1754716800,
//!   "updated_at": 1754716800
//! }
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::output::{Output, OutputKind};

/// Current schema version. Bump on breaking changes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

// ============================================================================
// Validation thresholds
// ============================================================================
// These are *advisory* bounds, not hard policy. The aim is to catch
// "user typed too many zeros" mistakes, not to enforce a specific
// scheduler. Keeping them in named constants lets tests, docs, and
// the validator share a single source of truth.

/// `sampling.temperature` must be in this range (inclusive).
pub const TEMPERATURE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
/// `sampling.top_p` must be in this range (inclusive).
pub const TOP_P_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
/// `sampling.top_k` is considered unreasonable above this.
pub const TOP_K_MAX: usize = 1_000_000;
/// `runner.max_iterations` is considered unreasonable above this.
pub const MAX_ITERATIONS_MAX: usize = 1_000;
/// `runner.max_tool_calls` is considered unreasonable above this.
pub const MAX_TOOL_CALLS_MAX: usize = 1_000;
/// `compression.tool_content_max_chars` is considered unreasonable above this.
pub const TOOL_CONTENT_MAX_CHARS_MAX: usize = 100_000;
/// `metadata.description` length above this triggers a warning.
pub const METADATA_DESCRIPTION_MAX: usize = 1_024;
/// `metadata.author` length above this triggers a warning.
pub const METADATA_AUTHOR_MAX: usize = 256;
/// `metadata.tags` count above this triggers a warning.
pub const METADATA_TAGS_MAX: usize = 32;
/// A single tag string above this length triggers a warning.
pub const METADATA_TAG_MAX: usize = 64;
/// `sampling.num_predict` is considered unreasonable above this.
pub const NUM_PREDICT_MAX: usize = 1_000_000;
/// Largest URL string we accept. Real-world Ollama / DeepSeek
/// URLs are well under 200 chars; a multi-kB URL is almost
/// certainly a paste accident or a hostile config and would
/// hang anything that prints it.
pub const URL_MAX: usize = 2_048;
/// Largest model name string we accept. Real model names are
/// typically under 100 chars (e.g. `gpt-4-1106-preview`,
/// `claude-3-5-sonnet-20241022`).
pub const MODEL_MAX: usize = 256;

/// Explicit override for the config file location.
pub const CONFIG_FILE_ENV: &str = "MAGENT_CONFIG_FILE";

/// Per-user config directory override.
pub const CONFIG_DIR_ENV: &str = "MAGENT_CONFIG_DIR";

/// Filename used inside the config directory.
pub const CONFIG_FILENAME: &str = "magent.json";

// ============================================================================
// Errors
// ============================================================================

/// Errors returned by the config layer. Each variant is specific enough
/// that the CLI can print a one-line actionable diagnostic.
#[derive(Debug)]
pub enum ConfigError {
    /// The user's home directory is unreachable; we can't auto-resolve
    /// the default config path.
    NoHomeDirectory,
    /// The user tried to read a key that doesn't exist.
    KeyNotFound { key: String, available: Vec<String> },
    /// A value couldn't be parsed as the requested type.
    TypeMismatch { key: String, expected: String, got: String },
    /// A key path is malformed (e.g. starts with `.` or contains `..`).
    InvalidKey(String),
    /// The config file exists but couldn't be read as JSON.
    Json { path: PathBuf, source: serde_json::Error },
    /// The config file uses a `schema_version` newer than we support.
    UnsupportedSchema { path: PathBuf, found: u32, supported: u32 },
    /// Writing the config file failed.
    Write { path: PathBuf, source: io::Error },
    /// I/O on the directory containing the config failed.
    DirIo { path: PathBuf, source: io::Error },
    /// `config validate` found one or more rule violations. The
    /// string contains a human-readable summary; the full issue
    /// list (with key paths and messages) is also written to the
    /// JSON envelope when `OutputKind::Json` is in use.
    Validation { summary: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoHomeDirectory => write!(
                f,
                "could not determine a config location: set $MAGENT_CONFIG_FILE or $HOME"
            ),
            ConfigError::KeyNotFound { key, available } => {
                write!(f, "config key {:?} not found; available keys: {:?}", key, available)
            }
            ConfigError::TypeMismatch { key, expected, got } => write!(
                f,
                "config key {:?}: expected {}, got {:?}",
                key, expected, got
            ),
            ConfigError::InvalidKey(k) => write!(
                f,
                "invalid config key {:?}: keys use dotted paths like `provider.ollama.url`",
                k
            ),
            ConfigError::Json { path, source } => {
                write!(f, "could not parse {}: {}", path.display(), source)
            }
            ConfigError::UnsupportedSchema { path, found, supported } => write!(
                f,
                "{} has schema_version {} but this magent binary only understands up to {}; \
                 upgrade the binary first",
                path.display(),
                found,
                supported
            ),
            ConfigError::Write { path, source } => {
                write!(f, "could not write {}: {}", path.display(), source)
            }
            ConfigError::DirIo { path, source } => {
                write!(f, "config directory {}: {}", path.display(), source)
            }
            ConfigError::Validation { summary } => {
                write!(f, "config validation failed: {}", summary)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(source: io::Error) -> Self {
        ConfigError::Write {
            path: PathBuf::from("(io)"),
            source,
        }
    }
}

// ============================================================================
// Records
// ============================================================================

/// The on-disk config shape. Every field is optional so users can
/// write partial files; missing fields fall back to the built-in
/// defaults at load time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigRecord {
    pub schema_version: u32,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub sampling: SamplingConfig,
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub io: IoConfig,
    #[serde(default)]
    pub metadata: ConfigMetadata,
    /// Unix seconds. Set on first write, refreshed on every update.
    pub created_at: u64,
    /// Unix seconds. Refreshed on every update.
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderConfig {
    /// `"ollama"` or `"deepseek"`. Empty → use the binary default.
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub ollama: ProviderEndpoint,
    #[serde(default)]
    pub deepseek: ProviderEndpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderEndpoint {
    /// Base URL. Empty → use the binary default.
    #[serde(default)]
    pub url: String,
    /// Model name. Empty → use the binary default.
    #[serde(default)]
    pub model: String,
    /// Name of the env var that holds the API key (e.g. `DEEPSEEK_API_KEY`).
    /// We deliberately do NOT store the key itself in the config file —
    /// this is the explicit "don't leak secrets to disk" choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SamplingConfig {
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_num_predict")]
    pub num_predict: usize,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_temperature() -> f32 { 0.3 }
fn default_num_predict() -> usize { 512 }
fn default_top_p() -> f32 { 1.0 }
fn default_top_k() -> usize { 40 }

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            num_predict: default_num_predict(),
            top_p: default_top_p(),
            top_k: default_top_k(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default)]
    pub probe_ollama_on_run: bool,
}

fn default_max_iterations() -> usize { 10 }
fn default_max_tool_calls() -> usize { 8 }

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            max_tool_calls: default_max_tool_calls(),
            probe_ollama_on_run: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompressionConfig {
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    #[serde(default = "default_tool_chars")]
    pub tool_content_max_chars: usize,
}

fn default_max_messages() -> usize { 32 }
fn default_tool_chars() -> usize { 800 }

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            max_messages: default_max_messages(),
            tool_content_max_chars: default_tool_chars(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IoConfig {
    #[serde(default)]
    pub no_color: bool,
    #[serde(default)]
    pub quiet_default: bool,
    #[serde(default)]
    pub json_default: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConfigMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Free-form owner/author name. Surfaced in audit logs and
    /// shown by `magent config show` so a team can tell who
    /// tuned the file. Optional; defaults to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ConfigRecord {
    /// Build a complete record with every field at its built-in
    /// default. Used by `magent config init` and by tests.
    pub fn with_defaults() -> Self {
        let now = now_unix_seconds();
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            provider: ProviderConfig {
                default: "ollama".to_string(),
                ollama: ProviderEndpoint {
                    url: "http://localhost:11434".to_string(),
                    model: "llama3.2".to_string(),
                    api_key_env: Some("OLLAMA_API_KEY".to_string()),
                },
                deepseek: ProviderEndpoint {
                    url: "https://api.deepseek.com/v1".to_string(),
                    model: "deepseek-chat".to_string(),
                    api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                },
            },
            sampling: SamplingConfig::default(),
            runner: RunnerConfig::default(),
            compression: CompressionConfig::default(),
            io: IoConfig::default(),
            metadata: ConfigMetadata::default(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Refresh `updated_at` and bump `schema_version`. `created_at`
    /// is preserved so audit logs see when the config was first
    /// written.
    pub fn touched(mut self, now: u64) -> Self {
        self.updated_at = now;
        self.schema_version = CURRENT_SCHEMA_VERSION;
        self
    }
}

// ============================================================================
// Path resolution
// ============================================================================

/// Resolve the canonical config file path, honouring
/// `MAGENT_CONFIG_FILE` first, then `MAGENT_CONFIG_DIR`, then
/// `$XDG_CONFIG_HOME/magent/magent.json`, then
/// `$HOME/.magent/config.json`.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    // 1. Explicit file override.
    if let Ok(p) = std::env::var(CONFIG_FILE_ENV) {
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p));
        }
    }

    // 2. Explicit directory override.
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV) {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir).join(CONFIG_FILENAME));
        }
    }

    // 3. XDG_CONFIG_HOME/magent/magent.json (or ~/.config/magent/magent.json).
    if let Some(base) = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|h| format!("{}/.config", h))
        })
    {
        return Ok(PathBuf::from(base).join("magent").join(CONFIG_FILENAME));
    }

    Err(ConfigError::NoHomeDirectory)
}

/// Print where the config file lives. Useful for `magent config show`
/// and `--help` output.
pub fn config_path_string() -> String {
    config_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unresolved — set $MAGENT_CONFIG_FILE or $HOME>".to_string())
}

/// Make sure the directory containing the config file exists.
pub fn ensure_config_dir() -> Result<PathBuf, ConfigError> {
    let path = config_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| ConfigError::DirIo {
            path: path.clone(),
            source: io::Error::other("config file has no parent directory"),
        })?;
    fs::create_dir_all(dir).map_err(|source| ConfigError::DirIo {
        path: dir.to_path_buf(),
        source,
    })?;
    Ok(dir.to_path_buf())
}

// ============================================================================
// Load / save
// ============================================================================

/// Load the config file. Returns the built-in defaults if the file
/// doesn't exist yet (so first-run code paths don't have to special-case
/// `NotFound`).
pub fn load() -> Result<ConfigRecord, ConfigError> {
    let path = config_path()?;
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(ConfigRecord::with_defaults());
        }
        Err(source) => {
            return Err(ConfigError::DirIo { path, source })
        }
    };
    let record: ConfigRecord =
        serde_json::from_str(&raw).map_err(|source| ConfigError::Json {
            path: path.clone(),
            source,
        })?;
    if record.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchema {
            path,
            found: record.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(record)
}

/// Save the config file. Preserves `created_at` from any existing file
/// so audit logs don't lose the original timestamp.
pub fn save(record: ConfigRecord) -> Result<PathBuf, ConfigError> {
    let _ = ensure_config_dir()?;
    let path = config_path()?;
    let created_at = match fs::read_to_string(&path) {
        Ok(existing) => serde_json::from_str::<ConfigRecord>(&existing)
            .map(|r| r.created_at)
            .unwrap_or(record.created_at),
        Err(_) => record.created_at,
    };

    let mut record = record;
    record.created_at = created_at;
    record.updated_at = now_unix_seconds();
    record.schema_version = CURRENT_SCHEMA_VERSION;

    let json = serde_json::to_string_pretty(&record).map_err(|source| ConfigError::Json {
        path: path.clone(),
        source,
    })?;
    // Write with owner-only permissions so the config (which can carry
    // credential-related settings) isn't world-readable.
    write_config_file(&path, &json)?;
    Ok(path)
}

/// Write the config file with owner-only permissions (0600 on Unix).
///
/// The config can carry provider endpoints and credential-related settings,
/// so it must not be left world-readable (the default `fs::write` mode of
/// 0644 would expose it to any other local user). On non-Unix platforms we
/// fall back to `fs::write` (there is no POSIX mode bit to set).
fn write_config_file(path: &Path, contents: &str) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        f.write_all(contents.as_bytes()).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        f.flush().map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Delete the config file. Returns `Ok(true)` if a file was removed,
/// `Ok(false)` if it didn't exist.
pub fn delete() -> Result<bool, ConfigError> {
    let path = config_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ConfigError::Write { path, source }),
    }
}

// ============================================================================
// Key-path accessors
// ============================================================================

/// Resolve a dotted key like `provider.ollama.url` against a
/// [`ConfigRecord`]. Returns the rendered JSON value (so the caller
/// can pretty-print it) plus a list of available keys at the same
/// depth for error messages.
///
/// The lookup is intentionally a small hand-rolled tree rather than a
/// generic reflection-based accessor. We don't need generality; we
/// need legible error messages and predictable behaviour when keys
/// are missing.
pub fn get(record: &ConfigRecord, key: &str) -> Result<serde_json::Value, ConfigError> {
    if key.is_empty() {
        return Err(ConfigError::InvalidKey(key.to_string()));
    }
    if key.contains("..") {
        return Err(ConfigError::InvalidKey(key.to_string()));
    }
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() > 3 {
        return Err(ConfigError::InvalidKey(key.to_string()));
    }
    match parts.as_slice() {
        ["provider"] => Ok(json_of(&record.provider)),
        ["provider", "default"] => Ok(json_of(&record.provider.default)),
        ["provider", name] => endpoint_value(&record.provider, name),
        ["provider", name, field] => endpoint_field(&record.provider, name, field),
        ["sampling"] => Ok(json_of(&record.sampling)),
        ["sampling", field] => sampling_field(&record.sampling, field),
        ["runner"] => Ok(json_of(&record.runner)),
        ["runner", field] => runner_field(&record.runner, field),
        ["compression"] => Ok(json_of(&record.compression)),
        ["compression", field] => compression_field(&record.compression, field),
        ["io"] => Ok(json_of(&record.io)),
        ["io", field] => io_field(&record.io, field),
        ["metadata"] => Ok(json_of(&record.metadata)),
        ["metadata", "description"] => Ok(json_of(&record.metadata.description)),
        ["metadata", "author"] => Ok(json_of(&record.metadata.author)),
        ["metadata", "tags"] => Ok(json_of(&record.metadata.tags)),
        // Per-index tag lookup. `magent config get metadata.tags.0`
        // returns the first tag as a string. Useful for scripts
        // that want to grep a single tag without parsing the whole
        // array. Out-of-range indices are NOT errors here — they
        // surface as `null`, matching JS-style semantics — so a
        // script can use them defensively.
        ["metadata", "tags", index] => {
            let i: usize = index.parse().map_err(|_| {
                ConfigError::InvalidKey(format!(
                    "metadata.tags.{} (expected non-negative integer)",
                    index
                ))
            })?;
            Ok(record
                .metadata
                .tags
                .get(i)
                .cloned()
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null))
        }
        ["schema_version"] => Ok(json_of(record.schema_version)),
        _ => Err(ConfigError::KeyNotFound {
            key: key.to_string(),
            available: top_level_keys(),
        }),
    }
}

/// Mutate a single dotted key. Returns the updated record. Complex
/// nested keys (`provider.ollama.url`) are fully supported; type
/// mismatches surface as [`ConfigError::TypeMismatch`].
pub fn set(record: ConfigRecord, key: &str, value: serde_json::Value) -> Result<ConfigRecord, ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["provider", "default"] => {
            let new = match value.as_str() {
                Some(s) => s.to_string(),
                None => return Err(type_err(key, "string")),
            };
            // Empty string is a "clear" sentinel (preserved from
            // earlier behaviour). Only whitespace-only *non-empty*
            // strings are rejected: an empty string is the user's
            // deliberate "no default" choice and the runner
            // already handles it.
            if !new.is_empty() && new.trim().is_empty() {
                return Err(type_err(key, "non-empty, non-whitespace string"));
            }
            if new.chars().any(|c| c.is_control()) {
                return Err(ConfigError::TypeMismatch {
                    key: key.to_string(),
                    expected: "string without control characters".to_string(),
                    got: format!("string of {} characters", new.len()),
                });
            }
            // Cap the length. A 1-MB string here is
            // almost certainly a paste accident and would
            // slow down every comparison.
            if new.len() > MODEL_MAX {
                return Err(ConfigError::TypeMismatch {
                    key: key.to_string(),
                    expected: format!("string of at most {} characters", MODEL_MAX),
                    got: format!("string of {} characters", new.len()),
                });
            }
            Ok(ConfigRecord {
                provider: ProviderConfig {
                    default: new,
                    ..record.provider
                },
                ..record
            })
        }
        ["provider", name, field] => {
            let endpoint = match *name {
                "ollama" => &record.provider.ollama,
                "deepseek" => &record.provider.deepseek,
                other => {
                    return Err(ConfigError::KeyNotFound {
                        key: format!("provider.{}.{}", other, field),
                        available: vec!["provider.ollama".into(), "provider.deepseek".into()],
                    })
                }
            };
            let updated = endpoint_set(endpoint.clone(), field, &value, key)?;
            let mut next = record;
            match *name {
                "ollama" => next.provider.ollama = updated,
                "deepseek" => next.provider.deepseek = updated,
                _ => unreachable!(),
            }
            Ok(next)
        }
        ["sampling", field] => sampling_set(record, field, value, key),
        ["runner", field] => runner_set(record, field, value, key),
        ["compression", field] => compression_set(record, field, value, key),
        ["io", field] => io_set(record, field, value, key),
        ["metadata", field] => metadata_set(record, field, value, key),
        _ => Err(ConfigError::KeyNotFound {
            key: key.to_string(),
            available: top_level_keys(),
        }),
    }
}

/// Setter for the `metadata.*` keys. Currently supports
/// `description` and `author` (both `Option<String>`), and
/// replaces the full `tags` list via `tags=foo,bar,baz` style
/// values (`array` JSON only). Tag list edits are deliberately
/// limited to "replace the whole list" — we don't expose
/// append/remove because that would invite users to write
/// `magent set metadata.tags +nrf52` to a config that may or may
/// not have a `+` syntax they remember, and a typo there is
/// worse than just editing the file.
///
/// [`parse_optional_string`] is the small helper used by the
/// `description` and `author` arms to accept `null`, `""`, or
/// a non-empty string uniformly.
fn metadata_set(
    mut record: ConfigRecord,
    field: &str,
    value: serde_json::Value,
    key: &str,
) -> Result<ConfigRecord, ConfigError> {
    match field {
        "description" => {
            let new = parse_optional_string(&value, key)?;
            // Cap the length to keep the file manageable. Description
            // is shown verbatim in `magent config show`, so a
            // 100-MB description would hang the terminal. We allow
            // a generous cap (`METADATA_DESCRIPTION_MAX`) — much
            // larger than a sane human description but small enough
            // to be safe.
            if let Some(d) = &new {
                if d.len() > METADATA_DESCRIPTION_MAX {
                    return Err(ConfigError::TypeMismatch {
                        key: key.to_string(),
                        expected: format!(
                            "string of at most {} characters",
                            METADATA_DESCRIPTION_MAX
                        ),
                        got: format!("string of {} characters", d.len()),
                    });
                }
                // Refuse control characters so a paste accident
                // (or a malicious config edit) can't smuggle
                // terminal escapes into the trace output. We
                // surface the count rather than the literal
                // character — the value itself may be hostile.
                if d.chars().any(|c| c.is_control()) {
                    return Err(ConfigError::TypeMismatch {
                        key: key.to_string(),
                        expected: "string without control characters".to_string(),
                        got: format!("string of {} characters", d.len()),
                    });
                }
            }
            record.metadata.description = new;
            Ok(record)
        }
        "author" => {
            let new = parse_optional_string(&value, key)?;
            // Refuse control characters at write time so a paste
            // accident (or a hostile config edit) can't smuggle
            // terminal escapes into trace output. We surface the
            // offending character count rather than the literal
            // string (the value itself may be hostile).
            if let Some(a) = &new {
                if a.chars().any(|c| c.is_control()) {
                    return Err(ConfigError::TypeMismatch {
                        key: key.to_string(),
                        expected: "string without control characters".to_string(),
                        got: format!(
                            "string with {} control character(s)",
                            a.chars().filter(|c| c.is_control()).count(),
                        ),
                    });
                }
                if a.len() > METADATA_AUTHOR_MAX {
                    return Err(ConfigError::TypeMismatch {
                        key: key.to_string(),
                        expected: format!("string of at most {} characters", METADATA_AUTHOR_MAX),
                        got: format!("string of {} characters", a.len()),
                    });
                }
            }
            record.metadata.author = new;
            Ok(record)
        }
        "tags" => {
            // Three accepted forms (matched in this order so the
            // cheapest check wins):
            //
            // 1. JSON array of strings, e.g. `["a", "b"]`.
            // 2. A JSON-encoded non-array (typically a string
            //    like `"a,b,c"` or a JSON object) — we
            //    fall back to comma-splitting the original input.
            //    This handles `magent config set metadata.tags
            //    '"a,b"'` (accidental JSON-quoting) gracefully.
            // 3. A plain comma-separated string, e.g. `a,b,c`.
            //    This is the form the user will type most often.
            let arr: Vec<serde_json::Value> = match &value {
                serde_json::Value::Array(_) => {
                    value.as_array().cloned().unwrap_or_default()
                }
                serde_json::Value::String(s) => {
                    // If the user actually handed us a JSON string
                    // *of* an array, prefer that. Otherwise (and
                    // for any other JSON shape that isn't an
                    // array) split on commas. An empty string is
                    // a "clear" sentinel and maps to an empty
                    // list — matches `description`/`author` and
                    // the `null` branch below.
                    if s.is_empty() {
                        Vec::new()
                    } else if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                        if let Some(a) = parsed.as_array() {
                            a.clone()
                        } else {
                            s.split(',')
                                .map(|t| serde_json::Value::String(t.trim().to_string()))
                                .collect()
                        }
                    } else {
                        s.split(',')
                            .map(|t| serde_json::Value::String(t.trim().to_string()))
                            .collect()
                    }
                }
                // `null` is a "clear" sentinel: writing
                // `metadata.tags = null` removes every tag. We
                // could equivalently accept `[]`, but accepting
                // `null` matches the shape of `--tags=NULL` in
                // languages where Option<String> is the natural
                // representation.
                serde_json::Value::Null => Vec::new(),
                _ => {
                    return Err(type_err(
                        key,
                        "array of strings, comma-separated string, or null",
                    ))
                }
            };
            let mut tags = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                let s = v.as_str().ok_or_else(|| {
                    ConfigError::TypeMismatch {
                        key: format!("{}[{}]", key, i),
                        expected: "string".to_string(),
                        got: format!("{:?}", v),
                    }
                })?;
                if s.is_empty() {
                    // "wrong type" would be misleading here —
                    // the value parses as a string but is empty.
                    // Use a custom TypeMismatch so the user sees
                    // an accurate message.
                    return Err(ConfigError::TypeMismatch {
                        key: format!("{}[{}]", key, i),
                        expected: "non-empty string".to_string(),
                        got: "empty string".to_string(),
                    });
                }
                if s != s.trim() {
                    return Err(ConfigError::TypeMismatch {
                        key: format!("{}[{}]", key, i),
                        expected: "string without leading/trailing whitespace".to_string(),
                        got: format!("string with surrounding whitespace: {:?}", s),
                    });
                }
                if s.len() > METADATA_TAG_MAX {
                    return Err(ConfigError::TypeMismatch {
                        key: format!("{}[{}]", key, i),
                        expected: format!("string of at most {} characters", METADATA_TAG_MAX),
                        got: format!("string of {} characters", s.len()),
                    });
                }
                tags.push(s.to_string());
            }
            // Cap the total count. We surface this *after* the
            // per-tag checks so the user sees the most useful
            // error first (a typo in one tag rather than the
            // global count).
            if tags.len() > METADATA_TAGS_MAX {
                return Err(ConfigError::TypeMismatch {
                    key: key.to_string(),
                    expected: format!("at most {} tags", METADATA_TAGS_MAX),
                    got: format!("{} tags", tags.len()),
                });
            }
            record.metadata.tags = tags;
            Ok(record)
        }
        other => Err(ConfigError::KeyNotFound {
            key: format!("metadata.{}", other),
            available: vec!["metadata.description".into(), "metadata.author".into(), "metadata.tags".into()],
        }),
    }
}

/// Flatten the record into `key → value` pairs for `magent config list`.
pub fn flatten(record: &ConfigRecord) -> Vec<(String, serde_json::Value)> {
    // Unconditional rows go into a `vec!` literal so the allocator
    // gets the right capacity upfront (vs. many `Vec::new()` +
    // `push` growths). Conditional rows (`if let Some`, `for`) are
    // collected separately and extended at the end.
    let mut out = vec![
        ("schema_version".to_string(), json_of(record.schema_version)),
        ("provider.default".to_string(), json_of(&record.provider.default)),
        ("provider.ollama.url".to_string(), json_of(&record.provider.ollama.url)),
        ("provider.ollama.model".to_string(), json_of(&record.provider.ollama.model)),
        ("provider.deepseek.url".to_string(), json_of(&record.provider.deepseek.url)),
        ("provider.deepseek.model".to_string(), json_of(&record.provider.deepseek.model)),
        ("sampling.temperature".to_string(), json_of(record.sampling.temperature)),
        ("sampling.num_predict".to_string(), json_of(record.sampling.num_predict)),
        ("sampling.top_p".to_string(), json_of(record.sampling.top_p)),
        ("sampling.top_k".to_string(), json_of(record.sampling.top_k)),
        ("runner.max_iterations".to_string(), json_of(record.runner.max_iterations)),
        ("runner.max_tool_calls".to_string(), json_of(record.runner.max_tool_calls)),
        ("runner.probe_ollama_on_run".to_string(), json_of(record.runner.probe_ollama_on_run)),
        ("compression.max_messages".to_string(), json_of(record.compression.max_messages)),
        ("compression.tool_content_max_chars".to_string(), json_of(record.compression.tool_content_max_chars)),
        ("io.no_color".to_string(), json_of(record.io.no_color)),
        ("io.quiet_default".to_string(), json_of(record.io.quiet_default)),
        ("io.json_default".to_string(), json_of(record.io.json_default)),
    ];
    if let Some(env) = &record.provider.ollama.api_key_env {
        out.push(("provider.ollama.api_key_env".to_string(), json_of(env)));
    }
    if let Some(env) = &record.provider.deepseek.api_key_env {
        out.push(("provider.deepseek.api_key_env".to_string(), json_of(env)));
    }
    if let Some(desc) = &record.metadata.description {
        out.push(("metadata.description".to_string(), json_of(desc)));
    }
    if let Some(author) = &record.metadata.author {
        out.push(("metadata.author".to_string(), json_of(author)));
    }
    for tag in &record.metadata.tags {
        out.push(("metadata.tags[]".to_string(), json_of(tag)));
    }
    out
}

fn top_level_keys() -> Vec<String> {
    vec![
        "schema_version".to_string(),
        "provider".to_string(),
        "sampling".to_string(),
        "runner".to_string(),
        "compression".to_string(),
        "io".to_string(),
        "metadata".to_string(),
    ]
}

// ============================================================================
// Per-field helpers
// ============================================================================

fn json_of<T: Serialize>(v: T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

fn endpoint_value(p: &ProviderConfig, name: &str) -> Result<serde_json::Value, ConfigError> {
    match name {
        "ollama" => Ok(json_of(&p.ollama)),
        "deepseek" => Ok(json_of(&p.deepseek)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("provider.{}", other),
            available: vec!["ollama".into(), "deepseek".into()],
        }),
    }
}

fn endpoint_field(p: &ProviderConfig, name: &str, field: &str) -> Result<serde_json::Value, ConfigError> {
    let ep = match name {
        "ollama" => &p.ollama,
        "deepseek" => &p.deepseek,
        other => {
            return Err(ConfigError::KeyNotFound {
                key: format!("provider.{}.{}", other, field),
                available: vec!["ollama".into(), "deepseek".into()],
            })
        }
    };
    match field {
        "url" => Ok(json_of(&ep.url)),
        "model" => Ok(json_of(&ep.model)),
        "api_key_env" => Ok(json_of(&ep.api_key_env)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("provider.{}.{}", name, other),
            available: vec!["url".into(), "model".into(), "api_key_env".into()],
        }),
    }
}

fn endpoint_set(ep: ProviderEndpoint, field: &str, value: &serde_json::Value, full_key: &str) -> Result<ProviderEndpoint, ConfigError> {
    match field {
        "url" => {
            let s = value.as_str().ok_or_else(|| type_err(full_key, "string"))?;
            if s.chars().any(|c| c.is_control()) {
                return Err(ConfigError::TypeMismatch {
                    key: full_key.to_string(),
                    expected: "string without control characters".to_string(),
                    got: format!("string with {} control character(s)", s.chars().filter(|c| c.is_control()).count()),
                });
            }
            if s.len() > URL_MAX {
                return Err(ConfigError::TypeMismatch {
                    key: full_key.to_string(),
                    expected: format!("string of at most {} characters", URL_MAX),
                    got: format!("string of {} characters", s.len()),
                });
            }
            Ok(ProviderEndpoint {
                url: s.to_string(),
                ..ep
            })
        }
        "model" => {
            let s = value.as_str().ok_or_else(|| type_err(full_key, "string"))?;
            if s.chars().any(char::is_whitespace) {
                return Err(ConfigError::TypeMismatch {
                    key: full_key.to_string(),
                    expected: "string without whitespace".to_string(),
                    got: format!("string with whitespace: {:?}", s),
                });
            }
            if s.chars().any(|c| c.is_control()) {
                return Err(ConfigError::TypeMismatch {
                    key: full_key.to_string(),
                    expected: "string without control characters".to_string(),
                    got: format!("string with {} control character(s)", s.chars().filter(|c| c.is_control()).count()),
                });
            }
            if s.len() > MODEL_MAX {
                return Err(ConfigError::TypeMismatch {
                    key: full_key.to_string(),
                    expected: format!("string of at most {} characters", MODEL_MAX),
                    got: format!("string of {} characters", s.len()),
                });
            }
            Ok(ProviderEndpoint {
                model: s.to_string(),
                ..ep
            })
        }
        "api_key_env" => {
            let s = value.as_str().ok_or_else(|| type_err(full_key, "string|null"))?;
            let new = if s.is_empty() {
                // Empty string is a valid "clear" sentinel;
                // None means "no env var configured".
                None
            } else {
                if !is_valid_env_name(s) {
                    // We deliberately store the bad value as
                    // `Some(s)` so `validate` can surface it and
                    // the user can fix it. Returning `Err` here
                    // would trap the user in a "can't even edit"
                    // loop.
                    return Err(ConfigError::TypeMismatch {
                        key: full_key.to_string(),
                        expected: "valid POSIX identifier ([A-Za-z_][A-Za-z0-9_]*)".to_string(),
                        got: format!("string of {} characters", s.len()),
                    });
                }
                if s.len() > METADATA_AUTHOR_MAX {
                    return Err(ConfigError::TypeMismatch {
                        key: full_key.to_string(),
                        expected: format!("string of at most {} characters", METADATA_AUTHOR_MAX),
                        got: format!("string of {} characters", s.len()),
                    });
                }
                Some(s.to_string())
            };
            Ok(ProviderEndpoint {
                api_key_env: new,
                ..ep
            })
        }
        _other => Err(ConfigError::KeyNotFound {
            key: full_key.to_string(),
            available: vec!["url".into(), "model".into(), "api_key_env".into()],
        }),
    }
}

fn sampling_field(s: &SamplingConfig, field: &str) -> Result<serde_json::Value, ConfigError> {
    match field {
        "temperature" => Ok(json_of(s.temperature)),
        "num_predict" => Ok(json_of(s.num_predict)),
        "top_p" => Ok(json_of(s.top_p)),
        "top_k" => Ok(json_of(s.top_k)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("sampling.{}", other),
            available: vec!["temperature".into(), "num_predict".into(), "top_p".into(), "top_k".into()],
        }),
    }
}

fn sampling_set(record: ConfigRecord, field: &str, value: serde_json::Value, full_key: &str) -> Result<ConfigRecord, ConfigError> {
    let mut next = record;
    match field {
        "temperature" => next.sampling.temperature = as_f32(&value, full_key)?,
        "num_predict" => next.sampling.num_predict = as_usize(&value, full_key)?,
        "top_p" => next.sampling.top_p = as_f32(&value, full_key)?,
        "top_k" => next.sampling.top_k = as_usize(&value, full_key)?,
        _other => return Err(ConfigError::KeyNotFound {
            key: full_key.to_string(),
            available: vec!["temperature".into(), "num_predict".into(), "top_p".into(), "top_k".into()],
        }),
    }
    Ok(next)
}

fn runner_field(r: &RunnerConfig, field: &str) -> Result<serde_json::Value, ConfigError> {
    match field {
        "max_iterations" => Ok(json_of(r.max_iterations)),
        "max_tool_calls" => Ok(json_of(r.max_tool_calls)),
        "probe_ollama_on_run" => Ok(json_of(r.probe_ollama_on_run)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("runner.{}", other),
            available: vec!["max_iterations".into(), "max_tool_calls".into(), "probe_ollama_on_run".into()],
        }),
    }
}

fn runner_set(record: ConfigRecord, field: &str, value: serde_json::Value, full_key: &str) -> Result<ConfigRecord, ConfigError> {
    let mut next = record;
    match field {
        "max_iterations" => next.runner.max_iterations = as_usize(&value, full_key)?,
        "max_tool_calls" => next.runner.max_tool_calls = as_usize(&value, full_key)?,
        "probe_ollama_on_run" => {
            next.runner.probe_ollama_on_run = value
                .as_bool()
                .ok_or_else(|| type_err(full_key, "boolean"))?;
        }
        _other => return Err(ConfigError::KeyNotFound {
            key: full_key.to_string(),
            available: vec!["max_iterations".into(), "max_tool_calls".into(), "probe_ollama_on_run".into()],
        }),
    }
    Ok(next)
}

fn compression_field(c: &CompressionConfig, field: &str) -> Result<serde_json::Value, ConfigError> {
    match field {
        "max_messages" => Ok(json_of(c.max_messages)),
        "tool_content_max_chars" => Ok(json_of(c.tool_content_max_chars)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("compression.{}", other),
            available: vec!["max_messages".into(), "tool_content_max_chars".into()],
        }),
    }
}

fn compression_set(record: ConfigRecord, field: &str, value: serde_json::Value, full_key: &str) -> Result<ConfigRecord, ConfigError> {
    let mut next = record;
    match field {
        "max_messages" => next.compression.max_messages = as_usize(&value, full_key)?,
        "tool_content_max_chars" => next.compression.tool_content_max_chars = as_usize(&value, full_key)?,
        _other => return Err(ConfigError::KeyNotFound {
            key: full_key.to_string(),
            available: vec!["max_messages".into(), "tool_content_max_chars".into()],
        }),
    }
    Ok(next)
}

fn io_field(i: &IoConfig, field: &str) -> Result<serde_json::Value, ConfigError> {
    match field {
        "no_color" => Ok(json_of(i.no_color)),
        "quiet_default" => Ok(json_of(i.quiet_default)),
        "json_default" => Ok(json_of(i.json_default)),
        other => Err(ConfigError::KeyNotFound {
            key: format!("io.{}", other),
            available: vec!["no_color".into(), "quiet_default".into(), "json_default".into()],
        }),
    }
}

fn io_set(record: ConfigRecord, field: &str, value: serde_json::Value, full_key: &str) -> Result<ConfigRecord, ConfigError> {
    let mut next = record;
    let as_bool = |key: &str| value.as_bool().ok_or_else(|| type_err(key, "boolean"));
    match field {
        "no_color" => next.io.no_color = as_bool(full_key)?,
        "quiet_default" => next.io.quiet_default = as_bool(full_key)?,
        "json_default" => next.io.json_default = as_bool(full_key)?,
        _other => return Err(ConfigError::KeyNotFound {
            key: full_key.to_string(),
            available: vec!["no_color".into(), "quiet_default".into(), "json_default".into()],
        }),
    }
    Ok(next)
}

fn type_err(key: &str, expected: &str) -> ConfigError {
    ConfigError::TypeMismatch {
        key: key.to_string(),
        expected: expected.to_string(),
        got: "wrong type".to_string(),
    }
}

/// Strip control characters from a string before echoing it
/// inside an error message. We use this in `run_set` so that a
/// user who paste-bombed a string with `\n` doesn't get that
/// `\n` reflected back into the terminal — that would be a
/// log-injection / terminal-escape vulnerability if the error
/// ever gets logged to a file other users read.
///
/// The replacement is `<U+00XX>` so the user can still see
/// roughly where the control char was without smuggling it.
/// Strip control characters from a string for safe inclusion
/// in an error message. The control character is replaced with
/// a hex notation (`<U+000A>`) so the user can see what was
/// present without the terminal interpreting an escape.
///
/// Long inputs are also truncated with a trailing `…` so a
/// 1-MB reject message doesn't blow up the terminal. The cap
/// is generous (256 chars) — enough for a sane JSON array or
/// short string, short enough that a paste of a multi-MB file
/// won't drown the error.
fn sanitize_for_error(s: &str) -> String {
    const MAX_LEN: usize = 256;
    let mut out = String::with_capacity(s.len().min(MAX_LEN + 16));
    for c in s.chars() {
        if c.is_control() {
            out.push_str(&format!("<U+{:04X}>", c as u32));
        } else {
            out.push(c);
        }
        if out.len() >= MAX_LEN {
            out.push('…');
            return out;
        }
    }
    out
}

fn as_f32(v: &serde_json::Value, key: &str) -> Result<f32, ConfigError> {
    if let Some(f) = v.as_f64() {
        return Ok(f as f32);
    }
    Err(type_err(key, "number"))
}

fn as_usize(v: &serde_json::Value, key: &str) -> Result<usize, ConfigError> {
    if let Some(n) = v.as_u64() {
        return Ok(n as usize);
    }
    Err(type_err(key, "unsigned integer"))
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Turn a JSON value into `Option<String>` for the
/// `metadata.description` / `metadata.author` setters. Three
/// accepted inputs:
///
/// * `null` → `None` (clear the field).
/// * `""` → `None` (also a clear sentinel; matches the JSON
///   form `""`).
/// * A non-empty string → `Some(_)`.
///
/// Other JSON types (numbers, booleans, arrays, objects)
/// produce a `TypeMismatch` so the user sees a sensible error
/// rather than a panic.
fn parse_optional_string(
    value: &serde_json::Value,
    key: &str,
) -> Result<Option<String>, ConfigError> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) if s.is_empty() => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s.to_string())),
        _ => Err(type_err(key, "string, empty string, or null")),
    }
}

// ============================================================================
// Subcommand glue
// ============================================================================

/// Sub-actions of `magent config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    /// Initialise the config file at the canonical location with
    /// built-in defaults. Refuses to overwrite an existing file.
    Init,
    /// Print the resolved path (`MAGENT_CONFIG_FILE` etc).
    Where,
    /// Print the entire config record as pretty JSON.
    Show,
    /// List every flattened key + value pair.
    List,
    /// Read a single key (`config get provider.ollama.url`).
    Get(String),
    /// Set a single key (`config set provider.ollama.model llama3.2`).
    Set { key: String, value: String },
    /// Delete the config file (`config reset --yes`).
    Reset { yes: bool },
    /// `magent config validate` — re-load the file and check that
    /// every section is present, every numeric field is in range,
    /// and every URL is well-formed. Refuses non-zero exit if the
    /// file fails any rule. Useful in CI / pre-commit hooks.
    Validate,
    /// Re-print the JSON file with keys re-serialised in canonical
    /// order. Useful after hand-edits.
    Format,
}

/// Glue struct so `main.rs` can construct and run the subcommand in
/// one line, mirroring `SetPromptCmd`.
pub struct ConfigCmd<'a> {
    pub action: &'a ConfigAction,
}

impl<'a> ConfigCmd<'a> {
    pub fn new(action: &'a ConfigAction) -> Self {
        Self { action }
    }

    pub fn execute(&self, out: &mut Output) -> Result<(), ConfigError> {
        match self.action {
            ConfigAction::Init => self.run_init(out),
            ConfigAction::Where => self.run_where(out),
            ConfigAction::Show => self.run_show(out),
            ConfigAction::List => self.run_list(out),
            ConfigAction::Get(key) => self.run_get(key, out),
            ConfigAction::Set { key, value } => self.run_set(key, value, out),
            ConfigAction::Reset { yes } => self.run_reset(*yes, out),
            ConfigAction::Format => self.run_format(out),
            ConfigAction::Validate => self.run_validate(out),
        }
    }

    fn run_init(&self, out: &mut Output) -> Result<(), ConfigError> {
        let path = config_path()?;
        if Path::new(&path).exists() {
            return Err(ConfigError::Write {
                path,
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "config file already exists; refusing to overwrite (use `config reset --yes` first)",
                ),
            });
        }
        let record = ConfigRecord::with_defaults();
        let written = save(record)?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "init",
                "path": written.to_string_lossy(),
            }))?;
        } else {
            out.info(&format!("initialised {}", written.display()))?;
        }
        Ok(())
    }

    fn run_where(&self, out: &mut Output) -> Result<(), ConfigError> {
        let resolved = config_path_string();
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "path": resolved,
                "exists": Path::new(&resolved).exists(),
            }))?;
        } else {
            let _ = out.stderr_fmt_line(format_args!("{}", resolved));
        }
        Ok(())
    }

    fn run_show(&self, out: &mut Output) -> Result<(), ConfigError> {
        let record = load()?;
        let json = serde_json::to_string_pretty(&record)
            .map_err(|source| ConfigError::Json {
                path: config_path().unwrap_or_else(|_| PathBuf::from("(unresolved)")),
                source,
            })?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::to_value(&record).map_err(|source| {
                ConfigError::Json {
                    path: config_path().unwrap_or_else(|_| PathBuf::from("(unresolved)")),
                    source,
                }
            })?)?;
        } else {
            let _ = out.stderr_fmt_line(format_args!("{}", json));
        }
        Ok(())
    }

    fn run_list(&self, out: &mut Output) -> Result<(), ConfigError> {
        let record = load()?;
        let flat = flatten(&record);
        if matches!(out.kind(), OutputKind::Json) {
            // The flat list can contain multiple entries with the
            // same key (e.g. `metadata.tags[]` once per tag) — a
            // plain `Map<String, Value>` would silently drop every
            // but the last. We preserve them by promoting
            // duplicates to a JSON array.
            let mut map: serde_json::Map<String, serde_json::Value> =
                serde_json::Map::new();
            for (k, v) in flat {
                match map.entry(k) {
                    serde_json::map::Entry::Vacant(slot) => {
                        slot.insert(v);
                    }
                    serde_json::map::Entry::Occupied(mut slot) => {
                        let existing = slot.get_mut();
                        // Promote the first duplicate into an array
                        // and append the new value.
                        let arr = match existing {
                            serde_json::Value::Array(items) => {
                                items.push(v);
                                continue;
                            }
                            other => std::mem::replace(other, serde_json::Value::Array(Vec::new())),
                        };
                        if let serde_json::Value::Array(items) = slot.get_mut() {
                            items.push(arr);
                            items.push(v);
                        }
                    }
                }
            }
            out.write_json(serde_json::Value::Object(map))?;
        } else {
            let _ = out.stderr_fmt_line(format_args!("{:<40} VALUE", "KEY"));
            let _ = out.stderr_fmt_line(format_args!("{}", "-".repeat(40 + 30)));
            for (k, v) in flat {
                let s = match &v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let _ = out.stderr_fmt_line(format_args!(
                    "{:<40} {}",
                    k,
                    truncate(&s, 60)
                ));
            }
        }
        Ok(())
    }

    fn run_get(&self, key: &str, out: &mut Output) -> Result<(), ConfigError> {
        let record = load()?;
        let value = get(&record, key)?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "key": key,
                "value": value,
            }))?;
        } else {
            match &value {
                serde_json::Value::String(s) => {
                    let _ = out.stderr_fmt_line(format_args!("{}", s));
                }
                other => {
                    let pretty = serde_json::to_string_pretty(other)
                        .unwrap_or_else(|_| other.to_string());
                    let _ = out.stderr_fmt_line(format_args!("{}", pretty));
                }
            }
        }
        Ok(())
    }

    fn run_set(&self, key: &str, value: &str, out: &mut Output) -> Result<(), ConfigError> {
        let mut record = load()?;
        // Auto-detect the JSON type: if the value parses as an integer,
        // treat it as usize; if it parses as a float, treat it as f32;
        // if it parses as `true`/`false`/string, use that.
        let parsed: serde_json::Value = if let Ok(n) = value.parse::<u64>() {
            serde_json::Value::Number(n.into())
        } else if let Ok(f) = value.parse::<f64>() {
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(value.to_string()))
        } else if value == "true" {
            serde_json::Value::Bool(true)
        } else if value == "false" {
            serde_json::Value::Bool(false)
        } else if value == "null" {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(value.to_string())
        };
        record = set(record, key, parsed.clone()).map_err(|e| match e {
            // Rewrite the error so the CLI user sees the key
            // they typed (the *outer* CLI key, e.g. `author`)
            // rather than the dotted key path we got from `set`
            // (e.g. `metadata.author`). Previously we stuffed
            // the full `e.to_string()` into `expected`, which
            // produced strings like
            // `config key "metadata.author": expected config
            //  key "metadata.author": expected …`. That was
            // both confusing and the key was duplicated.
            //
            // We also sanitize `got`: the user's raw input
            // (`value`) may contain control characters that
            // would re-introduce the very injection the
            // `metadata_set` validator was trying to block.
            // Strip them so the error message is safe to print.
            ConfigError::TypeMismatch {
                expected, ..
            } => ConfigError::TypeMismatch {
                key: key.to_string(),
                expected,
                got: sanitize_for_error(value),
            },
            other => other,
        })?;
        let path = save(record.touched(now_unix_seconds()))?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "set",
                "key": key,
                "value": parsed,
                "path": path.to_string_lossy(),
            }))?;
        } else {
            out.info(&format!("{} = {}", key, value))?;
        }
        Ok(())
    }

    fn run_reset(&self, yes: bool, out: &mut Output) -> Result<(), ConfigError> {
        if !yes {
            return Err(ConfigError::Write {
                path: config_path()?,
                source: io::Error::other(
                    "refusing to reset without `--yes` (this deletes the config file)",
                ),
            });
        }
        let removed = delete()?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "reset",
                "removed": removed,
            }))?;
        } else if removed {
            out.info("config file removed")?;
        } else {
            out.info("config file did not exist")?;
        }
        Ok(())
    }

    fn run_format(&self, out: &mut Output) -> Result<(), ConfigError> {
        let record = load()?;
        let path = save(record.touched(now_unix_seconds()))?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "format",
                "path": path.to_string_lossy(),
            }))?;
        } else {
            out.info(&format!("reformatted {}", path.display()))?;
        }
        Ok(())
    }

    /// Validate every field in the config file. Returns
    /// `Ok(())` on success and `Err(ConfigError::InvalidKey)`
    /// (with the first issue as the message) on failure so the
    /// CLI exit code is non-zero and CI can pick it up. The
    /// JSON envelope always includes the *full* list of issues
    /// regardless of which one we surface via the error path.
    fn run_validate(&self, out: &mut Output) -> Result<(), ConfigError> {
        let record = load()?;
        let issues = validate_record(&record);
        if issues.is_empty() {
            if matches!(out.kind(), OutputKind::Json) {
                out.write_json(serde_json::json!({
                    "action": "validate",
                    "valid": true,
                    "issues": [],
                }))?;
            } else {
                out.info("config is valid")?;
            }
            return Ok(());
        }
        // In JSON mode we surface the *full* issue list so a
        // CI script can grep for individual keys. In Human mode
        // we print one issue per line so the user can scan it
        // visually.
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "validate",
                "valid": false,
                "issues": issues,
            }))?;
        } else {
            for issue in &issues {
                let _ = out.stderr_fmt_line(format_args!("✗ {}", issue));
            }
        }
        // Surface a structured error so the exit code is non-zero.
        Err(ConfigError::Validation {
            summary: format!(
                "{} issue(s); first: {} ({})",
                issues.len(),
                issues[0].key,
                issues[0].message
            ),
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ============================================================================
// Validation
// ============================================================================

/// One rule violation found by [`validate_record`]. We collect every
/// violation instead of failing on the first so the user sees the
/// whole picture in one pass — important when a typo corrupts
/// multiple sibling keys.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationIssue {
    /// Dotted path to the offending key (e.g. `provider.ollama.url`).
    pub key: String,
    /// Short, actionable error message.
    pub message: String,
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

/// Run every rule against `record` and return the list of
/// violations. Empty list means "valid".
///
/// The rules are intentionally cheap and deterministic — no I/O,
/// no network calls. We only check what's already inside the
/// in-memory record.
pub fn validate_record(record: &ConfigRecord) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    // --- provider ---
    let default = record.provider.default.as_str();
    if !default.is_empty() {
        match default {
            "ollama" | "deepseek" => {}
            // Whitespace-only is technically not "unknown" — it's
            // "this string is the empty-by-accident class". We
            // surface it as a distinct message so the user knows
            // it's a typo rather than a stray misspelling.
            other if other.trim().is_empty() => issues.push(ValidationIssue {
                key: "provider.default".to_string(),
                message: "default provider is whitespace-only; \
                          expected `ollama` or `deepseek`"
                    .to_string(),
            }),
            other => issues.push(ValidationIssue {
                key: "provider.default".to_string(),
                message: format!(
                    "unknown provider {:?}; expected `ollama` or `deepseek`",
                    other
                ),
            }),
        }
    }
    // Reject control characters in `provider.default` so a
    // paste accident (or a hostile config edit) can't smuggle
    // terminal escapes into the trace output.
    if record.provider.default.chars().any(|c| c.is_control()) {
        issues.push(ValidationIssue {
            key: "provider.default".to_string(),
            message: "default provider contains control characters (newlines / tabs / escapes)"
                .to_string(),
        });
    }
    // Cap the length of `provider.default`. A 1-MB string here
    // is almost certainly a paste accident and would slow down
    // every comparison (`apply_config_overrides` does one per
    // run). 256 chars is hugely generous for a provider name.
    if record.provider.default.len() > MODEL_MAX {
        issues.push(ValidationIssue {
            key: "provider.default".to_string(),
            message: format!(
                "default provider is {} characters long; expected `ollama` or `deepseek` (<12 chars)",
                record.provider.default.len()
            ),
        });
    }
    // URLs — when non-empty, must look like `scheme://...`.
    for (name, endpoint) in [
        ("provider.ollama.url", &record.provider.ollama.url),
        ("provider.deepseek.url", &record.provider.deepseek.url),
    ] {
        if !endpoint.is_empty() {
            if !looks_like_url(endpoint) {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!(
                        "URL {:?} is missing a scheme (http:// or https://)",
                        endpoint
                    ),
                });
            }
            // Refuse control characters — a stray newline
            // would let an attacker split the URL into two
            // log lines and (depending on the consumer) forge
            // pivots to other URLs.
            if endpoint.chars().any(|c| c.is_control()) {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: "URL contains control characters".to_string(),
                });
            }
            // Sanity cap: any URL longer than 2 KB is almost
            // certainly a paste accident or a hostile config.
            // We refuse to load silently — surface it so the
            // user can fix or remove the line.
            if endpoint.len() > URL_MAX {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!(
                        "URL is {} characters long; consider <{}",
                        endpoint.len(),
                        URL_MAX
                    ),
                });
            }
        }
    }
    // Models — when non-empty, must not contain whitespace or
    // control characters, and must not be absurdly long.
    for (name, model) in [
        ("provider.ollama.model", &record.provider.ollama.model),
        ("provider.deepseek.model", &record.provider.deepseek.model),
    ] {
        if !model.is_empty() {
            if model.chars().any(char::is_whitespace) {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!("model name {:?} contains whitespace", model),
                });
            }
            if model.chars().any(|c| c.is_control()) {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: "model name contains control characters".to_string(),
                });
            }
            if model.len() > MODEL_MAX {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!(
                        "model name is {} characters long; consider <{}",
                        model.len(),
                        MODEL_MAX
                    ),
                });
            }
        }
    }
    // API-key env names — when non-empty, must look like a POSIX
    // env var (`[A-Za-z_][A-Za-z0-9_]*`). Anything else silently
    // breaks the key lookup, so we surface it here.
    for (name, env) in [
        (
            "provider.ollama.api_key_env",
            &record.provider.ollama.api_key_env,
        ),
        (
            "provider.deepseek.api_key_env",
            &record.provider.deepseek.api_key_env,
        ),
    ] {
    if let Some(env_name) = env {
        if !env_name.is_empty() {
            if !is_valid_env_name(env_name) {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!(
                        "env var name {:?} is not a valid POSIX identifier (must match [A-Za-z_][A-Za-z0-9_]*)",
                        env_name
                    ),
                });
            }
            // Cap the length. Real env var names are
            // tens of characters; a 256-char "name" is
            // almost certainly a paste accident and would
            // make every env lookup measurably slow.
            if env_name.len() > METADATA_AUTHOR_MAX {
                issues.push(ValidationIssue {
                    key: name.to_string(),
                    message: format!(
                        "env var name is {} characters long; expected <{}",
                        env_name.len(),
                        METADATA_AUTHOR_MAX
                    ),
                });
            }
        }
    }
    }

    // --- sampling ---
    if !TEMPERATURE_RANGE.contains(&record.sampling.temperature) {
        issues.push(ValidationIssue {
            key: "sampling.temperature".to_string(),
            message: format!(
                "temperature {} is outside the conventional 0.0–2.0 range",
                record.sampling.temperature
            ),
        });
    }
    if !TOP_P_RANGE.contains(&record.sampling.top_p) {
        issues.push(ValidationIssue {
            key: "sampling.top_p".to_string(),
            message: format!(
                "top_p {} is outside the conventional 0.0–1.0 range",
                record.sampling.top_p
            ),
        });
    }
    if record.sampling.top_k > TOP_K_MAX {
        issues.push(ValidationIssue {
            key: "sampling.top_k".to_string(),
            message: format!(
                "top_k {} is unreasonably large; the largest real-world value is <100",
                record.sampling.top_k
            ),
        });
    }
    if record.sampling.num_predict > NUM_PREDICT_MAX {
        issues.push(ValidationIssue {
            key: "sampling.num_predict".to_string(),
            message: format!(
                "num_predict {} is unreasonably large; consider <100000",
                record.sampling.num_predict
            ),
        });
    }

    // --- runner ---
    if record.runner.max_iterations > MAX_ITERATIONS_MAX {
        issues.push(ValidationIssue {
            key: "runner.max_iterations".to_string(),
            message: format!(
                "max_iterations {} is unreasonably large; consider <100",
                record.runner.max_iterations
            ),
        });
    }
    if record.runner.max_tool_calls > MAX_TOOL_CALLS_MAX {
        issues.push(ValidationIssue {
            key: "runner.max_tool_calls".to_string(),
            message: format!(
                "max_tool_calls {} is unreasonably large; consider <100",
                record.runner.max_tool_calls
            ),
        });
    }

    // --- compression ---
    if record.compression.tool_content_max_chars > TOOL_CONTENT_MAX_CHARS_MAX {
        issues.push(ValidationIssue {
            key: "compression.tool_content_max_chars".to_string(),
            message: format!(
                "tool_content_max_chars {} is unreasonably large; consider <10000",
                record.compression.tool_content_max_chars
            ),
        });
    }

    // --- metadata ---
    if let Some(desc) = &record.metadata.description {
        if desc.len() > METADATA_DESCRIPTION_MAX {
            issues.push(ValidationIssue {
                key: "metadata.description".to_string(),
                message: format!(
                    "description is {} characters long; consider <{}",
                    desc.len(),
                    METADATA_DESCRIPTION_MAX
                ),
            });
        }
        // Descriptions are shown verbatim in `magent config show`;
        // a control character is almost certainly a paste
        // accident or a hostile config edit. We refuse so a
        // user fixing the field knows about it instead of
        // silently seeing escape sequences in their terminal.
        if desc.chars().any(|c| c.is_control()) {
            issues.push(ValidationIssue {
                key: "metadata.description".to_string(),
                message: "description contains control characters (newlines / tabs / escapes)"
                    .to_string(),
            });
        }
    }
    if let Some(author) = &record.metadata.author {
        if author.len() > METADATA_AUTHOR_MAX {
            issues.push(ValidationIssue {
                key: "metadata.author".to_string(),
                message: format!(
                    "author is {} characters long; consider <{}",
                    author.len(),
                    METADATA_AUTHOR_MAX
                ),
            });
        }
        // Authors are typically names or emails; reject control
        // characters and newlines so a malicious config can't
        // smuggle log-injection or terminal-escape sequences
        // into the trace output.
        if author.chars().any(|c| c.is_control()) {
            issues.push(ValidationIssue {
                key: "metadata.author".to_string(),
                message: "author contains control characters (newlines / tabs / escapes)"
                    .to_string(),
            });
        }
    }
    // Per-tag length check: a single tag of >64 chars is almost
    // certainly a typo or pasted boilerplate. We don't enforce a
    // character class — tags can be arbitrary strings — but we
    // cap the size.
    for (i, tag) in record.metadata.tags.iter().enumerate() {
        if tag.len() > METADATA_TAG_MAX {
            issues.push(ValidationIssue {
                key: format!("metadata.tags[{}]", i),
                message: format!(
                    "tag {:?} is {} characters long; consider <{}",
                    tag,
                    tag.len(),
                    METADATA_TAG_MAX
                ),
            });
        }
    }
    if record.metadata.tags.len() > METADATA_TAGS_MAX {
        issues.push(ValidationIssue {
            key: "metadata.tags".to_string(),
            message: format!(
                "{} tags is unreasonably many; consider <{}",
                record.metadata.tags.len(),
                METADATA_TAGS_MAX
            ),
        });
    }
    // Each tag should be non-empty and free of leading/trailing
    // whitespace; users occasionally paste a stray newline.
    for (i, tag) in record.metadata.tags.iter().enumerate() {
        if tag.is_empty() {
            issues.push(ValidationIssue {
                key: format!("metadata.tags[{}]", i),
                message: "tag is empty".to_string(),
            });
        } else if tag != tag.trim() {
            issues.push(ValidationIssue {
                key: format!("metadata.tags[{}]", i),
                message: format!(
                    "tag {:?} has leading or trailing whitespace",
                    tag
                ),
            });
        }
    }

    issues
}

fn looks_like_url(s: &str) -> bool {
    // Cheap check: a scheme, then `://`, then *something*. We don't
    // try to validate the rest because the URL might legitimately
    // contain query strings, percent escapes, or even Unicode.
    let scheme_end = s.find("://");
    match scheme_end {
        None | Some(0) => false,
        Some(i) => {
            let scheme = &s[..i];
            scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
                && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        }
    }
}

/// POSIX-style env var name check: `[A-Za-z_][A-Za-z0-9_]*`. We
/// reject anything else because `std::env::var` won't find it (the
/// OS won't store such names) and a malformed `api_key_env` value
/// would just silently degrade security.
pub(crate) fn is_valid_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TempConfig(PathBuf);
    impl TempConfig {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "magent_config_{}_{}_{}.json",
                label,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            // SAFETY: see TempPromptsDir in prompt.rs.
            unsafe {
                std::env::set_var(CONFIG_FILE_ENV, &path);
            }
            Self(path)
        }
    }
    impl Drop for TempConfig {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var(CONFIG_FILE_ENV);
            }
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn schema_version_constant_is_one() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn validation_constants_are_sane() {
        // The thresholds are advisory, but they should at least
        // pass the defaults produced by `with_defaults()`.
        let r = ConfigRecord::with_defaults();
        assert!(TEMPERATURE_RANGE.contains(&r.sampling.temperature));
        assert!(TOP_P_RANGE.contains(&r.sampling.top_p));
        assert!(r.sampling.top_k <= TOP_K_MAX);
        assert!(r.runner.max_iterations <= MAX_ITERATIONS_MAX);
        assert!(r.runner.max_tool_calls <= MAX_TOOL_CALLS_MAX);
        assert!(r.compression.tool_content_max_chars <= TOOL_CONTENT_MAX_CHARS_MAX);
        assert!(r.metadata.tags.len() <= METADATA_TAGS_MAX);
    }

    #[test]
    fn defaults_are_complete() {
        let r = ConfigRecord::with_defaults();
        assert_eq!(r.provider.default, "ollama");
        assert_eq!(r.provider.ollama.url, "http://localhost:11434");
        assert_eq!(r.provider.ollama.model, "llama3.2");
        assert_eq!(r.provider.deepseek.url, "https://api.deepseek.com/v1");
        assert_eq!(r.sampling.temperature, 0.3);
        assert_eq!(r.compression.max_messages, 32);
    }

    #[test]
    fn partial_file_round_trips() {
        // User-written files that omit several sections should
        // round-trip; missing sections get default values at use time.
        let json = r#"{
            "schema_version": 1,
            "provider": { "default": "deepseek" },
            "created_at": 0,
            "updated_at": 0
        }"#;
        let record: ConfigRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.provider.default, "deepseek");
        // Defaults still apply for the omitted sections.
        assert_eq!(record.sampling.temperature, 0.3);
    }

    #[test]
    fn get_and_set_simple_key() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("get_set");
        let r = ConfigRecord::with_defaults();
        let r = set(r, "provider.default", serde_json::json!("deepseek")).unwrap();
        assert_eq!(
            get(&r, "provider.default").unwrap(),
            serde_json::json!("deepseek")
        );
    }

    #[test]
    fn get_nested_endpoint_field() {
        let r = ConfigRecord::with_defaults();
        assert_eq!(
            get(&r, "provider.ollama.model").unwrap(),
            serde_json::json!("llama3.2")
        );
        assert_eq!(
            get(&r, "provider.deepseek.url").unwrap(),
            serde_json::json!("https://api.deepseek.com/v1")
        );
    }

    #[test]
    fn get_unknown_key_lists_available() {
        let r = ConfigRecord::with_defaults();
        let err = get(&r, "provider.bogus").unwrap_err();
        match err {
            ConfigError::KeyNotFound { key, available } => {
                assert!(key.contains("bogus"));
                assert!(available.contains(&"ollama".to_string()));
            }
            other => panic!("expected KeyNotFound, got {:?}", other),
        }
    }

    #[test]
    fn get_metadata_tags_returns_array() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["a".to_string(), "b".to_string()];
        let v = get(&r, "metadata.tags").unwrap();
        assert_eq!(v, serde_json::json!(["a", "b"]));
    }

    #[test]
    fn get_metadata_tags_by_index() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(get(&r, "metadata.tags.0").unwrap(), serde_json::json!("alpha"));
        assert_eq!(get(&r, "metadata.tags.1").unwrap(), serde_json::json!("beta"));
    }

    #[test]
    fn get_metadata_tags_out_of_range_is_null() {
        // JS-style: indexes past the end return `null`, not an
        // error. This lets scripts do `magent config get
        // metadata.tags.5` defensively without trapping.
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["only".to_string()];
        assert_eq!(get(&r, "metadata.tags.99").unwrap(), serde_json::Value::Null);
    }

    #[test]
    fn get_metadata_tags_non_numeric_index_errors() {
        let r = ConfigRecord::with_defaults();
        let err = get(&r, "metadata.tags.foo").unwrap_err();
        assert!(matches!(err, ConfigError::InvalidKey(_)), "{:?}", err);
    }

    #[test]
    fn get_metadata_author_returns_string_or_null() {
        let mut r = ConfigRecord::with_defaults();
        // Both states: Some and None.
        let v1 = get(&r, "metadata.author").unwrap();
        assert_eq!(v1, serde_json::Value::Null);
        r.metadata.author = Some("arksong".to_string());
        let v2 = get(&r, "metadata.author").unwrap();
        assert_eq!(v2, serde_json::json!("arksong"));
    }

    #[test]
    fn get_metadata_description_returns_string_or_null() {
        // Same shape as the author test — guards against a
        // regression where `Some` gets serialized as a JSON
        // object instead of a string.
        let mut r = ConfigRecord::with_defaults();
        let v1 = get(&r, "metadata.description").unwrap();
        assert_eq!(v1, serde_json::Value::Null);
        r.metadata.description = Some("hello".to_string());
        let v2 = get(&r, "metadata.description").unwrap();
        assert_eq!(v2, serde_json::json!("hello"));
    }

    #[test]
    fn set_rejects_wrong_type() {
        let r = ConfigRecord::with_defaults();
        let err = set(r, "sampling.temperature", serde_json::json!("hot")).unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }));
    }

    #[test]
    fn set_rejects_unknown_key() {
        let r = ConfigRecord::with_defaults();
        let err = set(r, "provider.bogus.model", serde_json::json!("x")).unwrap_err();
        assert!(matches!(err, ConfigError::KeyNotFound { .. }));
    }

    #[test]
    fn set_api_key_env_empty_clears() {
        let r = ConfigRecord::with_defaults();
        assert_eq!(r.provider.ollama.api_key_env.as_deref(), Some("OLLAMA_API_KEY"));
        let updated = set(r, "provider.ollama.api_key_env", serde_json::json!("")).unwrap();
        assert_eq!(updated.provider.ollama.api_key_env, None);
    }

    #[test]
    fn set_api_key_env_rejects_invalid_name() {
        // A typo or a hostile config can't sneak a malformed
        // env var name through `set`. The runner would later
        // silently miss the key, so we refuse loudly at write
        // time.
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "provider.ollama.api_key_env",
            serde_json::json!("bad name with spaces"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_api_key_env_rejects_oversized() {
        let r = ConfigRecord::with_defaults();
        let big = "X".repeat(METADATA_AUTHOR_MAX + 1);
        let err = set(
            r,
            "provider.ollama.api_key_env",
            serde_json::json!(big),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_provider_default_rejects_oversized() {
        let r = ConfigRecord::with_defaults();
        let big = "x".repeat(MODEL_MAX + 1);
        let err = set(r, "provider.default", serde_json::json!(big)).unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn validate_rejects_oversized_api_key_env() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.api_key_env = Some("X".repeat(METADATA_AUTHOR_MAX + 1));
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.ollama.api_key_env"),
            "{:?}",
            issues
        );
    }

    #[test]
    fn save_writes_owner_only_permissions_on_unix() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempConfig::new("perm");
        let r = ConfigRecord::with_defaults();
        let path = save(r).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "config file must be owner-only (0600), got {:#o}",
                mode & 0o777
            );
        }
        let _ = tmp;
    }

    #[test]
    fn validate_rejects_oversized_provider_default() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.default = "x".repeat(MODEL_MAX + 1);
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.default"),
            "{:?}",
            issues
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        // `unwrap_or_else(|e| e.into_inner())` recovers from a
        // poisoned mutex — a previous test that panicked while
        // holding the lock shouldn't cascade into all later tests.
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("save_load");
        let r = ConfigRecord::with_defaults();
        let r = set(r, "sampling.temperature", serde_json::json!(0.7)).unwrap();
        save(r).unwrap();
        let loaded = load().unwrap();
        assert_eq!(loaded.sampling.temperature, 0.7);
    }

    #[test]
    fn save_preserves_created_at() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("preserve");
        let r1 = ConfigRecord::with_defaults();
        save(r1).unwrap();
        // Unix-second resolution means we have to sleep >1s to
        // guarantee `updated_at` advances. We pick 1.2s to leave
        // headroom on slow CI runners.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let r2 = load().unwrap();
        let created_at_1 = r2.created_at;
        save(r2.touched(now_unix_seconds() + 1000)).unwrap();
        let r3 = load().unwrap();
        assert_eq!(r3.created_at, created_at_1, "created_at must survive update");
        assert!(r3.updated_at > created_at_1);
    }

    #[test]
    fn delete_returns_true_when_present() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("delete_present");
        save(ConfigRecord::with_defaults()).unwrap();
        assert!(delete().unwrap());
    }

    #[test]
    fn delete_returns_false_when_missing() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("delete_missing");
        assert!(!delete().unwrap());
    }

    #[test]
    fn flatten_includes_every_field() {
        let r = ConfigRecord::with_defaults();
        let flat = flatten(&r);
        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"provider.ollama.url"));
        assert!(keys.contains(&"provider.deepseek.model"));
        assert!(keys.contains(&"sampling.temperature"));
        assert!(keys.contains(&"runner.max_iterations"));
        assert!(keys.contains(&"compression.max_messages"));
        assert!(keys.contains(&"io.no_color"));
    }

    #[test]
    fn flatten_emits_one_entry_per_tag() {
        // `metadata.tags[]` should appear once per tag. The list
        // output uses the same key for every entry, so JSON-mode
        // callers must promote duplicates to an array (see
        // `run_list`).
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let flat = flatten(&r);
        let tag_count = flat
            .iter()
            .filter(|(k, _)| k == "metadata.tags[]")
            .count();
        assert_eq!(tag_count, 3);
    }

    #[test]
    fn unsupported_schema_rejected() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempConfig::new("unsupported");
        fs::write(
            &tmp.0,
            r#"{"schema_version": 9999, "created_at": 0, "updated_at": 0}"#,
        )
        .unwrap();
        match load() {
            Err(ConfigError::UnsupportedSchema { found, supported, .. }) => {
                assert_eq!(found, 9999);
                assert_eq!(supported, CURRENT_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchema, got {:?}", other),
        }
    }

    #[test]
    fn init_refuses_to_overwrite() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("init_overwrite");
        // First init should succeed.
        ConfigCmd::new(&ConfigAction::Init).execute(&mut Output::new(OutputKind::Json, true)).unwrap();
        // Second init should fail.
        let err = ConfigCmd::new(&ConfigAction::Init)
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap_err();
        assert!(matches!(err, ConfigError::Write { .. }));
    }

    #[test]
    fn reset_requires_yes() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("reset_yes");
        save(ConfigRecord::with_defaults()).unwrap();
        let err = ConfigCmd::new(&ConfigAction::Reset { yes: false })
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap_err();
        assert!(matches!(err, ConfigError::Write { .. }));
        ConfigCmd::new(&ConfigAction::Reset { yes: true })
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap();
    }

    #[test]
    fn set_rejects_empty_key() {
        let r = ConfigRecord::with_defaults();
        let err = set(r, "", serde_json::json!("x")).unwrap_err();
        assert!(matches!(err, ConfigError::KeyNotFound { .. } | ConfigError::InvalidKey(_)));
    }

    // ------------------------------------------------------------------
    // `validate_record` — schema validation rules
    // ------------------------------------------------------------------

    #[test]
    fn validate_accepts_defaults() {
        let r = ConfigRecord::with_defaults();
        let issues = validate_record(&r);
        assert!(
            issues.is_empty(),
            "defaults should be valid; got {:?}",
            issues
        );
    }

    #[test]
    fn validate_rejects_unknown_provider() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.default = "gpt-9000".to_string();
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.default"));
    }

    #[test]
    fn validate_rejects_url_without_scheme() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.url = "localhost:11434".to_string();
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.ollama.url"));
    }

    #[test]
    fn validate_accepts_https_url() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.deepseek.url = "https://api.deepseek.com/v1".to_string();
        let issues = validate_record(&r);
        assert!(issues.is_empty(), "{:?}", issues);
    }

    #[test]
    fn validate_rejects_temperature_out_of_range() {
        let mut r = ConfigRecord::with_defaults();
        r.sampling.temperature = 5.0;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "sampling.temperature"));
    }

    #[test]
    fn validate_rejects_top_p_out_of_range() {
        let mut r = ConfigRecord::with_defaults();
        r.sampling.top_p = 1.5;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "sampling.top_p"));
    }

    #[test]
    fn validate_rejects_unreasonable_top_k() {
        let mut r = ConfigRecord::with_defaults();
        r.sampling.top_k = 10_000_000;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "sampling.top_k"));
    }

    #[test]
    fn validate_rejects_unreasonable_iterations() {
        let mut r = ConfigRecord::with_defaults();
        r.runner.max_iterations = 999_999;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "runner.max_iterations"));
    }

    #[test]
    fn validate_rejects_whitespace_in_model_name() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.model = "bad model name".to_string();
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.ollama.model"));
    }

    #[test]
    fn validate_collects_multiple_issues() {
        // Three independent violations; we expect *all three* to be
        // returned, not just the first one.
        let mut r = ConfigRecord::with_defaults();
        r.provider.default = "bogus".to_string();
        r.sampling.temperature = 9.9;
        r.runner.max_iterations = 999_999;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.default"));
        assert!(issues.iter().any(|i| i.key == "sampling.temperature"));
        assert!(issues.iter().any(|i| i.key == "runner.max_iterations"));
    }

    #[test]
    fn set_provider_default_rejects_whitespace() {
        // A config that says `provider.default = "   "` would
        // silently fall through to ollama at run time. We refuse
        // it at write time so the user fixes the typo.
        let r = ConfigRecord::with_defaults();
        let err = set(r, "provider.default", serde_json::json!("   ")).unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_provider_default_rejects_empty() {
        // Empty at write time — note this is distinct from the
        // "missing" case (None) which is the default. An empty
        // string is a deliberate "clear" and should be allowed.
        let r = ConfigRecord::with_defaults();
        let updated = set(r, "provider.default", serde_json::json!("")).unwrap();
        assert_eq!(updated.provider.default, "");
    }

    #[test]
    fn set_provider_default_rejects_control_chars() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "provider.default",
            serde_json::json!("ollama\n"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn validate_provider_default_whitespace_only() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.default = "   ".to_string();
        let issues = validate_record(&r);
        // Both the "whitespace" rule and the "control char" rule
        // for clarity — but at minimum the whitespace one.
        assert!(issues.iter().any(|i| i.key == "provider.default"
            && i.message.contains("whitespace")),
            "{:?}", issues);
    }

    #[test]
    fn validate_provider_default_control_chars() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.default = "ollama\n".to_string();
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.default"
            && i.message.contains("control characters")),
            "{:?}", issues);
    }

    #[test]
    fn validate_run_succeeds_on_clean_config() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("validate_ok");
        // No file yet → load() returns defaults → defaults are valid.
        let result = ConfigCmd::new(&ConfigAction::Validate)
            .execute(&mut Output::new(OutputKind::Json, true));
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn validate_run_fails_on_broken_config() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempConfig::new("validate_bad");
        // Write a config with an out-of-range temperature directly
        // to disk (bypassing `set` which would have validated).
        let mut r = ConfigRecord::with_defaults();
        r.sampling.temperature = 7.7;
        save(r).unwrap();
        let result = ConfigCmd::new(&ConfigAction::Validate)
            .execute(&mut Output::new(OutputKind::Human, true));
        assert!(result.is_err());
    }

    // -- Audit additions: env-name validation, num_predict, tags --

    #[test]
    fn validate_rejects_invalid_env_name() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.deepseek.api_key_env = Some("bad name with spaces".to_string());
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "provider.deepseek.api_key_env"));
    }

    #[test]
    fn validate_accepts_valid_env_name() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.api_key_env = Some("MY_OLLAMA_TOKEN".to_string());
        let issues = validate_record(&r);
        assert!(issues.is_empty(), "{:?}", issues);
    }

    #[test]
    fn validate_rejects_unreasonable_num_predict() {
        let mut r = ConfigRecord::with_defaults();
        r.sampling.num_predict = 5_000_000;
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "sampling.num_predict"));
    }

    #[test]
    fn validate_rejects_empty_tag() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["good".to_string(), String::new()];
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "metadata.tags[1]"));
    }

    #[test]
    fn validate_rejects_tag_with_whitespace() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["  leading".to_string(), "trailing  ".to_string()];
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "metadata.tags[0]"));
        assert!(issues.iter().any(|i| i.key == "metadata.tags[1]"));
    }

    #[test]
    fn is_valid_env_name_accepts_canonical() {
        assert!(is_valid_env_name("FOO"));
        assert!(is_valid_env_name("_FOO"));
        assert!(is_valid_env_name("FOO_BAR_42"));
        assert!(is_valid_env_name("a"));
    }

    #[test]
    fn is_valid_env_name_rejects_malformed() {
        assert!(!is_valid_env_name("1FOO"));        // can't start with digit
        assert!(!is_valid_env_name("FOO BAR"));    // space
        assert!(!is_valid_env_name("FOO-BAR"));    // hyphen
        assert!(!is_valid_env_name(""));           // empty
        assert!(!is_valid_env_name("FOO$BAR"));    // special char
        assert!(!is_valid_env_name("FOO=BAR"));    // equals
        assert!(!is_valid_env_name("../etc"));     // path traversal
    }

    // -- Audit additions: author, oversized tags, control chars --

    #[test]
    fn validate_accepts_short_author() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.author = Some("arksong".to_string());
        let issues = validate_record(&r);
        assert!(issues.is_empty(), "{:?}", issues);
    }

    #[test]
    fn validate_rejects_oversized_author() {
        let mut r = ConfigRecord::with_defaults();
        // One character over the cap.
        r.metadata.author = Some("a".repeat(METADATA_AUTHOR_MAX + 1));
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "metadata.author"));
    }

    #[test]
    fn validate_rejects_author_with_control_chars() {
        // A newline or escape sequence in `author` could be used
        // to break out of a structured log line. The validator
        // should refuse anything below 0x20 except plain space.
        for bad in ["alice\nbob", "alice\tbob", "alice\u{1b}[31mRED"] {
            let mut r = ConfigRecord::with_defaults();
            r.metadata.author = Some(bad.to_string());
            let issues = validate_record(&r);
            assert!(
                issues.iter().any(|i| i.key == "metadata.author"),
                "expected an author issue for {:?}; got {:?}",
                bad, issues
            );
        }
    }

    #[test]
    fn validate_rejects_oversized_description() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.description = Some("a".repeat(METADATA_DESCRIPTION_MAX + 1));
        let issues = validate_record(&r);
        assert!(issues.iter().any(|i| i.key == "metadata.description"));
    }

    #[test]
    fn validate_rejects_description_with_control_chars() {
        for bad in ["line1\nline2", "tab\there", "ansi\u{1b}[31mRED"] {
            let mut r = ConfigRecord::with_defaults();
            r.metadata.description = Some(bad.to_string());
            let issues = validate_record(&r);
            assert!(
                issues.iter().any(|i| i.key == "metadata.description"),
                "expected a description issue for {:?}; got {:?}",
                bad, issues
            );
        }
    }

    #[test]
    fn set_description_rejects_control_chars_at_write_time() {
        // Same protection as author: refuse at write time so a
        // paste accident can't sneak into the file in the first
        // place.
        let r = ConfigRecord::with_defaults();
        let err = set(r, "metadata.description", serde_json::json!("line1\nline2")).unwrap_err();
        assert!(
            matches!(err, ConfigError::TypeMismatch { .. }),
            "{:?}",
            err
        );
    }

    #[test]
    fn set_description_null_clears() {
        // `null` is accepted as a "clear" sentinel, mirroring the
        // empty-string form. This lets scripted callers use
        // `magent config set metadata.description null` without
        // tripping the type guard.
        let mut r = ConfigRecord::with_defaults();
        r.metadata.description = Some("hello".to_string());
        let updated = set(r, "metadata.description", serde_json::json!(null)).unwrap();
        assert_eq!(updated.metadata.description, None);
    }

    #[test]
    fn set_author_null_clears() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.author = Some("alice".to_string());
        let updated = set(r, "metadata.author", serde_json::json!(null)).unwrap();
        assert_eq!(updated.metadata.author, None);
    }

    #[test]
    fn set_metadata_description_rejects_number() {
        // Numbers, booleans, arrays, objects should all fail
        // with TypeMismatch — not silently coerce or panic.
        let r = ConfigRecord::with_defaults();
        let err = set(r, "metadata.description", serde_json::json!(42)).unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_metadata_tags_null_clears() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["a".to_string(), "b".to_string()];
        let updated = set(r, "metadata.tags", serde_json::json!(null)).unwrap();
        assert!(updated.metadata.tags.is_empty());
    }

    #[test]
    fn set_metadata_tags_array_with_non_string_errors() {
        // A `["a", 1, "b"]` array should fail on index 1 with a
        // useful message that includes the index.
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "metadata.tags",
            serde_json::json!(["a", 1, "b"]),
        )
        .unwrap_err();
        match err {
            ConfigError::TypeMismatch { key, .. } => {
                assert!(
                    key.contains("metadata.tags[1]"),
                    "expected index error; got {:?}",
                    key
                );
            }
            other => panic!("expected TypeMismatch, got {:?}", other),
        }
    }

    #[test]
    fn set_metadata_tags_rejects_too_many() {
        // The cap is enforced at write time, not just at
        // validate-record. A user writing 100 tags directly
        // would otherwise get a stale config that only fails
        // when `magent config validate` runs.
        let r = ConfigRecord::with_defaults();
        let big: Vec<String> = (0..METADATA_TAGS_MAX + 1).map(|i| format!("t{}", i)).collect();
        let err = set(r, "metadata.tags", serde_json::json!(big)).unwrap_err();
        match err {
            ConfigError::TypeMismatch { key, expected, .. } => {
                assert_eq!(key, "metadata.tags");
                assert!(expected.contains("at most"), "got {:?}", expected);
            }
            other => panic!("expected TypeMismatch, got {:?}", other),
        }
    }

    #[test]
    fn set_metadata_tags_accepts_exactly_max() {
        // Boundary: METADATA_TAGS_MAX should be the inclusive
        // upper bound. One over is rejected; exactly N is fine.
        let r = ConfigRecord::with_defaults();
        let exact: Vec<String> = (0..METADATA_TAGS_MAX).map(|i| format!("t{}", i)).collect();
        let updated = set(r, "metadata.tags", serde_json::json!(exact)).unwrap();
        assert_eq!(updated.metadata.tags.len(), METADATA_TAGS_MAX);
    }

    #[test]
    fn validate_rejects_oversized_tag() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["a".repeat(METADATA_TAG_MAX + 1)];
        let issues = validate_record(&r);
        // The issue key includes the offending index.
        assert!(
            issues.iter().any(|i| i.key == "metadata.tags[0]"),
            "{:?}",
            issues
        );
    }

    #[test]
    fn validate_rejects_oversized_url() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.url = "http://a".repeat(URL_MAX / 6 + 1);
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.ollama.url"),
            "{:?}",
            issues
        );
    }

    #[test]
    fn validate_rejects_url_with_control_chars() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.deepseek.url = "https://api.deepseek.com\n/v1".to_string();
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.deepseek.url"
                && i.message.contains("control characters")),
            "{:?}",
            issues
        );
    }

    #[test]
    fn validate_rejects_model_with_control_chars() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.ollama.model = "llama3.2\n".to_string();
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.ollama.model"
                && i.message.contains("control characters")),
            "{:?}",
            issues
        );
    }

    #[test]
    fn validate_rejects_oversized_model() {
        let mut r = ConfigRecord::with_defaults();
        r.provider.deepseek.model = "m".repeat(MODEL_MAX + 1);
        let issues = validate_record(&r);
        assert!(
            issues.iter().any(|i| i.key == "provider.deepseek.model"),
            "{:?}",
            issues
        );
    }

    #[test]
    fn set_url_rejects_oversized() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "provider.ollama.url",
            serde_json::json!("http://a".repeat(URL_MAX / 6 + 1)),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_url_rejects_control_chars() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "provider.ollama.url",
            serde_json::json!("http://localhost:11434\n"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_model_rejects_whitespace() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "provider.ollama.model",
            serde_json::json!("llama 3.2"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_metadata_tags_empty_string_clears() {
        // `magent config set metadata.tags ""` should be a no-op
        // clear that matches the `null` / `[]` pathways.
        let mut r = ConfigRecord::with_defaults();
        r.metadata.tags = vec!["hello".to_string()];
        let updated = set(r, "metadata.tags", serde_json::json!("")).unwrap();
        assert!(updated.metadata.tags.is_empty(), "{:?}", updated.metadata.tags);
    }

    #[test]
    fn set_author_round_trip() {
        let r = ConfigRecord::with_defaults();
        let updated =
            set(r, "metadata.author", serde_json::json!("alice@example.com")).unwrap();
        assert_eq!(
            updated.metadata.author.as_deref(),
            Some("alice@example.com")
        );
    }

    #[test]
    fn set_description_rejects_oversized() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "metadata.description",
            serde_json::json!("a".repeat(METADATA_DESCRIPTION_MAX + 1)),
        )
        .unwrap_err();
        match err {
            ConfigError::TypeMismatch { expected, got, .. } => {
                assert!(
                    expected.contains("characters"),
                    "expected an error mentioning the length cap; got {:?}",
                    expected
                );
                assert!(got.contains("characters"), "got {:?}, expected a length", got);
            }
            other => panic!("expected TypeMismatch, got {:?}", other),
        }
    }

    #[test]
    fn set_description_empty_clears() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.description = Some("hello".to_string());
        let updated = set(r, "metadata.description", serde_json::json!("")).unwrap();
        assert_eq!(updated.metadata.description, None);
    }

    #[test]
    fn sanitize_for_error_replaces_control_chars() {
        let s = sanitize_for_error("alice\nbob\tcat\x1b[31mRED");
        // Newline, tab, and ESC all gone; replaced with hex.
        assert!(!s.contains('\n'));
        assert!(!s.contains('\t'));
        assert!(!s.contains('\x1b'));
        assert!(s.contains("<U+000A>"), "{:?}", s);
        assert!(s.contains("<U+0009>"), "{:?}", s);
        assert!(s.contains("<U+001B>"), "{:?}", s);
        // Plain text preserved.
        assert!(s.contains("alice"));
        assert!(s.contains("RED"));
    }

    #[test]
    fn sanitize_for_error_truncates_long_input() {
        // 1-MB input would otherwise produce a 1-MB error message.
        // The helper caps the visible length and adds a trailing
        // ellipsis so the user can tell it was truncated.
        let big = "x".repeat(1_000_000);
        let s = sanitize_for_error(&big);
        assert!(
            s.ends_with('…'),
            "expected truncation marker, got tail {:?}",
            &s[s.len() - 5..]
        );
        assert!(s.len() < 300, "sanitized output is too long: {}", s.len());
    }

    #[test]
    fn set_author_control_char_error_sanitizes_got() {
        // The error message's `got` field must not echo raw
        // control characters back to the user — that's a
        // log-injection / terminal-escape vulnerability.
        let r = ConfigRecord::with_defaults();
        let r_with = r.clone();
        let err = set(r, "metadata.author", serde_json::json!("alice\nbob")).unwrap_err();
        match err {
            ConfigError::TypeMismatch { got, .. } => {
                assert!(!got.contains('\n'), "got contains raw newline: {:?}", got);
                assert!(!got.contains('\r'), "got contains raw CR: {:?}", got);
                assert!(!got.contains('\x1b'), "got contains ESC: {:?}", got);
            }
            other => panic!("expected TypeMismatch, got {:?}", other),
        }
        // Sanity check: the helper exists / r is still valid.
        let _ = r_with;
    }

    #[test]
    fn set_author_empty_clears() {
        let mut r = ConfigRecord::with_defaults();
        r.metadata.author = Some("alice".to_string());
        let updated = set(r, "metadata.author", serde_json::json!("")).unwrap();
        assert_eq!(updated.metadata.author, None);
    }

    #[test]
    fn set_author_rejects_control_chars_at_write_time() {
        let r = ConfigRecord::with_defaults();
        let err = set(r, "metadata.author", serde_json::json!("alice\nbob")).unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_tags_accepts_csv_string() {
        let r = ConfigRecord::with_defaults();
        let updated = set(r, "metadata.tags", serde_json::json!("nrf52,wearable"))
            .unwrap();
        assert_eq!(updated.metadata.tags, vec!["nrf52", "wearable"]);
    }

    #[test]
    fn set_tags_accepts_json_array() {
        let r = ConfigRecord::with_defaults();
        let updated = set(
            r,
            "metadata.tags",
            serde_json::json!(["a", "b", "c"]),
        )
        .unwrap();
        assert_eq!(updated.metadata.tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn set_tags_rejects_empty_string_in_list() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "metadata.tags",
            serde_json::json!(["good", ""]),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }

    #[test]
    fn set_tags_rejects_whitespace_padded() {
        let r = ConfigRecord::with_defaults();
        let err = set(
            r,
            "metadata.tags",
            serde_json::json!([" leading"]),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TypeMismatch { .. }), "{:?}", err);
    }
}
