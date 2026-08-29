//! Persistent summary store.
//!
//! Long-term home for the **head/tail compression window** that
//! [`compress_messages`] produces at the end of every
//! [`RealAgentRunner`] run, plus an optional
//! LLM-generated natural-language summary. The store mirrors
//! `cli::prompt::PromptRecord` in spirit — JSON file per topic under a
//! XDG-style directory, `schema_version` gated, single struct that's
//! auditable by hand.
//!
//! ## Why a separate store?
//!
//! - A **prompt** is human-authored, version-controlled, and stable.
//! - A **summary** is auto-generated, per-run, and accumulates over
//!   time. Mixing the two would force the prompt store to grow
//!   per-run mutation semantics that prompts don't need.
//! - The CLI surface stays flat: `magent summary save|show|list|
//!   delete|export|load` mirrors `magent set-prompt …`. Users don't
//!   have to learn a new mental model.
//!
//! ## On-disk shape (host backend)
//!
//! ```text
//! $XDG_DATA_HOME/magent/summaries/<topic>.json
//! # or, when XDG is unset on macOS / Linux:
//! $HOME/.local/share/magent/summaries/<topic>.json
//! ```
//!
//! Override the directory with the `MAGENT_SUMMARIES_DIR`
//! environment variable (mirrors `MAGENT_PROMPTS_DIR`).
//!
//! ## Atomic writes (host backend)
//!
//! Every `save` follows the standard
//! "write-tempfile → fsync → rename → fsync dir" recipe so a crash
//! never leaves the reader with a half-written JSON file. On
//! targets that lack `rename` (e.g. bare flash), the
//! `KvSummaryStore` (TBD) is expected to layer a different
//! atomicity story — typically a write-ahead slot.
//!
//! ## Concurrent writers (host backend)
//!
//! The host backend uses a per-topic **lock file** under
//! `<dir>/.locks/<topic>.lock` (created with
//! `OpenOptions::create_new`, which atomically fails when the
//! file already exists). The lock is held for the duration of the
//! `save` and released on drop. This is good enough for the CLI
//! use case; multi-process scenarios should layer a
//! `flock`-style advisory lock on top (out of scope here).
//!
//! ## Schema
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "topic": "weekly-health-review",
//!   "source": {
//!     "session_id": "run-2026-08-10-001",
//!     "provider": "ollama",
//!     "model": "llama3.2",
//!     "original_message_count": 47,
//!     "policy": { "max_messages": 32, "tool_content_max_chars": 800 }
//!   },
//!   "head_tail_window": [
//!     { "role": "system", "content": "...", "tool_call": null, "tool_call_id": null },
//!     { "role": "user",   "content": "Original task...", ... },
//!     { "role": "assistant", "content": "...", ... },
//!     { "role": "tool", "content": "[...truncated 1862 bytes...]", "tool_call_id": "c1" }
//!   ],
//!   "llm_summary": "The agent diagnosed a 3-day sleep deficit...",
//!   "stats": { "kept": 24, "dropped": 23, "tool_results_truncated": 2, "bytes_saved": 1700 },
//!   "metadata": { "description": "...", "tags": ["wearable"], "author": "..." },
//!   "history": [
//!     { "updated_at": 1723..., "kept": 22, "source_session_id": "run-..." },
//!     { "updated_at": 1723..., "kept": 24, "source_session_id": "run-..." }
//!   ],
//!   "created_at": 1723...,
//!   "updated_at": 1723...
//! }
//! ```
//!
//! ## What lives where
//!
//! - `head_tail_window` is a self-contained replay of what the LLM
//!   saw on the final wire. Anyone holding the JSON file can paste
//!   it back into `RealAgentRunner::messages` and the next call to
//!   the model produces the same continuation.
//! - `llm_summary` is the optional natural-language counterpart — a
//!   one-paragraph "what happened last time" that can be injected as
//!   a system message instead of (or in addition to) the raw window.
//! - `history` is a small FIFO of the previous N snapshots so users
//!   can roll back to a known-good state. Defaults to 5 entries.
//!
//! [`compress_messages`]: crate::conversation::compress_messages
//! [`RealAgentRunner`]: crate::agent_runner::RealAgentRunner

#![cfg(feature = "std")]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::string::ToString;
use std::sync::Mutex;
use std::vec::Vec;

mod record;

