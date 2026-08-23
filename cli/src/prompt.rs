//! `magent set-prompt` subcommand.
//!
//! Persists system prompts as JSON files in a dedicated directory so
//! users can audit, version-control, and reuse them across `magent run`
//! invocations. The on-disk shape is deliberately close to what the
//! JSON envelope `RunReport` produces, so the same `serde_json`
//! rendering code can read both.
//!
//! ## Storage
//!
//! By default prompts live under the user's XDG data directory
//! (`$XDG_DATA_HOME/magent/prompts/<name>.json`, or
//! `$HOME/.local/share/magent/prompts/<name>.json` on macOS / Linux
//! when XDG is unset). The location can be overridden with the
//! `MAGENT_PROMPTS_DIR` environment variable for CI / container use.
//!
//! ## Subcommands
//!
//! ```text
//! magent set-prompt set    <NAME> --prompt <TEXT|FILE> [--provider ...] [--model ...]
//! magent set-prompt show   <NAME>
//! magent set-prompt list
//! magent set-prompt delete <NAME>
//! magent set-prompt export <NAME> > out.txt
//! ```
//!
//! Each JSON file carries:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "name": "health_coach",
//!   "prompt": "You are ...",
//!   "provider": "ollama",
//!   "model": "llama3.2",
//!   "metadata": {
//!     "description": "Embedded health coaching agent.",
//!     "tags": ["wearable", "nrf52"],
//!     "author": "you@example.com"
//!   },
//!   "created_at": "2026-08-09T...",
//!   "updated_at": "2026-08-09T..."
//! }
//! ```
//!
//! The `schema_version` field is reserved for future migrations; bump
//! it (and the [`PromptRecord::CURRENT_SCHEMA_VERSION`] constant) when
//! you make a breaking change to the JSON shape.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::cli::RunOptions;
use crate::output::{Output, OutputKind};
#[cfg(feature = "web3_app")]
use crate::web3 as web3_cli;

/// Current schema version of [`PromptRecord`]. Bump when you make a
/// breaking change to the JSON shape; readers MUST refuse to load
/// files whose `schema_version` is higher than this constant.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Environment variable that overrides the default prompts directory.
/// Useful for tests, CI runners, and containerised deployments where
/// `$XDG_DATA_HOME` is unset or points somewhere ephemeral.
pub const PROMPTS_DIR_ENV: &str = "MAGENT_PROMPTS_DIR";

/// Errors returned by the prompt storage layer. Each variant carries
/// enough context to print a one-line diagnostic on the CLI.
#[derive(Debug)]
pub enum PromptError {
    /// `--prompt <FILE>` couldn't be read.
    PromptFileLoad { path: PathBuf, source: io::Error },
    /// A user-supplied name was empty or contained path separators.
    InvalidName(String),
    /// A prompt's metadata field failed validation (length cap,
    /// control character, etc.). The string is human-readable.
    InvalidMetadata(String),
    /// The prompts directory couldn't be created or inspected.
    DirIo { path: PathBuf, source: io::Error },
    /// A file existed but couldn't be parsed as a `PromptRecord`.
    Parse { path: PathBuf, source: serde_json::Error },
    /// A file parsed but its `schema_version` is newer than what we
    /// support, so we'd risk dropping fields silently.
    UnsupportedSchema { path: PathBuf, found: u32, supported: u32 },
    /// The named prompt doesn't exist on disk.
    NotFound(String),
    /// Writing the JSON file failed.
    Write { path: PathBuf, source: io::Error },
    /// `magent set-prompt sign` failed (vault read, passphrase
    /// missing, signing error, …). Gated on the `web3_app`
    /// feature so non-Web3 builds don't pay for the variant.
    #[cfg(feature = "web3_app")]
    Sign(String),
    /// `magent set-prompt verify-signed` failed (parse error,
    /// signature mismatch, expiry, …). Same gating as
    /// [`PromptError::Sign`].
    #[cfg(feature = "web3_app")]
    Verify(String),
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptError::PromptFileLoad { path, source } => {
                write!(f, "could not read prompt file {}: {}", path.display(), source)
            }
            PromptError::InvalidName(name) => {
                write!(f, "invalid prompt name {:?}: must be non-empty and contain no path separators", name)
            }
            PromptError::InvalidMetadata(msg) => {
                write!(f, "invalid prompt metadata: {}", msg)
            }
            PromptError::DirIo { path, source } => {
                write!(f, "prompts directory {}: {}", path.display(), source)
            }
            PromptError::Parse { path, source } => {
                write!(f, "could not parse {} as a prompt: {}", path.display(), source)
            }
            PromptError::UnsupportedSchema { path, found, supported } => {
                write!(
                    f,
                    "{} has schema_version {} but this magent binary only understands up to {}; \
                     upgrade the binary first",
                    path.display(),
                    found,
                    supported
                )
            }
            PromptError::NotFound(name) => write!(f, "no prompt named {:?} on disk", name),
            PromptError::Write { path, source } => {
                write!(f, "could not write {}: {}", path.display(), source)
            }
            #[cfg(feature = "web3_app")]
            PromptError::Sign(msg) => write!(f, "set-prompt sign failed: {}", msg),
            #[cfg(feature = "web3_app")]
            PromptError::Verify(msg) => write!(f, "set-prompt verify-signed failed: {}", msg),
        }
    }
}

impl std::error::Error for PromptError {}

impl From<io::Error> for PromptError {
    fn from(source: io::Error) -> Self {
        PromptError::Write {
            // Use a generic placeholder; the message printed in the
            // Display impl still names the operation that failed
            // (we keep the variants specific so callers can pattern-
            // match on them). Using `"(io)"` as the path is honest
            // about the fact that we lost the original path — the
            // surrounding context already tells the user what was
            // being written.
            path: PathBuf::from("(io)"),
            source,
        }
    }
}

/// A single prompt stored on disk. Designed to be auditable: every
/// field is a primitive or a simple struct, no opaque blobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRecord {
    /// Bumped whenever the JSON shape changes in a breaking way. See
    /// [`CURRENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lower-case, filesystem-safe identifier. Used as the file stem
    /// (`<name>.json`). Never contains `/` or `..`.
    pub name: String,
    /// The actual system prompt text.
    pub prompt: String,
    /// Provider name (`"ollama"`, `"deepseek"`). Empty means "any
    /// provider, runner uses its default".
    #[serde(default)]
    pub provider: String,
    /// Model name. Empty means "use the provider's default model".
    #[serde(default)]
    pub model: String,
    /// Free-form metadata for humans (`description`, `tags`, `author`).
    /// Optional; older files written before this field existed will
    /// deserialize with an empty [`PromptMetadata`].
    #[serde(default)]
    pub metadata: PromptMetadata,
    /// Unix seconds. Set on first write, refreshed on every update.
    /// `serde(default)` lets `magent set-prompt import` accept
    /// hand-written JSON files that don't carry the timestamp —
    /// missing values are filled with the current time so the record
    /// still round-trips cleanly through `show`.
    #[serde(default = "now_unix_seconds")]
    pub created_at: u64,
    /// Unix seconds. Always ≥ `created_at`. Same default-on-missing
    /// behaviour as `created_at`.
    #[serde(default = "now_unix_seconds")]
    pub updated_at: u64,
}

/// Optional metadata for a prompt. `Option<String>` fields are `None`
/// when absent so a hand-edited JSON file can omit them without
/// breaking deserialisation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Free-form tags. Always serialised as a JSON array (possibly
    /// empty) so downstream tools can `grep` for them.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Maximum length (bytes) for a single tag. Mirrors
/// `ConfigRecord`'s `METADATA_TAG_MAX` so the two stores agree on
/// what a "reasonable" tag looks like.
pub const PROMPT_TAG_MAX: usize = 64;
/// Maximum tags per prompt. Beyond this the listing table gets
/// unreadable and the JSON file blows up.
pub const PROMPT_TAGS_MAX: usize = 32;
/// Maximum length (bytes) for `description`. Same magic number as
/// `ConfigRecord::METADATA_DESCRIPTION_MAX`.
pub const PROMPT_DESCRIPTION_MAX: usize = 1_024;
/// Maximum length (bytes) for `author`. Same as the config
/// counterpart so the two stores have consistent limits.
pub const PROMPT_AUTHOR_MAX: usize = 256;