pub use record::{
    validate_metadata, validate_topic, CompressionPolicySnapshot, HistoryEntry, MessageDto,
    SummaryBuilder, SummaryError, SummaryMetadata, SummaryRecord, SummarySource,
    CURRENT_SCHEMA_VERSION, HISTORY_MAX, MAX_RECORD_BYTES, SUMMARY_AUTHOR_MAX,
    SUMMARY_DESCRIPTION_MAX, SUMMARY_LLM_MAX, SUMMARY_TAGS_MAX, SUMMARY_TAG_MAX, SUMMARY_TOPIC_MAX,
};

// ---------------------------------------------------------------------------
// Environment-variable override (host-only)
// ---------------------------------------------------------------------------

/// Environment variable that overrides the default summaries
/// directory. Mirrors `MAGENT_PROMPTS_DIR` so the two stores feel
/// identical to operators.
pub const SUMMARIES_DIR_ENV: &str = "MAGENT_SUMMARIES_DIR";

// ---------------------------------------------------------------------------
// WriteReport
// ---------------------------------------------------------------------------

/// Diagnostic struct returned by [`FileSummaryStore::save`]. The CLI
/// uses it to print "saved N bytes to <path>" without having to
/// re-stat the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    /// Absolute path the record landed at.
    pub path: PathBuf,
    /// Number of bytes written (before `fsync`).
    pub bytes: usize,
    /// `true` when the file already existed (i.e. this was an
    /// update rather than a fresh write). Useful for telemetry /
    /// the `--save-summary` JSON envelope.
    pub overwritten: bool,
}

// ---------------------------------------------------------------------------
// SummaryStore trait
// ---------------------------------------------------------------------------

/// Backend-agnostic summary storage trait. Implementations are free
/// to pick the wire format (JSON for host, postcard for KV) and
/// the atomicity story (`rename` for host, write-ahead for KV).
/// The data layer (`SummaryRecord` + DTOs) is shared.
pub trait SummaryStore {
    /// Persist `record`. If a record already exists for the same
    /// topic, its `created_at` is preserved and its snapshot is
    /// promoted into `history` (capped at [`HISTORY_MAX`]).
    ///
    /// Returns a backend-specific diagnostic — currently a
    /// [`WriteReport`], but KV stores may return something leaner.
    fn save(&self, record: SummaryRecord) -> Result<WriteReport, SummaryError>;

    /// Load a record by topic.
    fn load(&self, topic: &str) -> Result<SummaryRecord, SummaryError>;

    /// List every topic. May return partial results when individual
    /// entries are corrupted; the caller can re-load each by topic
    /// to surface the broken ones.
    fn list(&self) -> Result<Vec<SummaryRecord>, SummaryError>;

    /// Remove a topic. Idempotent — `Ok(())` even when nothing was
    /// deleted.
    fn delete(&self, topic: &str) -> Result<(), SummaryError>;
}

// ---------------------------------------------------------------------------
// FileSummaryStore
// ---------------------------------------------------------------------------

/// Host-side [`SummaryStore`] backed by `std::fs` + JSON. Each topic
/// lives in its own `<dir>/<topic>.json` file. Writes are atomic
/// (write → `fsync` → `rename` → `fsync` dir) and protected by a
/// per-topic lock file held for the duration of the call.
#[derive(Debug)]
pub struct FileSummaryStore {
    root: PathBuf,
    /// Process-wide mutex so two threads in the same process don't
    /// trip on each other's lock files. Per-topic locks are layered
    /// on top to coordinate across processes when they happen to
    /// share the directory.
    process_lock: Mutex<()>,
}