/// Validate the contents of a [`PromptMetadata`]. Returns the
/// first violation found; the store calls this before write so
/// that a paste accident (leading/trailing whitespace, oversize
/// field, etc.) is caught before the file lands on disk.
///
/// We use a single `String` error rather than a structured
/// `ValidationIssue` because `PromptRecord` doesn't have a
/// `validate` CLI sub-command; the error is for the user, not
/// for a CI script.
pub fn validate_metadata(meta: &PromptMetadata) -> Result<(), PromptError> {
    if let Some(d) = &meta.description {
        if d.len() > PROMPT_DESCRIPTION_MAX {
            return Err(PromptError::InvalidMetadata(format!(
                "description is {} bytes; max is {}",
                d.len(),
                PROMPT_DESCRIPTION_MAX
            )));
        }
    }
    if let Some(a) = &meta.author {
        if a.len() > PROMPT_AUTHOR_MAX {
            return Err(PromptError::InvalidMetadata(format!(
                "author is {} bytes; max is {}",
                a.len(),
                PROMPT_AUTHOR_MAX
            )));
        }
        if a.chars().any(|c| c.is_control()) {
            return Err(PromptError::InvalidMetadata(
                "author contains control characters (newlines / tabs / escapes)"
                    .to_string(),
            ));
        }
    }
    if meta.tags.len() > PROMPT_TAGS_MAX {
        return Err(PromptError::InvalidMetadata(format!(
            "tags has {} entries; max is {}",
            meta.tags.len(),
            PROMPT_TAGS_MAX
        )));
    }
    for (i, tag) in meta.tags.iter().enumerate() {
        if tag.is_empty() {
            return Err(PromptError::InvalidMetadata(format!(
                "tags[{}] is empty",
                i
            )));
        }
        if tag != tag.trim() {
            return Err(PromptError::InvalidMetadata(format!(
                "tags[{}] {:?} has leading or trailing whitespace",
                i, tag
            )));
        }
        if tag.len() > PROMPT_TAG_MAX {
            return Err(PromptError::InvalidMetadata(format!(
                "tags[{}] is {} bytes; max is {}",
                i,
                tag.len(),
                PROMPT_TAG_MAX
            )));
        }
    }
    Ok(())
}

impl PromptRecord {
    /// Build a brand-new record. `now` is the wall-clock seconds; we
    /// accept it as an argument so unit tests can pin time.
    pub fn new(
        name: impl Into<String>,
        prompt: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        metadata: PromptMetadata,
        now: u64,
    ) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            name: name.into(),
            prompt: prompt.into(),
            provider: provider.into(),
            model: model.into(),
            metadata,
            created_at: now,
            updated_at: now,
        }
    }

    /// Refresh `updated_at` and return a new record. The `created_at`
    /// is preserved so audit logs can tell when the prompt was first
    /// written.
    pub fn updated(mut self, now: u64) -> Self {
        self.updated_at = now;
        self
    }
}

/// Resolve the prompts directory, honouring `MAGENT_PROMPTS_DIR` if
/// set. Falls back to `$XDG_DATA_HOME/magent/prompts` or
/// `$HOME/.local/share/magent/prompts`.
pub fn prompts_dir() -> Result<PathBuf, PromptError> {
    if let Ok(override_dir) = std::env::var(PROMPTS_DIR_ENV) {
        if !override_dir.trim().is_empty() {
            return Ok(PathBuf::from(override_dir));
        }
    }

    // XDG spec: $XDG_DATA_HOME or $HOME/.local/share.
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|home| format!("{}/.local/share", home))
        });

    match base {
        Some(b) => Ok(PathBuf::from(b).join("magent").join("prompts")),
        None => Err(PromptError::DirIo {
            // We use this variant to surface "we couldn't figure out
            // where to put prompts" — the message still reads OK.
            path: PathBuf::from("(none)"),
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "neither MAGENT_PROMPTS_DIR, XDG_DATA_HOME, nor HOME is set",
            ),
        }),
    }
}