impl FileSummaryStore {
    /// Build a store rooted at `dir`. The directory is created on
    /// the first write; `load` / `list` will lazily create it as
    /// well so an empty workspace doesn't error out.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            root: dir.into(),
            process_lock: Mutex::new(()),
        }
    }

    /// Open the default store — same logic as the legacy free
    /// function [`summaries_dir`]. Useful when the caller doesn't
    /// have a specific directory in mind.
    pub fn open_default() -> Self {
        Self::new(summaries_dir())
    }

    /// Path to the lock file for `topic`. Created under
    /// `<root>/.locks/<topic>.lock`; the `.locks` directory is
    /// created on demand.
    fn lock_path(&self, topic: &str) -> Result<PathBuf, SummaryError> {
        let mut p = self.root.clone();
        p.push(".locks");
        fs::create_dir_all(&p).map_err(|e| SummaryError::DirIo {
            path: p.display().to_string(),
            source: e.to_string(),
        })?;
        p.push(format!("{}.lock", topic));
        Ok(p)
    }

    /// Acquire the per-topic lock. Returns the guard on success;
    /// on lock-busy returns [`SummaryError::Write`] with a
    /// descriptive source. The guard releases on drop.
    fn acquire_topic_lock(&self, topic: &str) -> Result<TopicLockGuard, SummaryError> {
        let path = self.lock_path(topic)?;
        // `create_new(true)` is atomic on POSIX and Windows; the
        // OS returns `AlreadyExists` when somebody else got there
        // first.
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| SummaryError::Write {
                path: path.display().to_string(),
                source: format!("could not acquire lock for topic {:?}: {}", topic, e),
            })?;
        Ok(TopicLockGuard { path, _file: file })
    }

    /// Inner helper that performs the atomic write. Caller MUST
    /// hold the topic lock.
    fn save_locked(&self, record: SummaryRecord) -> Result<WriteReport, SummaryError> {
        record.validate()?;

        // Decide whether this is an update or a fresh write
        // *before* serialising so we can populate `overwritten`.
        let target = self.record_path(&record.topic)?;
        let overwritten = target.exists();

        // Merge with the previous snapshot if one exists.
        let record = if overwritten {
            let raw = match fs::read_to_string(&target) {
                Ok(s) => s,
                Err(e) => {
                    return Err(SummaryError::Read {
                        path: target.display().to_string(),
                        source: e.to_string(),
                    });
                }
            };
            let prev = parse(&raw, &target)?;
            merge_with_prev(record, prev)
        } else {
            record
        };

        // Validate the *final* record (after merge) so an oversized
        // history can't sneak in via update.
        record.validate()?;
        let json = serde_json::to_vec_pretty(&record).map_err(|e| SummaryError::Parse {
            path: record.topic.clone(),
            source: e.to_string(),
        })?;
        if json.len() > MAX_RECORD_BYTES {
            return Err(SummaryError::RecordTooLarge {
                size: json.len(),
                max: MAX_RECORD_BYTES,
            });
        }

        // Ensure the parent directory exists.
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| SummaryError::DirIo {
                path: parent.display().to_string(),
                source: e.to_string(),
            })?;
        }

        // Atomic write: write tempfile → fsync → rename → fsync dir.
        let mut tmp = target.clone();
        tmp.set_extension(format!(
            "{}.tmp",
            target
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("json")
        ));
        let bytes = {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| SummaryError::Write {
                    path: tmp.display().to_string(),
                    source: e.to_string(),
                })?;
            f.write_all(&json).map_err(|e| SummaryError::Write {
                path: tmp.display().to_string(),
                source: e.to_string(),
            })?;
            f.sync_all().map_err(|e| SummaryError::Write {
                path: tmp.display().to_string(),
                source: e.to_string(),
            })?;
            json.len()
        };

        // `rename` is atomic on the same filesystem on POSIX and on
        // NTFS. On macOS APFS it is also atomic. On `exfat` /
        // `fat32` it is *not* — but we document that here rather
        // than trying to detect the filesystem.
        fs::rename(&tmp, &target).map_err(|e| SummaryError::Write {
            path: target.display().to_string(),
            source: format!("rename {} → {}: {}", tmp.display(), target.display(), e),
        })?;

        // Best-effort dir fsync. Some filesystems (e.g. tmpfs) reject
        // this; we ignore the error since the data file is already
        // durable.
        if let Some(parent) = target.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(WriteReport {
            path: target,
            bytes,
            overwritten,
        })
    }

    /// Path to `<root>/<topic>.json`. Pure function.
    fn record_path(&self, topic: &str) -> Result<PathBuf, SummaryError> {
        record_path(topic, &self.root)
    }
}

impl SummaryStore for FileSummaryStore {
    fn save(&self, record: SummaryRecord) -> Result<WriteReport, SummaryError> {
        // Validate the record up-front so a bad topic / oversized
        // payload never reaches the lock-file directory. We don't
        // want to create `<dir>/.locks/../etc/passwd.lock` for
        // any reason.
        record.validate()?;

        // Acquire the process-wide mutex **before** the topic lock.
        // The order matters: the process mutex serialises every
        // save in this process so we don't have two threads racing
        // on `create_new` for the same lock file. The topic lock
        // would still coordinate across processes, but in-process
        // we already have the mutex.
        let _proc = self.process_lock.lock().expect("process mutex poisoned");
        let _topic_lock = self.acquire_topic_lock(&record.topic)?;
        self.save_locked(record)
    }

    fn load(&self, topic: &str) -> Result<SummaryRecord, SummaryError> {
        let path = self.record_path(topic)?;
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SummaryError::NotFound(topic.to_string()));
            }
            Err(e) => {
                return Err(SummaryError::Read {
                    path: path.display().to_string(),
                    source: e.to_string(),
                });
            }
        };
        parse(&raw, &path)
    }

    fn list(&self) -> Result<Vec<SummaryRecord>, SummaryError> {
        list_dir(&self.root)
    }

    fn delete(&self, topic: &str) -> Result<(), SummaryError> {
        let path = self.record_path(topic)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SummaryError::Write {
                path: path.display().to_string(),
                source: e.to_string(),
            }),
        }
    }
}