/// Make sure the prompts directory exists. Returns the resolved
/// directory on success so callers don't have to call `prompts_dir()`
/// twice.
pub fn ensure_prompts_dir() -> Result<PathBuf, PromptError> {
    let dir = prompts_dir()?;
    fs::create_dir_all(&dir).map_err(|source| PromptError::DirIo {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// Validate a prompt name. Names map directly to file stems so they
/// must be filesystem-safe: no slashes, no parent-dir references, no
/// NUL bytes, no leading dots (which would create hidden files), and
/// no surrounding whitespace.
///
/// The empty string is rejected because it would map to `<dir>.json`
/// — a hidden file the user can't easily discover. Names like `.`
/// or `..` are rejected because of the same reason plus the parent-dir
/// traversal risk. Leading dots in any position are rejected because
/// POSIX systems treat them as hidden. `..` is *componentistically*
/// rejected (we check the right-hand side of a `/` or the whole
/// string), so `myname..withdot` is still allowed — only the
/// traversal sequence `..` as a path component is rejected.
pub fn validate_name(name: &str) -> Result<&str, PromptError> {
    if name.is_empty() {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    // Path separators — outright rejection.
    if name.contains('/') || name.contains('\\') {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    // NUL byte — outright rejection (would silently truncate the
    // filename on POSIX).
    if name.contains('\0') {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    // Reject a literal `..` (parent directory component) by
    // checking each component bounded by `/`. We already rejected
    // `/` above, so `..` is the whole string here.
    if name == ".." || name == "." {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    // Reject leading-dot names (which would create hidden files on
    // POSIX systems) and any name that begins or ends with
    // whitespace (which would map to a file path with embedded
    // spaces — confusing in shell and easy to typo).
    if name.starts_with('.') {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    if name != name.trim() {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    // Cap the length at 255 bytes (the POSIX `NAME_MAX` limit).
    // A longer name would fail to write to disk with an obscure
    // `ENAMETOOLONG` error; rejecting it at the validator is
    // clearer. We measure bytes (not chars) because that's what
    // the filesystem limit is on.
    if name.len() > MAX_NAME_LEN {
        return Err(PromptError::InvalidName(name.to_string()));
    }
    Ok(name)
}

/// Maximum permitted prompt name length in bytes. Matches the
/// POSIX `NAME_MAX` limit so a valid name here is guaranteed to
/// be writable to disk on every major target.
const MAX_NAME_LEN: usize = 255;

/// Compose the full file path for a named prompt.
fn prompt_path(name: &str) -> Result<PathBuf, PromptError> {
    let dir = prompts_dir()?;
    Ok(dir.join(format!("{}.json", validate_name(name)?)))
}

/// Load a prompt by name. Returns [`PromptError::NotFound`] if the
/// file doesn't exist.
pub fn load(name: &str) -> Result<PromptRecord, PromptError> {
    let path = prompt_path(name)?;
    let raw = fs::read_to_string(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            PromptError::NotFound(name.to_string())
        } else {
            PromptError::PromptFileLoad {
                path: path.clone(),
                source,
            }
        }
    })?;
    let record: PromptRecord =
        serde_json::from_str(&raw).map_err(|source| PromptError::Parse {
            path: path.clone(),
            source,
        })?;
    if record.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(PromptError::UnsupportedSchema {
            path,
            found: record.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(record)
}

/// Persist a prompt to disk. If a file already exists for `name`, we
/// preserve its `created_at` so audit logs can tell when the prompt
/// was first written.
pub fn save(record: PromptRecord) -> Result<PathBuf, PromptError> {
    let _ = validate_name(&record.name)?;
    let dir = ensure_prompts_dir()?;
    let path = dir.join(format!("{}.json", record.name));

    // Preserve `created_at` if the file exists.
    let created_at = match fs::read_to_string(&path) {
        Ok(existing) => serde_json::from_str::<PromptRecord>(&existing)
            .map(|r| r.created_at)
            .unwrap_or(record.created_at),
        Err(_) => record.created_at,
    };

    let mut record = record;
    record.created_at = created_at;
    record.updated_at = now_unix_seconds();
    record.schema_version = CURRENT_SCHEMA_VERSION;

    // Validate metadata before writing so a paste accident lands
    // as a friendly error rather than as a corrupted-on-disk
    // record that the user only notices later.
    validate_metadata(&record.metadata)?;

    // 2-space indentation: makes `git diff` legible for hand-edited
    // prompts. The size penalty is small (a 4 KB prompt becomes ~6 KB
    // JSON, still well under any realistic cap).
    let json = serde_json::to_string_pretty(&record).map_err(|source| PromptError::Parse {
        path: path.clone(),
        source,
    })?;
    fs::write(&path, json).map_err(|source| PromptError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Delete a prompt by name. Returns `Ok(true)` if the file was
/// removed, `Ok(false)` if it didn't exist (so the CLI can warn
/// without failing outright).
pub fn delete(name: &str) -> Result<bool, PromptError> {
    let path = prompt_path(name)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PromptError::Write { path, source }),
    }
}

/// List all prompts currently on disk. Entries are sorted by name so
/// the CLI output is stable for `diff` / CI consumption.
pub fn list() -> Result<Vec<PromptRecord>, PromptError> {
    let dir = prompts_dir()?;
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(PromptError::DirIo {
                path: dir.clone(),
                source,
            })
        }
    };

    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PromptError::DirIo {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|source| PromptError::PromptFileLoad {
            path: path.clone(),
            source,
        })?;
        let record: PromptRecord =
            serde_json::from_str(&raw).map_err(|source| PromptError::Parse {
                path: path.clone(),
                source,
            })?;
        if record.schema_version <= CURRENT_SCHEMA_VERSION {
            out.push(record);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read the contents of `--prompt <FILE>` if it points to a file, or
/// treat the literal value as the prompt itself. The decision rule is
/// "does the value exist as a file?" — same heuristic `clap`'s
/// `Arg::value_parser` would use for "file or string" inputs.
pub fn read_prompt_source(value: &str) -> Result<String, PromptError> {
    let candidate = Path::new(value);
    if candidate.is_file() {
        let mut buf = String::new();
        let mut f = fs::File::open(candidate).map_err(|source| PromptError::PromptFileLoad {
            path: candidate.to_path_buf(),
            source,
        })?;
        f.read_to_string(&mut buf)
            .map_err(|source| PromptError::PromptFileLoad {
                path: candidate.to_path_buf(),
                source,
            })?;
        return Ok(buf.trim_end().to_string());
    }
    Ok(value.to_string())
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ============================================================================
// Subcommand plumbing
// ============================================================================

/// Sub-actions of `magent set-prompt`. The CLI parser picks one of
/// these from the second positional argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetPromptAction {
    /// `magent set-prompt set <NAME> ...` — write or overwrite.
    Set(SetPromptSetOptions),
    /// `magent set-prompt show <NAME>` — print the JSON record.
    Show(String),
    /// `magent set-prompt list` — list every stored prompt.
    List,
    /// `magent set-prompt delete <NAME>` — remove the file.
    Delete(String),
    /// `magent set-prompt export <NAME>` — print just the prompt text.
    Export(String),
    /// `magent set-prompt import <PATH> [--name <NAME>]` — read a
    /// JSON file and write its record into the store. The new file
    /// uses the name passed via `--name` (or the `name` field inside
    /// the JSON, or the file stem, in that order).
    Import(SetPromptImportOptions),
    /// `magent set-prompt template <NAME> [--var KEY=VALUE]…` —
    /// render the stored prompt with `{{KEY}}` placeholders
    /// substituted from the supplied variables.
    Template(SetPromptTemplateOptions),
    /// `magent set-prompt sign <NAME> [--signer <NAME>] [--signed-output <PATH>]`
    /// — load the prompt, build a `SignedPrompt` envelope using the
    /// named vault identity, and write the JSON envelope to disk.
    /// Gated on the `web3_app` feature; without it the parser
    /// rejects the action with `UnknownFlag`.
    #[cfg(feature = "web3_app")]
    Sign(SetPromptSignOptions),
    /// `magent set-prompt verify-signed <PATH>` — read a
    /// `SignedPrompt` JSON envelope and verify its signature +
    /// domain-separation + clock-window. Gated on `web3_app`.
    #[cfg(feature = "web3_app")]
    VerifySigned(SetPromptVerifySignedOptions),
}

/// Options for the `set-prompt set <NAME>` sub-action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPromptSetOptions {
    pub name: String,
    /// Either the literal prompt text or a path to a file. See
    /// [`read_prompt_source`].
    pub prompt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub tags: Vec<String>,
}

/// Options for the `set-prompt import <PATH>` sub-action.
///
/// The file path is required. `--name` is optional and overrides
/// whatever the JSON's `name` field says — useful for renaming on
/// import (e.g. importing `health_coach.json` as
/// `health_coach_v2`). `--force` overwrites an existing prompt with
/// the same name without complaining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPromptImportOptions {
    pub path: PathBuf,
    pub name: Option<String>,
    pub force: bool,
}

/// Options for the `set-prompt template <NAME>` sub-action.
///
/// `--var KEY=VALUE` is repeatable; each occurrence adds one
/// binding. `--vars-from <PATH>` reads a JSON object whose keys
/// are the variable names (so a single file can supply a
/// pre-baked dictionary of substitutions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPromptTemplateOptions {
    pub name: String,
    pub vars: Vec<(String, String)>,
    pub vars_from: Option<PathBuf>,
}

/// Options for the `set-prompt sign <NAME>` sub-action.
///
/// `--signer <NAME>` names the vault identity whose secret key
/// is used to sign the envelope; defaults to `"default"` (the
/// same convention the `magent run --sign` path uses). The
/// passphrase comes from `MAGENT_WEB3_PASSPHRASE` or whatever
/// `--passphrase-env <VAR>` points at.
///
/// `--signed-output <PATH>` overrides the destination file;
/// defaults to `<prompts-dir>/<name>.signed.json`. The
/// envelope is also printed to stdout in JSON mode for
/// downstream tooling.
///
/// Gated on the `web3_app` feature so the type doesn't exist
/// in non-Web3 builds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(feature = "web3_app")]
pub struct SetPromptSignOptions {
    pub name: String,
    pub signer: String,
    pub signed_output: Option<PathBuf>,
    pub passphrase_env: Option<String>,
    pub not_before_unix: Option<u64>,
    pub not_after_unix: Option<u64>,
}

/// Options for the `set-prompt verify-signed <PATH>` sub-action.
///
/// `path` is the location of the JSON envelope on disk. The
/// clock is read from the local wall clock (`now_secs`) by the
/// runner; we don't expose `--now` because signed-envelope
/// verification is always done against "now".
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(feature = "web3_app")]
pub struct SetPromptVerifySignedOptions {
    pub path: PathBuf,
}

/// Glue struct so `main.rs` can construct and run the subcommand in
/// one line, mirroring `RunCmd` / `DoctorCmd`.
pub struct SetPromptCmd<'a> {
    pub action: &'a SetPromptAction,
}

impl<'a> SetPromptCmd<'a> {
    pub fn new(action: &'a SetPromptAction) -> Self {
        Self { action }
    }

    /// Execute the subcommand. Always writes to `out` so both human
    /// and JSON modes get consistent output.
    pub fn execute(&self, out: &mut Output) -> Result<(), PromptError> {
        match self.action {
            SetPromptAction::Set(opts) => self.run_set(opts, out),
            SetPromptAction::Show(name) => self.run_show(name, out),
            SetPromptAction::List => self.run_list(out),
            SetPromptAction::Delete(name) => self.run_delete(name, out),
            SetPromptAction::Export(name) => self.run_export(name, out),
            SetPromptAction::Import(opts) => self.run_import(opts, out),
            SetPromptAction::Template(opts) => self.run_template(opts, out),
            #[cfg(feature = "web3_app")]
            SetPromptAction::Sign(opts) => self.run_sign(opts, out),
            #[cfg(feature = "web3_app")]
            SetPromptAction::VerifySigned(opts) => self.run_verify_signed(opts, out),
        }
    }

    fn run_set(&self, opts: &SetPromptSetOptions, out: &mut Output) -> Result<(), PromptError> {
        let prompt_text = read_prompt_source(&opts.prompt)?;
        let metadata = PromptMetadata {
            description: opts.description.clone(),
            author: opts.author.clone(),
            tags: opts.tags.clone(),
        };
        let now = now_unix_seconds();
        // If the user is re-running `set`, preserve the original
        // `created_at` by going through `save` (which already merges).
        let record = match load(&opts.name) {
            Ok(existing) => PromptRecord {
                prompt: prompt_text.clone(),
                provider: opts.provider.clone().unwrap_or(existing.provider),
                model: opts.model.clone().unwrap_or(existing.model),
                metadata: PromptMetadata {
                    description: opts
                        .description
                        .clone()
                        .or(existing.metadata.description),
                    author: opts.author.clone().or(existing.metadata.author),
                    // Tags: replace if the user provided any, else keep.
                    tags: if opts.tags.is_empty() {
                        existing.metadata.tags
                    } else {
                        opts.tags.clone()
                    },
                },
                ..existing
            }
            .updated(now),
            Err(PromptError::NotFound(_)) => PromptRecord::new(
                opts.name.clone(),
                prompt_text.clone(),
                opts.provider.clone().unwrap_or_default(),
                opts.model.clone().unwrap_or_default(),
                metadata,
                now,
            ),
            Err(e) => return Err(e),
        };

        let path = save(record.clone())?;
        out.info(&format!(
            "stored prompt {:?} ({})",
            record.name,
            human_size(record.prompt.len())
        ))?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "set",
                "path": path.to_string_lossy(),
                "prompt": &record,
            }))?;
        }
        Ok(())
    }

    fn run_show(&self, name: &str, out: &mut Output) -> Result<(), PromptError> {
        let record = load(name)?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::to_value(&record).map_err(|source| {
                PromptError::Parse {
                    path: PathBuf::from(name),
                    source,
                }
            })?)?;
        } else {
            // Pretty-print the same shape the file on disk uses so
            // the user can compare `set-prompt show` with `cat
            // ~/.local/share/magent/prompts/<name>.json`.
            let pretty = serde_json::to_string_pretty(&record)
                .map_err(|source| PromptError::Parse {
                    path: PathBuf::from(name),
                    source,
                })?;
            let _ = out.stderr_fmt_line(format_args!("{}", pretty));
        }
        Ok(())
    }

    fn run_list(&self, out: &mut Output) -> Result<(), PromptError> {
        let records = list()?;
        if matches!(out.kind(), OutputKind::Json) {
            // Wrap the array under a `prompts` key so the JSON
            // envelope stays a single object — `write_json` only
            // merges in fields when handed an Object, and a raw
            // `Value::Array` would otherwise be silently dropped.
            let json_records = serde_json::to_value(&records).map_err(|source| {
                PromptError::Parse {
                    path: PathBuf::from("(list)"),
                    source,
                }
            })?;
            out.write_json(serde_json::json!({ "prompts": json_records }))?;
        } else if records.is_empty() {
            out.info("no prompts stored yet — try `magent set-prompt set ...` first")?;
        } else {
            let _ = out.stderr_fmt_line(format_args!(
                "{:<24} {:<10} {:<20} TAGS",
                "NAME", "PROVIDER", "MODEL"
            ));
            let _ = out.stderr_fmt_line(format_args!(
                "{}",
                "-".repeat(24 + 10 + 20 + 8)
            ));
            for r in &records {
                let _ = out.stderr_fmt_line(format_args!(
                    "{:<24} {:<10} {:<20} {}",
                    truncate(&r.name, 24),
                    truncate(if r.provider.is_empty() { "(default)" } else { &r.provider }, 10),
                    truncate(if r.model.is_empty() { "(default)" } else { &r.model }, 20),
                    r.metadata.tags.join(",")
                ));
            }
        }
        Ok(())
    }

    fn run_delete(&self, name: &str, out: &mut Output) -> Result<(), PromptError> {
        let removed = delete(name)?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "delete",
                "name": name,
                "removed": removed,
            }))?;
        } else if removed {
            out.info(&format!("removed prompt {:?}", name))?;
        } else {
            out.info(&format!("prompt {:?} did not exist", name))?;
        }
        Ok(())
    }

    fn run_export(&self, name: &str, out: &mut Output) -> Result<(), PromptError> {
        let record = load(name)?;
        // Always write the raw prompt text to stdout — export is a
        // piping command and JSON envelopes would force downstream
        // tools to parse them. The metadata (if anyone needs it)
        // is available via `show`.
        let stdout = out.stdout_writer();
        stdout
            .write_all(record.prompt.as_bytes())
            .map_err(|source| PromptError::Write {
                path: PathBuf::from("<stdout>"),
                source,
            })?;
        Ok(())
    }

    /// Read a JSON file from disk, optionally rename it, and write
    /// it into the prompt store. The name resolution order is:
    ///
    /// 1. `--name <NAME>` (explicit override).
    /// 2. The `name` field inside the JSON.
    /// 3. The file stem (e.g. `health_coach.json` → `health_coach`).
    ///
    /// Existing prompts with the resolved name are refused unless
    /// `--force` is set; the user can then re-run with `--force` to
    /// confirm the overwrite.
    fn run_import(
        &self,
        opts: &SetPromptImportOptions,
        out: &mut Output,
    ) -> Result<(), PromptError> {
        let raw = fs::read_to_string(&opts.path).map_err(|source| {
            PromptError::PromptFileLoad {
                path: opts.path.clone(),
                source,
            }
        })?;
        // Parse the JSON. We use `PromptRecord` as the target shape
        // so any unknown fields surface as a parse error rather
        // than being silently dropped — better to fail loudly than
        // to round-trip data loss.
        let mut record: PromptRecord =
            serde_json::from_str(&raw).map_err(|source| PromptError::Parse {
                path: opts.path.clone(),
                source,
            })?;
        // Resolve the final name (override → JSON → file stem).
        let final_name = opts
            .name
            .clone()
            .or_else(|| {
                let n = record.name.trim().to_string();
                if n.is_empty() { None } else { Some(n) }
            })
            .unwrap_or_else(|| {
                opts.path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported")
                    .to_string()
            });
        // Validate the resolved name *before* doing the existence
        // check, so an invalid name surfaces the right error
        // (`InvalidName`) instead of slipping through the
        // `unwrap_or(false)` to fail later in `save` with a
        // confusing "could not write" message.
        let final_name = validate_name(&final_name)?.to_string();
        // Refuse the import if it would clobber an existing prompt
        // unless `--force` was passed.
        if !opts.force && prompt_path(&final_name).map(|p| p.exists()).unwrap_or(false) {
            return Err(PromptError::Write {
                path: prompt_path(&final_name).unwrap_or_else(|_| PathBuf::from("?")),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "a prompt named {:?} already exists; pass --force to overwrite",
                        final_name
                    ),
                ),
            });
        }
        // Stamp the resolved name back into the record so the JSON
        // we write to the store matches the file we wrote to disk.
        record.name = final_name.clone();
        save(record)?;
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "import",
                "name": final_name,
                "path": opts.path.to_string_lossy(),
            }))?;
        } else {
            out.info(&format!(
                "imported {} as {:?}",
                opts.path.display(),
                final_name
            ))?;
        }
        Ok(())
    }

    /// Render a stored prompt with `{{KEY}}` placeholders
    /// substituted. Variables come from two sources, merged in
    /// `--var` → `--vars-from` order:
    ///
    /// 1. `--var KEY=VALUE` flags (repeatable; later wins on conflict).
    /// 2. `--vars-from <PATH>` JSON object whose keys are variable
    ///    names and whose values are strings.
    ///
    /// In Human mode the rendered text is written to stdout (so it
    /// pipes cleanly into `magent run --prompt "$(...)"`). In JSON
    /// mode the rendered text is wrapped in an envelope so callers
    /// can extract it programmatically.
    ///
    /// Unknown placeholders (`{{FOO}}` with no `FOO` binding) are
    /// left untouched — we deliberately do not error so partial
    /// renders stay useful for previewing what would change.
    fn run_template(
        &self,
        opts: &SetPromptTemplateOptions,
        out: &mut Output,
    ) -> Result<(), PromptError> {
        let record = load(&opts.name)?;
        // Merge variables: --var overrides --vars-from on conflict.
        let mut vars: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        if let Some(p) = &opts.vars_from {
            let raw = fs::read_to_string(p).map_err(|source| {
                PromptError::PromptFileLoad { path: p.clone(), source }
            })?;
            let parsed: serde_json::Value =
                serde_json::from_str(&raw).map_err(|source| PromptError::Parse {
                    path: p.clone(),
                    source,
                })?;
            let obj = parsed.as_object().ok_or_else(|| PromptError::Parse {
                path: p.clone(),
                source: serde_json::Error::custom(
                    "--vars-from file must be a JSON object of string→string",
                ),
            })?;
            for (k, v) in obj {
                // Only top-level scalar values are useful as
                // template variables. Nested objects / arrays
                // would string-serialize to JSON (e.g.
                // `["a","b"]`) which is almost never what the
                // user wants in a prompt — surface a clear error
                // so they can fix the file.
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    other => {
                        return Err(PromptError::Parse {
                            path: p.clone(),
                            source: serde_json::Error::custom(format!(
                                "variable {:?} is {}; only strings, numbers, bools, and null are allowed as template values",
                                k, other
                            )),
                        })
                    }
                };
                vars.insert(k.clone(), s);
            }
        }
        for (k, v) in &opts.vars {
            vars.insert(k.clone(), v.clone());
        }
        let (rendered, warnings) = render_template_with_warnings(&record.prompt, &vars);
        if matches!(out.kind(), OutputKind::Json) {
            out.write_json(serde_json::json!({
                "action": "template",
                "name": opts.name,
                "rendered": rendered,
                "variables": vars,
                "unfilled": warnings,
            }))?;
        } else {
            // Pipeable output: write the rendered prompt directly to
            // stdout with no decoration, so callers can use
            // `magent run --prompt "$(magent set-prompt template foo --var X=1)"`.
            let stdout = out.stdout_writer();
            stdout
                .write_all(rendered.as_bytes())
                .map_err(|source| PromptError::Write {
                    path: PathBuf::from("<stdout>"),
                    source,
                })?;
            // After the body, surface any unfilled placeholders
            // as a single warning line. We use stderr so the
            // stdout body remains clean for command
            // substitution.
            if !warnings.is_empty() {
                let _ = out.stderr_fmt_line(format_args!(
                    "warning: unfilled placeholders: {}",
                    warnings.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// `magent set-prompt sign <NAME> [--signer <NAME>] [--signed-output <PATH>] …`.
    ///
    /// Mirrors the `magent run --sign` flow: load the named
    /// prompt, mirror it into `PromptFields`, decrypt the
    /// vault identity via `web3_cli::decrypt_identity`, build
    /// a `SignedPrompt` envelope, and write the JSON to disk
    /// (default location: `<prompts-dir>/<name>.signed.json`).
    ///
    /// Errors map to [`PromptError`] so the CLI exit-code
    /// table treats them like any other `set-prompt` failure.
    #[cfg(feature = "web3_app")]
    fn run_sign(
        &self,
        opts: &SetPromptSignOptions,
        out: &mut Output,
    ) -> Result<(), PromptError> {
        // 1. Load the prompt record from disk.
        let record = load(&opts.name)?;

        // 2. Resolve the passphrase via the env var, falling
        //    back to `--passphrase-env <VAR>` (which re-routes
        //    to a different env name; defaults to
        //    `MAGENT_WEB3_PASSPHRASE`).
        let env_var = opts
            .passphrase_env
            .as_deref()
            .unwrap_or("MAGENT_WEB3_PASSPHRASE");
        let passphrase = std::env::var(env_var).map_err(|_| {
            PromptError::Sign(format!(
                "passphrase not found in environment variable {}",
                env_var
            ))
        })?;
        if passphrase.is_empty() {
            return Err(PromptError::Sign(format!(
                "passphrase env var {} is empty",
                env_var
            )));
        }

        // 3. Decrypt the identity from the default vault
        //    location, named by `--signer` (defaults to
        //    `"default"`).
        let vault_path = web3_cli::default_vault_path();
        let mut vault = if vault_path.exists() {
            web3_cli::load_vault(&vault_path).map_err(|e| PromptError::Sign(
                format!("could not load vault {}: {}", vault_path.display(), e),
            ))?
        } else {
            web3_cli::empty_vault()
        };
        let identity =
            web3_cli::decrypt_identity(&mut vault, &opts.signer, passphrase.as_bytes()).map_err(
                |e| PromptError::Sign(format!("decrypt_identity failed: {}", e)),
            )?;

        // 4. Mirror `PromptRecord` into the typed payload.
        let payload = magent_core::web3_app::PromptFields::new(
            record.name.clone(),
            record.prompt.clone(),
            record.provider.clone(),
            record.model.clone(),
            record.created_at,
            record.updated_at,
        );

        // 5. Issue timestamp is "now"; the signer is allowed to
        //    set an expiry window via `--not-before` /
        //    `--not-after`.
        let now_secs = now_unix_seconds();
        let envelope = magent_core::web3_app::SignedPrompt::sign(
            &identity,
            now_secs,
            opts.not_before_unix,
            opts.not_after_unix,
            payload,
        )
        .map_err(|e| PromptError::Sign(format!("sign failed: {}", e)))?;

        // 6. Write the envelope to disk. Default location is
        //    alongside the prompt file but with a `.signed.json`
        //    suffix.
        let out_path = opts.signed_output.clone().unwrap_or_else(|| {
            // If the prompts dir can't be resolved we fall back
            // to "." — the write will then surface the real
            // problem via `PromptError::Write` rather than a
            // generic "could not find prompts dir".
            let prompts_dir = prompts_dir().unwrap_or_else(|_| PathBuf::from("."));
            prompts_dir.join(format!("{}.signed.json", opts.name))
        });
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| {
                PromptError::Write {
                    path: out_path.clone(),
                    source,
                }
            })?;
        }
        let json = envelope.to_json_pretty();
        std::fs::write(&out_path, json.as_bytes()).map_err(|source| {
            PromptError::Write {
                path: out_path.clone(),
                source,
            }
        })?;

        // 7. Emit user-facing feedback.
        if matches!(out.kind(), OutputKind::Json) {
            let parsed: serde_json::Value = serde_json::from_str(&envelope.to_json())
                .map_err(|source| PromptError::Parse {
                    path: out_path.clone(),
                    source,
                })?;
            out.write_json(serde_json::json!({
                "action": "sign",
                "path": out_path.to_string_lossy(),
                "signer": envelope.signer,
                "signed_envelope": parsed,
            }))?;
        } else {
            let _ = out.info(&format!(
                "signed prompt {:?} → {} (signer={})",
                opts.name,
                out_path.display(),
                envelope.signer
            ));
        }
        Ok(())
    }

    /// `magent set-prompt verify-signed <PATH>`.
    ///
    /// Reads a `SignedPrompt` envelope from disk and verifies
    /// it against the local wall clock. In JSON mode we emit
    /// the envelope's `payload` field back to the caller so
    /// downstream tooling can pipe the verified content.
    #[cfg(feature = "web3_app")]
    fn run_verify_signed(
        &self,
        opts: &SetPromptVerifySignedOptions,
        out: &mut Output,
    ) -> Result<(), PromptError> {
        let raw = std::fs::read_to_string(&opts.path).map_err(|source| {
            PromptError::PromptFileLoad {
                path: opts.path.clone(),
                source,
            }
        })?;
        let now = now_unix_seconds();
        let env = magent_core::web3_app::SignedPrompt::parse_and_verify(&raw, now)
            .map_err(|e| PromptError::Verify(format!("verify_signed_prompt failed: {}", e)))?;

        if matches!(out.kind(), OutputKind::Json) {
            let payload_value = serde_json::to_value(&env.payload).map_err(|source| {
                PromptError::Parse {
                    path: opts.path.clone(),
                    source,
                }
            })?;
            out.write_json(serde_json::json!({
                "action": "verify-signed",
                "path": opts.path.to_string_lossy(),
                "signer": env.signer,
                "payload_type": env.payload_type,
                "issued_at_unix": env.issued_at_unix,
                "not_before_unix": env.not_before_unix,
                "not_after_unix": env.not_after_unix,
                "payload": payload_value,
            }))?;
        } else {
            let _ = out.info(&format!(
                "✓ verified prompt envelope: signer={} issued_at={} path={}",
                env.signer,
                env.issued_at_unix,
                opts.path.display()
            ));
        }
        Ok(())
    }
}

/// Substitute every `{{KEY}}` placeholder in `template` with the
/// matching value from `vars`. Whitespace inside the braces is
/// tolerated (`{{ KEY }}` is equivalent to `{{KEY}}`). Unknown
/// placeholders are left as-is so users can preview a render that
/// is only partially populated.
pub fn render_template(template: &str, vars: &std::collections::BTreeMap<String, String>) -> String {
    let (rendered, _warnings) = render_template_with_warnings(template, vars);
    rendered
}

/// Render `template` and return both the rendered text and a
/// list of placeholders that had no matching variable. Unknown
/// placeholders are left as-is in the output (so the user can
/// see what was missing) and *also* surfaced via the warnings
/// vector so the CLI can emit a single line about them at the
/// end of the run.
///
/// We don't refuse on unknown placeholders — the user may be
/// rendering a template that intentionally has unfilled slots
/// (e.g. a partial preview). We just make sure the missed
/// substitutions are visible.
pub fn render_template_with_warnings(
    template: &str,
    vars: &std::collections::BTreeMap<String, String>,
) -> (String, Vec<String>) {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut warnings: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Look for the opening `{{`. Anything that isn't `{{` is
        // copied through verbatim.
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the matching `}}`. If we don't find it, copy the
            // `{{` through and continue — we never error on
            // malformed input.
            if let Some(end_rel) = find_close(&bytes[i + 2..]) {
                let name_raw = &template[i + 2..i + 2 + end_rel];
                let name = name_raw.trim();
                match vars.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        // Leave the placeholder intact so the caller
                        // can spot what was missing. Dedupe warnings
                        // — a template that uses `{{x}}` ten times
                        // produces one warning, not ten.
                        if !warnings.iter().any(|w| w == name) {
                            warnings.push(name.to_string());
                        }
                        out.push_str(&template[i..i + 2 + end_rel + 2]);
                    }
                }
                i += 2 + end_rel + 2;
                continue;
            } else {
                out.push_str("{{");
                i += 2;
                continue;
            }
        }
        // Push the next character (handling UTF-8 safely by going
        // char-by-char when we don't match `{{`).
        let ch_end = next_char_boundary(template, i);
        out.push_str(&template[i..ch_end]);
        i = ch_end;
    }
    (out, warnings)
}

fn find_close(bytes: &[u8]) -> Option<usize> {
    // Returns the offset of the matching `}}` relative to the start
    // of `bytes`. The `}}` must close within the slice.
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Render a prompt path given the `MAGENT_PROMPTS_DIR` resolution. We
/// expose this (rather than just `prompts_dir()`) so the CLI can print
/// the resolved location next to the `--prompt-name` lookup message.
pub fn resolved_prompts_dir_string() -> String {
    match prompts_dir() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => "<unresolved — set $MAGENT_PROMPTS_DIR or $HOME>".to_string(),
    }
}

/// Resolve a `--prompt-name <NAME>` into a [`PromptRecord`], with a
/// fallback to `RunOptions::prompt_file` for callers that prefer to
/// hand a path instead. Returns the system-prompt string the runner
/// should use, plus the (possibly overridden) provider/model.
pub struct ResolvedPrompt {
    pub text: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Resolution order:
///
///   1. `--prompt-name <NAME>`        — load from the prompts directory
///   2. `--prompt <FILE>`             — load from disk (legacy path)
///   3. `RunnerConfig::default()`     — built-in default
pub fn resolve_for_run(opts: &RunOptions) -> Result<ResolvedPrompt, PromptError> {
    // 1. Named prompt wins over everything. This is the new preferred
    //    path because the prompt is version-controlled JSON, not an
    //    arbitrary .txt file.
    if let Some(name) = opts.prompt_name.as_deref() {
        let record = load(name)?;
        return Ok(ResolvedPrompt {
            text: record.prompt,
            provider: non_empty(record.provider),
            model: non_empty(record.model),
        });
    }
    // 2. Legacy file path.
    if let Some(path) = opts.prompt_file.as_ref() {
        let text = crate::runner::load_prompt_file(path).map_err(|e| {
            PromptError::PromptFileLoad {
                path: path.clone(),
                source: io::Error::other(e),
            }
        })?;
        return Ok(ResolvedPrompt {
            text,
            provider: None,
            model: None,
        });
    }
    // 3. Built-in default.
    Ok(ResolvedPrompt {
        text: crate::runner::default_system_prompt().to_string(),
        provider: None,
        model: None,
    })
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
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

fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Per-test serialisation: the prompt directory is process-global
    // (env var, XDG fallback), so we run tests one at a time to avoid
    // stepping on each other.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that sets `MAGENT_PROMPTS_DIR` to a unique temp
    /// directory for the duration of a test, restoring the prior
    /// value on drop.
    struct TempPromptsDir(PathBuf);
    impl TempPromptsDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("magent_prompts_{}_{}", label, std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create temp prompts dir");
            // SAFETY: the env var is only mutated while the global
            // test lock is held, so we can't race with another test.
            unsafe {
                std::env::set_var(PROMPTS_DIR_ENV, &dir);
            }
            Self(dir)
        }
    }
    impl Drop for TempPromptsDir {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var(PROMPTS_DIR_ENV);
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn schema_version_constant_is_one() {
        // A reminder to keep tests and documentation in lockstep.
        assert_eq!(CURRENT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn validate_name_rejects_path_separators() {
        assert!(validate_name("foo/bar").is_err());
        assert!(validate_name("foo\\bar").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\0b").is_err());
        assert_eq!(validate_name("health_coach").unwrap(), "health_coach");
        assert_eq!(validate_name("p1").unwrap(), "p1");
    }

    #[test]
    fn validate_name_rejects_hidden_and_whitespace_names() {
        // Names that would create hidden files on POSIX systems.
        assert!(validate_name(".").is_err(), "bare dot should be rejected");
        assert!(validate_name(".hidden").is_err(), "leading-dot should be rejected");
        // Whitespace around the name is rejected because file paths
        // with embedded spaces are easy to typo and shell-hostile.
        assert!(validate_name("   ").is_err(), "pure whitespace should be rejected");
        assert!(validate_name(" leading").is_err(), "leading whitespace should be rejected");
        assert!(validate_name("trailing ").is_err(), "trailing whitespace should be rejected");
        // Sanity: a normal name still works.
        assert_eq!(validate_name("abc").unwrap(), "abc");
    }

    #[test]
    fn validate_name_accepts_double_dots_in_middle() {
        // `myname..withdot` is a legitimate filename — the substring
        // `..` only matters when it is a path *component*. Names
        // containing `..` as a literal substring (e.g. user names
        // with two dots) should be accepted.
        assert_eq!(validate_name("myname..withdot").unwrap(), "myname..withdot");
        assert_eq!(validate_name("a..b").unwrap(), "a..b");
        // Leading-dot is still rejected (hidden file risk).
        assert!(validate_name("..foo").is_err());
        assert!(validate_name(".foo").is_err());
    }

    #[test]
    fn validate_name_rejects_long_names() {
        // POSIX NAME_MAX is 255 bytes; we reject anything longer
        // here instead of letting the OS surface ENAMETOOLONG.
        assert!(validate_name(&"a".repeat(256)).is_err(),
            "names longer than 255 bytes should be rejected");
        // 255 bytes exactly is OK.
        assert!(validate_name(&"a".repeat(255)).is_ok(),
            "names of exactly 255 bytes should be accepted");
    }

    #[test]
    fn validate_metadata_accepts_reasonable_input() {
        let meta = PromptMetadata {
            description: Some("a description".to_string()),
            author: Some("alice".to_string()),
            tags: vec!["nrf52".to_string(), "wearable".to_string()],
        };
        assert!(validate_metadata(&meta).is_ok());
    }

    #[test]
    fn validate_metadata_rejects_oversized_author() {
        let meta = PromptMetadata {
            author: Some("a".repeat(PROMPT_AUTHOR_MAX + 1)),
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn validate_metadata_rejects_author_with_control_chars() {
        for bad in ["alice\nbob", "alice\ttab", "alice\u{1b}ESC"] {
            let meta = PromptMetadata {
                author: Some(bad.to_string()),
                ..Default::default()
            };
            assert!(
                validate_metadata(&meta).is_err(),
                "expected an error for {:?}",
                bad
            );
        }
    }

    #[test]
    fn validate_metadata_rejects_oversized_tag() {
        let meta = PromptMetadata {
            tags: vec!["a".repeat(PROMPT_TAG_MAX + 1)],
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn validate_metadata_rejects_too_many_tags() {
        let meta = PromptMetadata {
            tags: (0..PROMPT_TAGS_MAX + 1)
                .map(|i| format!("tag{}", i))
                .collect(),
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn validate_metadata_rejects_empty_tag() {
        let meta = PromptMetadata {
            tags: vec!["good".to_string(), String::new()],
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn validate_metadata_rejects_whitespace_padded_tag() {
        let meta = PromptMetadata {
            tags: vec![" leading".to_string()],
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn validate_metadata_rejects_oversized_description() {
        let meta = PromptMetadata {
            description: Some("a".repeat(PROMPT_DESCRIPTION_MAX + 1)),
            ..Default::default()
        };
        assert!(validate_metadata(&meta).is_err());
    }

    #[test]
    fn record_round_trips_via_json() {
        let r = PromptRecord::new(
            "alpha",
            "You are a helpful agent.",
            "ollama",
            "llama3.2",
            PromptMetadata {
                description: Some("first test".into()),
                author: Some("me".into()),
                tags: vec!["a".into(), "b".into()],
            },
            1_000,
        );
        let json = serde_json::to_string(&r).unwrap();
        let back: PromptRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn record_missing_metadata_round_trips() {
        // Hand-written JSON files often omit the metadata block.
        // Make sure we can still load them.
        let json = r#"{
            "schema_version": 1,
            "name": "minimal",
            "prompt": "hi",
            "created_at": 0,
            "updated_at": 0
        }"#;
        let record: PromptRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.provider, "");
        assert_eq!(record.model, "");
        assert!(record.metadata.tags.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("save_load");
        let r = PromptRecord::new(
            "rt",
            "hello",
            "ollama",
            "llama3.2",
            PromptMetadata::default(),
            1_000,
        );
        let path = save(r.clone()).unwrap();
        assert!(path.ends_with("rt.json"));
        let loaded = load("rt").unwrap();
        assert_eq!(loaded.prompt, "hello");
        assert_eq!(loaded.provider, "ollama");
        assert_eq!(loaded.model, "llama3.2");
        assert_eq!(loaded.created_at, 1_000);
    }

    #[test]
    fn save_preserves_created_at_on_update() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("preserve_created");
        let r1 = PromptRecord::new(
            "upd",
            "first",
            "ollama",
            "m1",
            PromptMetadata::default(),
            100,
        );
        save(r1).unwrap();

        // Sleep briefly so updated_at differs (only on real filesystems).
        std::thread::sleep(std::time::Duration::from_millis(10));

        let r2 = PromptRecord {
            prompt: "second".into(),
            ..PromptRecord::new("upd", "", "", "", PromptMetadata::default(), 200)
        };
        save(r2).unwrap();

        let loaded = load("upd").unwrap();
        assert_eq!(loaded.created_at, 100, "created_at must survive update");
        assert!(loaded.updated_at >= 200, "updated_at must be refreshed");
        assert_eq!(loaded.prompt, "second");
    }

    #[test]
    fn list_returns_sorted_records() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("list_sorted");
        save(PromptRecord::new("z", "z", "", "", PromptMetadata::default(), 1)).unwrap();
        save(PromptRecord::new("a", "a", "", "", PromptMetadata::default(), 1)).unwrap();
        save(PromptRecord::new("m", "m", "", "", PromptMetadata::default(), 1)).unwrap();
        let names: Vec<String> = list().unwrap().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["a", "m", "z"]);
    }

    #[test]
    fn delete_removes_existing_file() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("delete_ok");
        save(PromptRecord::new("d", "x", "", "", PromptMetadata::default(), 1)).unwrap();
        assert!(delete("d").unwrap());
        assert!(matches!(load("d"), Err(PromptError::NotFound(_))));
    }

    #[test]
    fn delete_missing_returns_false() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("delete_missing");
        assert!(!delete("nope").unwrap());
    }

    #[test]
    fn load_missing_returns_not_found() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("load_missing");
        match load("does-not-exist") {
            Err(PromptError::NotFound(n)) => assert_eq!(n, "does-not-exist"),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_schema_rejected() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("unsupported_schema");
        // Hand-write a future-schema file.
        let dir = prompts_dir().unwrap();
        fs::write(
            dir.join("future.json"),
            r#"{
                "schema_version": 9999,
                "name": "future",
                "prompt": "x",
                "created_at": 0,
                "updated_at": 0
            }"#,
        )
        .unwrap();
        match load("future") {
            Err(PromptError::UnsupportedSchema {
                found, supported, ..
            }) => {
                assert_eq!(found, 9999);
                assert_eq!(supported, CURRENT_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchema, got {:?}", other),
        }
    }

    #[test]
    fn read_prompt_source_handles_both_file_and_literal() {
        // 1. Literal value passes through untouched.
        assert_eq!(read_prompt_source("hello world").unwrap(), "hello world");

        // 2. Existing file is read.
        let dir = std::env::temp_dir().join("magent_prompts_src");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("p.txt");
        fs::write(&path, "from-file\n").unwrap();
        let s = read_prompt_source(path.to_str().unwrap()).unwrap();
        assert_eq!(s, "from-file");
    }

    #[test]
    fn human_size_formats_both_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    // ------------------------------------------------------------------
    // `set-prompt import` — round-trip tests
    // ------------------------------------------------------------------

    /// End-to-end: build a JSON file on disk, import it, then load
    /// it back from the store and confirm the fields survived.
    #[test]
    fn import_writes_record_to_store() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_basic");
        // Write a hand-crafted JSON file in a temp dir.
        let dir = std::env::temp_dir().join("magent_import_basic");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("health_coach.json");
        let raw = serde_json::json!({
            "schema_version": 1,
            "name": "health_coach",
            "prompt": "You are a health coach.",
            "provider": "ollama",
            "model": "llama3.2",
            "metadata": {
                "description": "imported test prompt",
                "author": "ci",
                "tags": ["imported", "nrf52"]
            },
            "created_at": 1_700_000_000,
            "updated_at": 1_700_000_000
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let opts = SetPromptImportOptions {
            path: path.clone(),
            name: None,
            force: false,
        };
        SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap();

        let loaded = load("health_coach").unwrap();
        assert_eq!(loaded.prompt, "You are a health coach.");
        assert_eq!(loaded.provider, "ollama");
        assert_eq!(loaded.model, "llama3.2");
        assert_eq!(loaded.metadata.description.as_deref(), Some("imported test prompt"));
        assert_eq!(loaded.metadata.author.as_deref(), Some("ci"));
        assert_eq!(loaded.metadata.tags, vec!["imported".to_string(), "nrf52".to_string()]);
    }

    /// `--name` overrides the JSON's `name` field.
    #[test]
    fn import_renames_via_flag() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_rename");
        let dir = std::env::temp_dir().join("magent_import_rename");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("old_name.json");
        let raw = serde_json::json!({
            "schema_version": 1,
            "name": "old_name",
            "prompt": "x",
            "provider": "",
            "model": "",
            "metadata": {},
            "created_at": 0,
            "updated_at": 0
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let opts = SetPromptImportOptions {
            path,
            name: Some("renamed".to_string()),
            force: false,
        };
        SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap();
        // The new name is in the store; the old one is not.
        assert!(load("renamed").is_ok());
        assert!(matches!(load("old_name"), Err(PromptError::NotFound(_))));
    }

    /// Falls back to the file stem when neither `--name` nor the
    /// JSON's `name` field is present.
    #[test]
    fn import_falls_back_to_file_stem() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_stem");
        let dir = std::env::temp_dir().join("magent_import_stem");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("from_stem.json");
        // Note: empty `name` in the JSON.
        let raw = serde_json::json!({
            "schema_version": 1,
            "name": "",
            "prompt": "y",
            "provider": "",
            "model": "",
            "metadata": {},
            "created_at": 0,
            "updated_at": 0
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let opts = SetPromptImportOptions {
            path,
            name: None,
            force: false,
        };
        SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap();
        // File stem is `from_stem`, so the store now has that name.
        let loaded = load("from_stem").unwrap();
        assert_eq!(loaded.prompt, "y");
    }

    /// Refuses to overwrite an existing prompt without `--force`.
    #[test]
    fn import_refuses_overwrite_without_force() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_refuse");
        // Seed the store with a prompt called `existing`.
        save(PromptRecord::new(
            "existing",
            "first body",
            "",
            "",
            PromptMetadata::default(),
            1_000,
        ))
        .unwrap();
        // Build an import file with the same name.
        let dir = std::env::temp_dir().join("magent_import_refuse");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("existing.json");
        let raw = serde_json::json!({
            "schema_version": 1,
            "name": "existing",
            "prompt": "second body",
            "provider": "",
            "model": "",
            "metadata": {},
            "created_at": 0,
            "updated_at": 0
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let opts = SetPromptImportOptions {
            path: path.clone(),
            name: None,
            force: false,
        };
        let err = SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap_err();
        assert!(matches!(err, PromptError::Write { .. }), "got {:?}", err);
        // Original body is still there.
        let loaded = load("existing").unwrap();
        assert_eq!(loaded.prompt, "first body");
    }

    /// `--force` lets the import overwrite the existing record.
    #[test]
    fn import_with_force_overwrites() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_force");
        save(PromptRecord::new(
            "victim",
            "old",
            "",
            "",
            PromptMetadata::default(),
            1_000,
        ))
        .unwrap();
        let dir = std::env::temp_dir().join("magent_import_force");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("victim.json");
        let raw = serde_json::json!({
            "schema_version": 1,
            "name": "victim",
            "prompt": "new",
            "provider": "",
            "model": "",
            "metadata": {},
            "created_at": 0,
            "updated_at": 0
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let opts = SetPromptImportOptions {
            path,
            name: None,
            force: true,
        };
        SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap();
        let loaded = load("victim").unwrap();
        assert_eq!(loaded.prompt, "new");
    }

    /// Reading a non-existent file surfaces a `PromptFileLoad` error.
    #[test]
    fn import_missing_file_is_an_error() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("import_missing");
        let opts = SetPromptImportOptions {
            path: PathBuf::from("/no/such/file/anywhere.json"),
            name: None,
            force: false,
        };
        let err = SetPromptCmd::new(&SetPromptAction::Import(opts))
            .execute(&mut Output::new(OutputKind::Json, true))
            .unwrap_err();
        assert!(matches!(err, PromptError::PromptFileLoad { .. }), "got {:?}", err);
    }

    // ------------------------------------------------------------------
    // `render_template` unit tests
    // ------------------------------------------------------------------

    #[test]
    fn template_replaces_known_placeholders() {
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("name".to_string(), "Alice".to_string());
        vars.insert("device".to_string(), "nrf52".to_string());
        let out = render_template(
            "Hello {{name}}, your device is {{device}}.",
            &vars,
        );
        assert_eq!(out, "Hello Alice, your device is nrf52.");
    }

    #[test]
    fn template_leaves_unknown_placeholders_intact() {
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("a".to_string(), "X".to_string());
        let out = render_template("{{a}} {{b}}", &vars);
        // `a` resolves, `b` is left as-is.
        assert_eq!(out, "X {{b}}");
    }

    #[test]
    fn template_tolerates_whitespace_inside_braces() {
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("k".to_string(), "V".to_string());
        let out = render_template("{{ k }} and {{k}}", &vars);
        assert_eq!(out, "V and V");
    }

    #[test]
    fn template_with_no_placeholders_is_identity() {
        let vars = std::collections::BTreeMap::new();
        let out = render_template("plain text, no braces", &vars);
        assert_eq!(out, "plain text, no braces");
    }

    #[test]
    fn template_does_not_panic_on_unclosed_braces() {
        let vars = std::collections::BTreeMap::new();
        // Stray `{` and `}}` without matching pairs. The render
        // should be a verbatim copy, never a panic.
        let out = render_template("a { b }} c", &vars);
        assert_eq!(out, "a { b }} c");
    }

    #[test]
    fn template_preserves_unicode_content() {
        // UTF-8 multi-byte characters surrounding the placeholder
        // must round-trip safely. (The {{ ... }} syntax itself is
        // ASCII, but the surrounding text is not.)
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("name".to_string(), "张三".to_string());
        let out = render_template("用户 {{name}} 已登录", &vars);
        assert_eq!(out, "用户 张三 已登录");
    }

    #[test]
    fn template_with_empty_value_renders_to_empty() {
        // A variable bound to the empty string should still be
        // substituted (just to nothing). The common GitHub Actions
        // case.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("k".to_string(), String::new());
        let out = render_template("a{{k}}b", &vars);
        assert_eq!(out, "ab");
    }

    #[test]
    fn template_with_value_containing_braces() {
        // A substitution value that itself contains `{{` or `}}`
        // must NOT be re-processed. We do a single pass.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("x".to_string(), "{{y}}".to_string());
        let out = render_template("{{x}}", &vars);
        assert_eq!(out, "{{y}}");
    }

    #[test]
    fn template_with_empty_template() {
        let vars = std::collections::BTreeMap::new();
        assert_eq!(render_template("", &vars), "");
    }

    #[test]
    fn template_preserves_multibyte_chars() {
        // UTF-8 boundary handling: a placeholder that contains a
        // multi-byte codepoint must round-trip cleanly.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("city".to_string(), "苏黎世".to_string());
        let out = render_template("Hello {{city}}！", &vars);
        assert_eq!(out, "Hello 苏黎世！");
    }

    #[test]
    fn template_handles_emoji_in_placeholder_value() {
        // 4-byte UTF-8 codepoints (emoji) inside the substituted
        // value must not corrupt the surrounding output.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("e".to_string(), "🚀🎉".to_string());
        let out = render_template("[{{e}}]", &vars);
        assert_eq!(out, "[🚀🎉]");
    }

    #[test]
    fn template_repeated_placeholder_substitutes_each_time() {
        // The same `{{k}}` may appear multiple times. Every
        // occurrence must be substituted; the implementation
        // shouldn't accidentally only handle the first.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("x".to_string(), "42".to_string());
        let out = render_template("{{x}} - {{x}} - {{x}}", &vars);
        assert_eq!(out, "42 - 42 - 42");
    }

    #[test]
    fn template_with_warnings_reports_unfilled() {
        // A template that uses `{{known}}` and `{{unknown}}`
        // must render the unknown one inline, and *also* report
        // it via the warnings list so the CLI can surface it.
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("known".to_string(), "yes".to_string());
        let (rendered, warnings) =
            render_template_with_warnings("{{known}} and {{unknown}}", &vars);
        assert_eq!(rendered, "yes and {{unknown}}");
        assert_eq!(warnings, vec!["unknown".to_string()]);
    }

    #[test]
    fn template_with_warnings_dedupes_repeats() {
        // A template that uses `{{x}}` ten times should produce
        // one warning, not ten. The warning is "the user didn't
        // supply x", not "x appears 10 times".
        let vars = std::collections::BTreeMap::new();
        let (rendered, warnings) = render_template_with_warnings(
            "{{x}} {{x}} {{x}} {{x}} {{x}}",
            &vars,
        );
        assert_eq!(rendered, "{{x}} {{x}} {{x}} {{x}} {{x}}");
        assert_eq!(warnings, vec!["x".to_string()]);
    }

    #[test]
    fn template_with_warnings_handles_empty_template() {
        let (rendered, warnings) =
            render_template_with_warnings("", &std::collections::BTreeMap::new());
        assert_eq!(rendered, "");
        assert!(warnings.is_empty());
    }

    // ------------------------------------------------------------------
    // `set-prompt template` end-to-end
    // ------------------------------------------------------------------

    fn seed_prompt(name: &str, body: &str) {
        save(PromptRecord::new(
            name,
            body,
            "ollama",
            "llama3.2",
            PromptMetadata::default(),
            1_700_000_000,
        ))
        .unwrap();
    }

    #[test]
    fn template_substitutes_into_stdout() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_basic");
        seed_prompt(
            "greet",
            "Hello {{user}}, today is {{day}}.",
        );
        let opts = SetPromptTemplateOptions {
            name: "greet".to_string(),
            vars: vec![
                ("user".to_string(), "Bob".to_string()),
                ("day".to_string(), "Tuesday".to_string()),
            ],
            vars_from: None,
        };
        let mut out = Output::new(OutputKind::Human, true);
        SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut out)
            .unwrap();
    }

    #[test]
    fn template_reads_vars_from_file() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_from_file");
        seed_prompt("greet", "Hello {{name}}.");
        // Write a JSON file with variable bindings.
        let dir = std::env::temp_dir().join("magent_template_from_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("vars.json");
        std::fs::write(
            &path,
            r#"{"name": "From-File"}"#,
        )
        .unwrap();
        let opts = SetPromptTemplateOptions {
            name: "greet".to_string(),
            vars: Vec::new(),
            vars_from: Some(path),
        };
        // Just verify it doesn't error.
        let mut out = Output::new(OutputKind::Human, true);
        SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut out)
            .unwrap();
    }

    #[test]
    fn template_var_overrides_vars_from() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_override");
        seed_prompt("greet", "Hello {{name}}.");
        let dir = std::env::temp_dir().join("magent_template_override");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("vars.json");
        std::fs::write(&path, r#"{"name": "From-File"}"#).unwrap();
        let opts = SetPromptTemplateOptions {
            name: "greet".to_string(),
            // --var wins over --vars-from on conflict.
            vars: vec![("name".to_string(), "From-Flag".to_string())],
            vars_from: Some(path),
        };
        let mut out = Output::new(OutputKind::Json, true);
        SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut out)
            .unwrap();
    }

    #[test]
    fn template_unknown_prompt_is_an_error() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_missing");
        let opts = SetPromptTemplateOptions {
            name: "no_such_prompt".to_string(),
            vars: Vec::new(),
            vars_from: None,
        };
        let err = SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut Output::new(OutputKind::Human, true))
            .unwrap_err();
        assert!(matches!(err, PromptError::NotFound(_)));
    }

    #[test]
    fn template_vars_from_rejects_nested_object() {
        // A vars file with a nested object value (e.g. `{"auth":
        // {"user": "x"}}`) should be rejected with a clear error
        // rather than silently string-serialised to JSON.
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_nested");
        seed_prompt("greet", "Hello {{name}}.");
        let dir = std::env::temp_dir().join("magent_template_nested");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("vars.json");
        std::fs::write(&path, r#"{"name": "x", "auth": {"u": "y"}}"#).unwrap();
        let opts = SetPromptTemplateOptions {
            name: "greet".to_string(),
            vars: Vec::new(),
            vars_from: Some(path),
        };
        let err = SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut Output::new(OutputKind::Human, true))
            .unwrap_err();
        assert!(matches!(err, PromptError::Parse { .. }), "got {:?}", err);
    }

    #[test]
    fn template_vars_from_accepts_numbers_and_bools() {
        // Numbers / bools / null must be coerced to strings — they're
        // sensible template values when stringified.
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _tmp = TempPromptsDir::new("template_scalars");
        seed_prompt("greet", "v={{n}}, ok={{b}}");
        let dir = std::env::temp_dir().join("magent_template_scalars");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("vars.json");
        std::fs::write(&path, r#"{"n": 42, "b": true}"#).unwrap();
        let opts = SetPromptTemplateOptions {
            name: "greet".to_string(),
            vars: Vec::new(),
            vars_from: Some(path),
        };
        // Just verify it doesn't error.
        let mut out = Output::new(OutputKind::Human, true);
        SetPromptCmd::new(&SetPromptAction::Template(opts))
            .execute(&mut out)
            .unwrap();
    }
}