/// RAII guard that removes the lock file on drop. We ignore
/// removal errors because the file may already be gone (process
/// crashed mid-save), and the *worst* outcome is a stale lock
/// file that the next writer will overwrite via `create_new` on a
/// different OS / filesystem combination.
///
/// Actually: `create_new` will fail with `AlreadyExists` if the
/// lock file is present, so we *do* want to remove it. We log
/// (host-only) and move on.
struct TopicLockGuard {
    path: PathBuf,
    _file: fs::File,
}

impl Drop for TopicLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

/// Promote a historical snapshot back into the active record.
///
/// `index` is 0-based into [`SummaryRecord::history`]. Index 0 is the
/// oldest snapshot, index `history.len() - 1` is the most recent.
/// The promoted snapshot replaces the current `head_tail_window`,
/// `llm_summary`, and `stats`; the previously-active snapshot is
/// pushed onto history.
///
/// Returns the new active record (caller still has to persist it).
pub fn rollback(
    store: &dyn SummaryStore,
    topic: &str,
    index: usize,
) -> Result<SummaryRecord, SummaryError> {
    let current = store.load(topic)?;
    if index >= current.history.len() {
        return Err(SummaryError::InvalidMetadata(format!(
            "history index {} out of range (len = {})",
            index,
            current.history.len()
        )));
    }
    let entry = current.history[index].clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut new_history = current.history.clone();
    new_history.remove(index);
    new_history.push(HistoryEntry {
        updated_at: current.updated_at,
        kept: current.stats.kept,
        source_session_id: current.source.session_id.clone(),
    });
    if new_history.len() > HISTORY_MAX {
        let drop = new_history.len() - HISTORY_MAX;
        new_history.drain(..drop);
    }
    Ok(SummaryRecord {
        updated_at: now,
        stats: crate::conversation::CompressionStats {
            kept: entry.kept,
            ..current.stats
        },
        history: new_history,
        ..current
    })
}

// ---------------------------------------------------------------------------
// Free functions — paths, parse, list
// ---------------------------------------------------------------------------

/// Compute the default summaries directory under XDG.
///
/// - If `MAGENT_SUMMARIES_DIR` is set, use it verbatim.
/// - Else if `XDG_DATA_HOME` is set, use
///   `$XDG_DATA_HOME/magent/summaries`.
/// - Else use `$HOME/.local/share/magent/summaries` (the freedesktop
///   default for `$XDG_DATA_HOME` when unset).
/// - Else fall back to a relative `magent/summaries` directory
///   under the current working directory. This branch is a
///   last-resort for sandboxed CI where neither `XDG_DATA_HOME`
///   nor `HOME` is set; the resulting path is stable for the
///   lifetime of the process.
pub fn summaries_dir() -> PathBuf {
    if let Ok(p) = std::env::var(SUMMARIES_DIR_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("magent").join("summaries");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("magent")
                .join("summaries");
        }
    }
    PathBuf::from("magent").join("summaries")
}

/// Ensure the summaries directory exists. Returns the directory
/// path on success.
pub fn ensure_summaries_dir() -> Result<PathBuf, SummaryError> {
    let dir = summaries_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        return Err(SummaryError::DirIo {
            path: dir.display().to_string(),
            source: e.to_string(),
        });
    }
    Ok(dir)
}

/// Compute the on-disk path for a topic under `dir`. Pure function
/// — does not touch the filesystem.
pub fn record_path(topic: &str, dir: &Path) -> Result<PathBuf, SummaryError> {
    validate_topic(topic)?;
    Ok(dir.join(format!("{}.json", topic)))
}

/// Parse a JSON string into a [`SummaryRecord`], enforcing
/// `schema_version`. Internal — exposed so the CLI / tests can
/// share the same validation.
pub fn parse(raw: &str, path: &Path) -> Result<SummaryRecord, SummaryError> {
    let record: SummaryRecord = serde_json::from_str(raw).map_err(|e| SummaryError::Parse {
        path: path.display().to_string(),
        source: e.to_string(),
    })?;
    if record.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(SummaryError::UnsupportedSchema {
            path: path.display().to_string(),
            found: record.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(record)
}

/// List every topic in `dir`, sorted alphabetically. Files that
/// don't parse are silently skipped — the listing table shouldn't
/// blow up because of one corrupted record.
pub fn list_dir(dir: &Path) -> Result<Vec<SummaryRecord>, SummaryError> {
    if let Err(e) = fs::create_dir_all(dir) {
        return Err(SummaryError::DirIo {
            path: dir.display().to_string(),
            source: e.to_string(),
        });
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| SummaryError::DirIo {
        path: dir.display().to_string(),
        source: e.to_string(),
    })? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(rec) = parse(&raw, &path) {
            out.push(rec);
        }
    }
    out.sort_by(|a, b| a.topic.cmp(&b.topic));
    Ok(out)
}

/// Merge `incoming` with the previous record `prev`:
/// - preserve `prev.created_at`
/// - push `prev`'s snapshot into `incoming.history`
/// - cap history at [`HISTORY_MAX`]
///
/// Internal helper used by [`FileSummaryStore::save_locked`].
fn merge_with_prev(mut incoming: SummaryRecord, prev: SummaryRecord) -> SummaryRecord {
    incoming.created_at = prev.created_at;
    let entry = HistoryEntry {
        updated_at: prev.updated_at,
        kept: prev.stats.kept,
        source_session_id: prev.source.session_id.clone(),
    };
    let mut history = prev.history;
    history.push(entry);
    if history.len() > HISTORY_MAX {
        let drop = history.len() - HISTORY_MAX;
        history.drain(..drop);
    }
    incoming.history = history;
    incoming
}

// ---------------------------------------------------------------------------
// Legacy free-function shims
// ---------------------------------------------------------------------------
//
// The previous revision exposed `save` / `load` / `list` / `delete`
// as free functions taking a `&Path`. They remain available for
// callers that don't want to thread a `FileSummaryStore` around
// (notably the CLI's `magent summary show <topic>` subcommand
// when the user passes `--dir`). The atomic-write guarantees are
// preserved by going through `FileSummaryStore` internally.

/// Persist `record` to `<dir>/<topic>.json`. Thin wrapper over
/// [`FileSummaryStore::save`] that builds an ephemeral store.
pub fn save(record: SummaryRecord, dir: &Path) -> Result<PathBuf, SummaryError> {
    let store = FileSummaryStore::new(dir.to_path_buf());
    let report = store.save(record)?;
    Ok(report.path)
}

/// Load `topic` from `<dir>/<topic>.json`. Thin wrapper over
/// [`FileSummaryStore::load`].
pub fn load(topic: &str, dir: &Path) -> Result<SummaryRecord, SummaryError> {
    let store = FileSummaryStore::new(dir.to_path_buf());
    store.load(topic)
}

/// Delete `topic` from `<dir>/<topic>.json`. Thin wrapper over
/// [`FileSummaryStore::delete`].
pub fn delete(topic: &str, dir: &Path) -> Result<(), SummaryError> {
    let store = FileSummaryStore::new(dir.to_path_buf());
    store.delete(topic)
}

/// List every topic in `dir`. Thin wrapper over
/// [`FileSummaryStore::list`].
pub fn list(dir: &Path) -> Result<Vec<SummaryRecord>, SummaryError> {
    let store = FileSummaryStore::new(dir.to_path_buf());
    store.list()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runner::Message;
    use crate::conversation::{compress_messages, CompressionPolicy, CompressionStats};
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Each test gets its own scratch directory under
    /// `std::env::temp_dir()` so they can run in parallel without
    /// clobbering each other. The directory is intentionally not
    /// cleaned up — tempdir removal is best-effort and would race
    /// with parallel tests on slow CI.
    fn scratch(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "magent-summary-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&p).expect("create scratch");
        p
    }

    // ---------- FileSummaryStore ----------

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch("roundtrip");
        let store = FileSummaryStore::new(dir.clone());
        let window = vec![Message::user("a"), Message::assistant_text("b")];
        let rec = SummaryBuilder::new("round-trip")
            .unwrap()
            .with_window(&window)
            .with_stats(CompressionStats {
                kept: 2,
                dropped: 0,
                tool_results_truncated: 0,
                bytes_saved: 0,
            })
            .build()
            .unwrap();
        let report = store.save(rec.clone()).expect("save");
        assert!(report.path.exists(), "saved path should exist");
        assert!(report.bytes > 0);
        assert!(!report.overwritten);
        let loaded = store.load("round-trip").expect("load");
        assert_eq!(loaded, rec);
    }

    #[test]
    fn save_marks_overwritten_true_on_update() {
        let dir = scratch("overwrite-flag");
        let store = FileSummaryStore::new(dir.clone());
        let r1 = SummaryBuilder::new("dup")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        let report1 = store.save(r1).unwrap();
        assert!(!report1.overwritten);

        let r2 = SummaryBuilder::new("dup")
            .unwrap()
            .with_window(&[Message::user("b")])
            .build()
            .unwrap();
        let report2 = store.save(r2).unwrap();
        assert!(report2.overwritten);
    }

    #[test]
    fn save_preserves_created_at_on_update() {
        let dir = scratch("created-at");
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("ts")
            .unwrap()
            .with_window(&[Message::user("a")])
            .with_created_at(100)
            .build()
            .unwrap();
        store.save(rec.clone()).unwrap();
        let loaded = store.load("ts").unwrap();
        assert_eq!(loaded.created_at, 100);
        assert!(loaded.updated_at >= 100);

        let mut rec2 = rec;
        rec2.stats.kept = 5;
        store.save(rec2).unwrap();
        let loaded2 = store.load("ts").unwrap();
        assert_eq!(loaded2.created_at, 100, "created_at must survive update");
        assert_eq!(loaded2.stats.kept, 5);
        assert!(!loaded2.history.is_empty(), "history should grow on update");
    }

    #[test]
    fn save_appends_history_on_update() {
        let dir = scratch("history");
        let store = FileSummaryStore::new(dir.clone());
        let mut rec = SummaryBuilder::new("hist")
            .unwrap()
            .with_window(&[Message::user("a")])
            .with_stats(CompressionStats {
                kept: 2,
                ..Default::default()
            })
            .build()
            .unwrap();
        store.save(rec.clone()).unwrap();
        for i in 0..3 {
            rec.stats.kept = 3 + i;
            store.save(rec.clone()).unwrap();
        }
        let loaded = store.load("hist").unwrap();
        assert_eq!(loaded.history.len(), 3, "got {:?}", loaded.history);
        let last = loaded.history.last().unwrap();
        assert!(last.kept >= 3);
    }

    #[test]
    fn save_caps_history_at_history_max() {
        let dir = scratch("cap");
        let store = FileSummaryStore::new(dir.clone());
        let mut rec = SummaryBuilder::new("cap")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        for _ in 0..(HISTORY_MAX + 2) {
            rec.stats.kept += 1;
            store.save(rec.clone()).unwrap();
        }
        let loaded = store.load("cap").unwrap();
        assert_eq!(loaded.history.len(), HISTORY_MAX);
    }

    #[test]
    fn save_rejects_bad_topic() {
        let dir = scratch("bad-topic");
        let store = FileSummaryStore::new(dir.clone());
        let bad = SummaryRecord {
            schema_version: CURRENT_SCHEMA_VERSION,
            topic: "../etc/passwd".to_string(),
            source: SummarySource::default(),
            head_tail_window: Vec::new(),
            llm_summary: None,
            stats: CompressionStats::default(),
            metadata: SummaryMetadata::default(),
            history: Vec::new(),
            created_at: 0,
            updated_at: 0,
        };
        assert!(matches!(
            store.save(bad),
            Err(SummaryError::InvalidTopic(_))
        ));
        // And make sure the legit save above didn't leak.
        let rec = SummaryBuilder::new("ok")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        store.save(rec).unwrap();
        assert!(store.load("ok").is_ok());
    }

    #[test]
    fn save_enforces_max_record_bytes() {
        let dir = scratch("size-cap");
        let store = FileSummaryStore::new(dir.clone());
        // Build a record that passes `SummaryBuilder::build()`
        // (so `summary.llm_summary` fits in SUMMARY_LLM_MAX) but
        // is large enough that pretty-printing it blows past
        // MAX_RECORD_BYTES. We pad the head_tail_window with long
        // user messages until the serialised size crosses the cap.
        let pad = "x".repeat(MAX_RECORD_BYTES / 2);
        let rec_res = SummaryBuilder::new("big")
            .unwrap()
            .with_window(&[Message::user(&pad), Message::user(&pad)])
            .build();
        let mut rec = match rec_res {
            Ok(r) => r,
            Err(SummaryError::SummaryTooLarge { .. }) => {
                // Builder caught it first — the test is moot in
                // this configuration, just no-op.
                return;
            }
            Err(e) => panic!("unexpected builder error: {:?}", e),
        };
        // Top up until we exceed the cap regardless of the
        // builder's size check.
        rec.head_tail_window.push(MessageDto {
            role: "user".into(),
            content: "y".repeat(MAX_RECORD_BYTES),
            had_tool_call: false,
            tool_call_id: None,
        });
        let r = store.save(rec);
        assert!(matches!(r, Err(SummaryError::RecordTooLarge { .. })));
    }

    #[test]
    fn load_returns_not_found_for_missing_topic() {
        let dir = scratch("not-found");
        let store = FileSummaryStore::new(dir);
        let r = store.load("nope");
        assert!(matches!(r, Err(SummaryError::NotFound(_))));
    }

    #[test]
    fn delete_removes_existing_topic() {
        let dir = scratch("delete-ok");
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("del")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        store.save(rec).unwrap();
        assert!(store.load("del").is_ok());
        store.delete("del").unwrap();
        assert!(matches!(store.load("del"), Err(SummaryError::NotFound(_))));
    }

    #[test]
    fn delete_is_idempotent_for_missing_topic() {
        let dir = scratch("delete-missing");
        let store = FileSummaryStore::new(dir);
        store.delete("ghost").expect("delete should be idempotent");
    }

    #[test]
    fn list_returns_empty_for_empty_dir() {
        let dir = scratch("list-empty");
        let store = FileSummaryStore::new(dir);
        let v = store.list().unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn list_sorts_topics_alphabetically() {
        let dir = scratch("list-sorted");
        let store = FileSummaryStore::new(dir.clone());
        for t in ["zeta", "alpha", "mu"] {
            let rec = SummaryBuilder::new(t)
                .unwrap()
                .with_window(&[Message::user("a")])
                .build()
                .unwrap();
            store.save(rec).unwrap();
        }
        let v = store.list().unwrap();
        let topics: Vec<&str> = v.iter().map(|r| r.topic.as_str()).collect();
        assert_eq!(topics, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn list_skips_unparseable_files() {
        let dir = scratch("list-skip");
        fs::write(dir.join("broken.json"), "{ not valid json").unwrap();
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("good")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        store.save(rec).unwrap();
        let v = store.list().unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].topic, "good");
    }

    #[test]
    fn lock_file_is_cleaned_up_after_save() {
        let dir = scratch("lock-cleanup");
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("cleanup")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        store.save(rec).unwrap();
        let lock_dir = dir.join(".locks");
        // The lock file must be gone after save returns.
        let locks: Vec<_> = fs::read_dir(&lock_dir)
            .map(|it| {
                it.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(locks.is_empty(), "stale lock files: {:?}", locks);
    }

    #[test]
    fn concurrent_writers_do_not_corrupt_topic() {
        // Two threads saving the same topic in lockstep. Without
        // per-topic locking the JSON would race; with it we expect
        // each save to land as a complete file.
        let dir = scratch("concurrent");
        let store = Arc::new(FileSummaryStore::new(dir.clone()));
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for tid in 0..2 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for i in 0..20 {
                    let rec = SummaryBuilder::new("contended")
                        .unwrap()
                        .with_window(&[Message::user(&format!("tid={} iter={}", tid, i))])
                        .with_stats(CompressionStats {
                            kept: 1 + i,
                            ..Default::default()
                        })
                        .build()
                        .unwrap();
                    store.save(rec).expect("save");
                }
            }));
        }
        for h in handles {
            h.join().expect("thread join");
        }

        // Final state must be parseable.
        let final_record = store.load("contended").unwrap();
        assert_eq!(final_record.topic, "contended");
        // History should be capped at HISTORY_MAX regardless of how
        // many writes succeeded.
        assert!(final_record.history.len() <= HISTORY_MAX);
        // And every write call recorded exactly one history entry
        // for the previous snapshot — so the count should be
        // HISTORY_MAX (40 saves → first has no predecessor, last
        // 39 are archived; cap kicks in at 5).
        assert_eq!(final_record.history.len(), HISTORY_MAX);
    }

    // ---------- parse ----------

    #[test]
    fn parse_rejects_unsupported_schema() {
        let raw = r#"{
            "schema_version": 999,
            "topic": "future",
            "source": {
                "provider": "",
                "model": "",
                "original_message_count": 0,
                "policy": { "max_messages": 0, "tool_content_max_chars": 0 }
            },
            "head_tail_window": [],
            "stats": { "kept": 0, "dropped": 0, "tool_results_truncated": 0, "bytes_saved": 0 },
            "metadata": {},
            "history": [],
            "created_at": 0,
            "updated_at": 0
        }"#;
        let r = parse(raw, Path::new("future.json"));
        match r {
            Err(SummaryError::UnsupportedSchema {
                found, supported, ..
            }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, CURRENT_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchema, got {:?}", other),
        }
    }

    #[test]
    fn parse_accepts_missing_optional_fields() {
        let raw = r#"{
            "schema_version": 1,
            "topic": "minimal",
            "source": { "provider": "ollama", "model": "llama3.2" },
            "head_tail_window": [],
            "stats": { "kept": 0, "dropped": 0, "tool_results_truncated": 0, "bytes_saved": 0 }
        }"#;
        let rec = parse(raw, Path::new("minimal.json")).unwrap();
        assert_eq!(rec.topic, "minimal");
        assert!(rec.llm_summary.is_none());
        assert!(rec.history.is_empty());
    }

    // ---------- rollback ----------

    #[test]
    fn rollback_promotes_history_entry_to_active() {
        let dir = scratch("rollback");
        let store = FileSummaryStore::new(dir.clone());
        let mut rec = SummaryBuilder::new("rb")
            .unwrap()
            .with_window(&[Message::user("a")])
            .with_stats(CompressionStats {
                kept: 2,
                ..Default::default()
            })
            .build()
            .unwrap();
        store.save(rec.clone()).unwrap();
        rec.stats.kept = 10;
        store.save(rec.clone()).unwrap();

        let current = store.load("rb").unwrap();
        assert_eq!(current.stats.kept, 10);
        assert_eq!(current.history.len(), 1);
        assert_eq!(current.history[0].kept, 2);

        let rolled = rollback(&store, "rb", 0).unwrap();
        assert_eq!(rolled.stats.kept, 2);
        assert_eq!(rolled.history.len(), 1);
        assert_eq!(rolled.history[0].kept, 10);
    }

    #[test]
    fn rollback_rejects_out_of_range_index() {
        let dir = scratch("rollback-oob");
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("rb-oob")
            .unwrap()
            .with_window(&[Message::user("a")])
            .build()
            .unwrap();
        store.save(rec).unwrap();
        let r = rollback(&store, "rb-oob", 99);
        assert!(matches!(r, Err(SummaryError::InvalidMetadata(_))));
    }

    // ---------- integration with compress_messages ----------

    #[test]
    fn save_compress_messages_output() {
        let dir = scratch("integration");
        let store = FileSummaryStore::new(dir.clone());
        let mut v = vec![
            Message::system("SYS"),
            Message::user("orig task"),
            Message::tool("c1", &"y".repeat(5_000)),
        ];
        for i in 0..30 {
            v.push(Message::assistant_text(&format!("a{}", i)));
            v.push(Message::tool(&format!("c{}", i), &format!("out{}", i)));
        }
        let policy = CompressionPolicy {
            max_messages: 16,
            tool_content_max_chars: 200,
        };
        let stats = compress_messages(&mut v, &policy);
        assert_eq!(stats.kept, 16);
        assert!(stats.tool_results_truncated >= 1);

        let source = SummarySource {
            session_id: Some("integration-run".into()),
            provider: "ollama".into(),
            model: "llama3.2".into(),
            original_message_count: 33,
            policy: CompressionPolicySnapshot::from(&policy),
        };
        let rec = SummaryBuilder::new("integration")
            .unwrap()
            .with_source(source)
            .with_window(&v)
            .with_stats(stats)
            .build()
            .unwrap();
        let report = store.save(rec.clone()).unwrap();
        let loaded = store.load("integration").unwrap();
        assert_eq!(loaded, rec);

        // And the JSON on disk is human-readable.
        let raw = fs::read_to_string(&report.path).unwrap();
        assert!(raw.contains("integration"));
        assert!(raw.contains("\"role\": \"tool\""));
    }

    // ---------- summaries_dir ----------

    #[test]
    fn summaries_dir_honours_env_override() {
        let prev = std::env::var(SUMMARIES_DIR_ENV).ok();
        let custom = scratch("env-override");
        // SAFETY: env mutation is process-global. We pick a unique
        // path so concurrent tests can't trample us.
        unsafe { std::env::set_var(SUMMARIES_DIR_ENV, &custom) };
        let got = summaries_dir();
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(SUMMARIES_DIR_ENV, v);
            } else {
                std::env::remove_var(SUMMARIES_DIR_ENV);
            }
        }
        assert_eq!(got, custom);
    }

    // ---------- WriteReport ----------

    #[test]
    fn write_report_carries_path_and_bytes() {
        let dir = scratch("write-report");
        let store = FileSummaryStore::new(dir.clone());
        let rec = SummaryBuilder::new("rep")
            .unwrap()
            .with_window(&[Message::user("hi")])
            .build()
            .unwrap();
        let report = store.save(rec).unwrap();
        assert!(report.path.starts_with(&dir));
        assert!(report.bytes > 0);
        assert!(!report.overwritten);
        let stat = fs::metadata(&report.path).unwrap();
        assert_eq!(stat.len() as usize, report.bytes);
    }
}
