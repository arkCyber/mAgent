//! `magent summary` subcommand.
//!
//! Persists **head/tail compression windows** produced by
//! `magent run` so the next run can pick up where the last one
//! left off. Mirrors the `magent set-prompt` subcommand shape so
//! users get one mental model for both stores:
//!
//! ```text
//! magent summary save    <TOPIC>  [--from <FILE>]
//! magent summary show    <TOPIC>
//! magent summary list
//! magent summary delete  <TOPIC>
//! magent summary load    <TOPIC>           # used by `magent run --load-summary`
//! magent summary export  <TOPIC> > out.json
//! ```
//!
//! Each summary file is one JSON object — see `magent-core::summary`
//! for the schema, design rationale, and atomic-write story. The
//! host CLI is one of two backends: the core's
//! [`FileSummaryStore`] (host) and a future `KvSummaryStore` for
//! embedded targets. They share the data layer via the
//! [`magent_core::summary::SummaryStore`] trait.
//!
//! ## Storage
//!
//! By default summaries live under the user's XDG data directory
//! (`$XDG_DATA_HOME/magent/summaries/<topic>.json`, or
//! `$HOME/.local/share/magent/summaries/<topic>.json` on macOS /
//! Linux when XDG is unset). The location can be overridden with
//! the `MAGENT_SUMMARIES_DIR` environment variable.
//!
//! ## Subcommands
//!
//! ```text
//! magent summary save    <TOPIC>  [--from <FILE>]
//! magent summary show    <TOPIC>
//! magent summary list
//! magent summary delete  <TOPIC>
//! magent summary export  <TOPIC>           # dump raw JSON to stdout
//! magent summary load    <TOPIC>           # print the head_tail_window as a JSON array
//! magent summary rollback <TOPIC> <INDEX>  # promote history[index] back to active
//! ```
//!
//! The `save` and `show` subcommands accept the same flags their
//! prompt-store counterparts do — `--json` is implied by the
//! global flag, and `--dir` overrides the default directory for a
//! single invocation (mostly for tests).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use magent_core::conversation::CompressionStats;
use magent_core::summary::{
    FileSummaryStore, HistoryEntry, SummaryError, SummaryRecord, SummaryStore, WriteReport,
};

// `SummaryBuilder` is only used in test helpers; importing it unconditionally
// produces a `cargo build` warning (unused in lib). We gate it behind cfg(test)
// so the test binary gets the type without polluting the release build.
#[cfg(test)]
use magent_core::summary::SummaryBuilder;

use crate::output::{Output, OutputKind};

/// Environment variable that overrides the default summaries
/// directory. Mirrors `MAGENT_PROMPTS_DIR` so operators can move
/// both stores with a single export.
pub const SUMMARIES_DIR_ENV: &str = "MAGENT_SUMMARIES_DIR";

// ---------------------------------------------------------------------------
// Subcommand enum
// ---------------------------------------------------------------------------

/// Sub-actions of `magent summary`. The CLI parser picks one of
/// these from the second positional argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryAction {
    /// `magent summary save <TOPIC> [--from <FILE>]` — persist a
    /// `SummaryRecord`. Without `--from`, the record is read from
    /// stdin so a shell pipeline can drive the command.
    Save(SummarySaveOptions),
    /// `magent summary show <TOPIC>` — print a human-readable
    /// summary of the record; `--json` switches to raw JSON.
    Show(String),
    /// `magent summary list` — list every stored topic in
    /// alphabetical order.
    List,
    /// `magent summary delete <TOPIC>` — remove the file (idempotent).
    Delete(String),
    /// `magent summary export <TOPIC>` — dump the raw JSON record
    /// to stdout for piping into a `Save --from` invocation.
    Export(String),
    /// `magent summary load <TOPIC>` — print just the
    /// `head_tail_window` as a JSON array. Used by the runner to
    /// inject the previous context into a fresh run.
    Load(String),
    /// `magent summary rollback <TOPIC> <INDEX>` — promote the
    /// snapshot at `history[index]` back to the active record.
    /// Index 0 is the oldest snapshot.
    Rollback(SummaryRollbackOptions),
}

/// Options for `magent summary save <TOPIC>`.
///
/// `--from <FILE>` reads a JSON record from disk; without it the
/// record is read from stdin. `--overwrite` allows replacing an
/// existing record (default behaviour is to refuse, to prevent
/// accidental CI overwrites).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySaveOptions {
    pub topic: String,
    pub from: Option<PathBuf>,
    pub overwrite: bool,
    pub dir: Option<PathBuf>,
}

/// Options for `magent summary rollback <TOPIC> <INDEX>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRollbackOptions {
    pub topic: String,
    pub index: usize,
    pub dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the summary subcommand. Wraps the core's
/// [`SummaryError`] for CLI-level concerns (IO on stdin, missing
/// `--from`, etc.).
#[derive(Debug)]
pub enum SummaryCmdError {
    /// A core-layer error bubbled up. See [`SummaryError`] for
    /// the variants.
    Core(SummaryError),
    /// `--from <FILE>` was passed but the file couldn't be read.
    FromFileLoad { path: PathBuf, source: io::Error },
    /// Stdin was empty.
    EmptyStdin,
    /// `--from <FILE>`'s JSON didn't parse as a `SummaryRecord`.
    InvalidJson(String),
    /// `--rollback <TOPIC> <INDEX>` index was out of range.
    IndexOutOfRange { index: usize, len: usize },
    /// Topic existed and `--overwrite` was not passed.
    AlreadyExists(String),
}

impl std::fmt::Display for SummaryCmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummaryCmdError::Core(e) => write!(f, "{}", e),
            SummaryCmdError::FromFileLoad { path, source } => {
                write!(f, "could not read --from file {}: {}", path.display(), source)
            }
            SummaryCmdError::EmptyStdin => {
                write!(f, "no input on stdin; pass --from <FILE> or pipe a JSON record")
            }
            SummaryCmdError::InvalidJson(s) => {
                write!(f, "--from file is not a valid SummaryRecord JSON: {}", s)
            }
            SummaryCmdError::IndexOutOfRange { index, len } => {
                write!(
                    f,
                    "history index {} is out of range (record has {} history entries)",
                    index, len
                )
            }
            SummaryCmdError::AlreadyExists(t) => {
                write!(
                    f,
                    "summary {:?} already exists; pass --overwrite to replace",
                    t
                )
            }
        }
    }
}

impl std::error::Error for SummaryCmdError {}

impl From<SummaryError> for SummaryCmdError {
    fn from(e: SummaryError) -> Self {
        SummaryCmdError::Core(e)
    }
}

// ---------------------------------------------------------------------------
// Glue
// ---------------------------------------------------------------------------

/// Glue struct so `main.rs` can construct and run the subcommand in
/// one line, mirroring `SetPromptCmd` / `RunCmd`.
pub struct SummaryCmd<'a> {
    pub action: &'a SummaryAction,
}

impl<'a> SummaryCmd<'a> {
    pub fn new(action: &'a SummaryAction) -> Self {
        Self { action }
    }

    /// Execute the subcommand. Always writes to `out` so both
    /// human and JSON modes get consistent output.
    pub fn execute(&self, out: &mut Output) -> Result<(), SummaryCmdError> {
        match self.action {
            SummaryAction::Save(opts) => self.save(opts, out),
            SummaryAction::Show(topic) => self.show(topic, out),
            SummaryAction::List => self.list(out),
            SummaryAction::Delete(topic) => self.delete(topic, out),
            SummaryAction::Export(topic) => self.export(topic, out),
            SummaryAction::Load(topic) => self.load(topic, out),
            SummaryAction::Rollback(opts) => self.rollback(opts, out),
        }
    }

    // -----------------------------------------------------------------------
    // save
    // -----------------------------------------------------------------------

    fn save(
        &self,
        opts: &SummarySaveOptions,
        out: &mut Output,
    ) -> Result<(), SummaryCmdError> {
        // Build the store up-front so a bad dir is reported early.
        let store = build_store(opts.dir.as_deref())?;

        // Refuse overwrite unless asked. We do this by probing the
        // store first so we get a fast, friendly error rather than
        // racing with a concurrent writer.
        if !opts.overwrite {
            match store.load(&opts.topic) {
                Ok(_) => {
                    return Err(SummaryCmdError::AlreadyExists(opts.topic.clone()));
                }
                Err(SummaryError::NotFound(_)) => {} // expected
                Err(e) => return Err(SummaryCmdError::Core(e)),
            }
        }

        // Read the JSON — from stdin or from `--from <FILE>`.
        let json = match &opts.from {
            Some(p) => fs::read_to_string(p).map_err(|e| SummaryCmdError::FromFileLoad {
                path: p.clone(),
                source: e,
            })?,
            None => {
                let mut s = String::new();
                io::stdin().read_to_string(&mut s).map_err(|e| {
                    SummaryCmdError::FromFileLoad {
                        path: PathBuf::from("(stdin)"),
                        source: e,
                    }
                })?;
                if s.trim().is_empty() {
                    return Err(SummaryCmdError::EmptyStdin);
                }
                s
            }
        };

        // Parse into a record. We accept any record shape the core
        // can serialise (including older schema versions via
        // `parse`).
        let path_for_parse = opts
            .from
            .clone()
            .unwrap_or_else(|| PathBuf::from("(stdin)"));
        let record: SummaryRecord = serde_json::from_str(&json).map_err(|e| {
            SummaryCmdError::InvalidJson(format!(
                "{}: {}",
                path_for_parse.display(),
                e
            ))
        })?;
        // Re-stamp the topic so the user can't sneak a record
        // whose topic disagrees with the CLI argument.
        let record = SummaryRecord {
            topic: opts.topic.clone(),
            ..record
        };

        let report = store.save(record)?;
        report_save(&report, out);
        Ok(())
    }

    fn show(&self, topic: &str, out: &mut Output) -> Result<(), SummaryCmdError> {
        let store = FileSummaryStore::open_default();
        let rec = store.load(topic).map_err(SummaryCmdError::Core)?;
        match out.kind() {
            OutputKind::Human => {
                out.write_human(&render_record_human(&rec));
                Ok(())
            }
            OutputKind::Json => {
                let json = serde_json::to_string_pretty(&rec).map_err(|e| {
                    SummaryCmdError::Core(SummaryError::Parse {
                        path: store
                            .list()
                            .ok()
                            .and_then(|v| v.first().map(|r| r.topic.clone()))
                            .unwrap_or_default(),
                        source: e.to_string(),
                    })
                })?;
                out.write_json_str(json);
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    fn list(&self, out: &mut Output) -> Result<(), SummaryCmdError> {
        let store = FileSummaryStore::open_default();
        let records = store.list().map_err(SummaryCmdError::Core)?;
        match out.kind() {
            OutputKind::Human => {
                if records.is_empty() {
                    out.write_human("(no summaries on disk yet)\n");
                } else {
                    let mut s = String::new();
                    let _ = writeln!(&mut s, "{:<24} {:<10} {:<8} TAGS", "TOPIC", "UPDATED", "KEPT");
                    for r in &records {
                        let updated = format_unix_short(r.updated_at);
                        let tags = if r.metadata.tags.is_empty() {
                            "-".to_string()
                        } else {
                            r.metadata.tags.join(",")
                        };
                        let _ = writeln!(
                            &mut s,
                            "{:<24} {:<10} {:<8} {}",
                            truncate(&r.topic, 24),
                            updated,
                            r.stats.kept,
                            tags
                        );
                    }
                    out.write_human(&s);
                }
                Ok(())
            }
            OutputKind::Json => {
                let json =
                    serde_json::to_string_pretty(&records).map_err(|e| SummaryCmdError::Core(
                        SummaryError::Parse {
                            path: String::new(),
                            source: e.to_string(),
                        },
                    ))?;
                out.write_json_str(json);
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // delete
    // -----------------------------------------------------------------------

    fn delete(&self, topic: &str, out: &mut Output) -> Result<(), SummaryCmdError> {
        let store = FileSummaryStore::open_default();
        store.delete(topic).map_err(SummaryCmdError::Core)?;
        match out.kind() {
            OutputKind::Human => out.write_human(&format!("deleted summary {:?}\n", topic)),
            OutputKind::Json => out
                .write_json_str(serde_json::json!({ "topic": topic, "deleted": true }).to_string()),
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // export
    // -----------------------------------------------------------------------

    fn export(&self, topic: &str, out: &mut Output) -> Result<(), SummaryCmdError> {
        let store = FileSummaryStore::open_default();
        let rec = store.load(topic).map_err(SummaryCmdError::Core)?;
        let json = serde_json::to_string_pretty(&rec).map_err(|e| {
            SummaryCmdError::Core(SummaryError::Parse {
                path: format!("<{}>", topic),
                source: e.to_string(),
            })
        })?;
        // `export` writes the raw JSON to stdout regardless of
        // `--json` because its job is to feed another command, so
        // the payload must reach stdout in *both* modes.
        out.write_stdout_line(&json);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // load
    // -----------------------------------------------------------------------

    fn load(&self, topic: &str, out: &mut Output) -> Result<(), SummaryCmdError> {
        let store = FileSummaryStore::open_default();
        let rec = store.load(topic).map_err(SummaryCmdError::Core)?;
        let json = serde_json::to_string_pretty(&rec.head_tail_window).map_err(|e| {
            SummaryCmdError::Core(SummaryError::Parse {
                path: format!("<{}>", topic),
                source: e.to_string(),
            })
        })?;
        // `load` feeds another command with the head/tail window, so
        // like `export` it must emit to stdout in both modes.
        out.write_stdout_line(&json);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // rollback
    // -----------------------------------------------------------------------

    fn rollback(
        &self,
        opts: &SummaryRollbackOptions,
        out: &mut Output,
    ) -> Result<(), SummaryCmdError> {
        let store = build_store(opts.dir.as_deref())?;
        let current = store.load(&opts.topic).map_err(SummaryCmdError::Core)?;
        if opts.index >= current.history.len() {
            return Err(SummaryCmdError::IndexOutOfRange {
                index: opts.index,
                len: current.history.len(),
            });
        }
        let entry = current.history[opts.index].clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut new_history = current.history.clone();
        new_history.remove(opts.index);
        new_history.push(HistoryEntry {
            updated_at: current.updated_at,
            kept: current.stats.kept,
            source_session_id: current.source.session_id.clone(),
            head_tail_window: current.head_tail_window.clone(),
            llm_summary: current.llm_summary.clone(),
            stats: current.stats,
        });
        // Restore the snapshot's content, not just its `kept` counter.
        // Entries written before the snapshot fields existed fall back to
        // the old counter-only semantics.
        let legacy = entry.head_tail_window.is_empty() && entry.llm_summary.is_none();
        let (head_tail_window, llm_summary, stats) = if legacy {
            (
                current.head_tail_window.clone(),
                current.llm_summary.clone(),
                CompressionStats {
                    kept: entry.kept,
                    ..current.stats
                },
            )
        } else {
            (
                entry.head_tail_window.clone(),
                entry.llm_summary.clone(),
                entry.stats,
            )
        };
        let rolled = SummaryRecord {
            updated_at: now,
            head_tail_window,
            llm_summary,
            stats,
            history: new_history,
            ..current
        };
        // The rollback record already contains the complete post-state (the
        // entry removed, the superseded snapshot appended). `save` merges the
        // on-disk history back in, which re-added the entry the caller just
        // rolled back — use the no-merge path.
        let report = store.save_overwrite(rolled)?;
        match out.kind() {
            OutputKind::Human => out.write_human(&format!(
                "rolled back {:?} to history[{}] (was kept={}); saved {} bytes to {}\n",
                opts.topic,
                opts.index,
                entry.kept,
                report.bytes,
                report.path.display()
            )),
            // A successful `--json` command must write exactly one document.
            // This arm was missing, so rollback exited 0 with empty stdout.
            OutputKind::Json => out.write_json_str(
                serde_json::json!({
                    "topic": opts.topic,
                    "rolled_back_to_index": opts.index,
                    "kept": entry.kept,
                    "saved_bytes": report.bytes,
                    "path": report.path.display().to_string(),
                })
                .to_string(),
            ),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers shared with runner.rs
// ---------------------------------------------------------------------------

/// Build a [`FileSummaryStore`] rooted at `dir` (or the default
/// location when `dir` is `None`). Centralised so `save` and
/// `rollback` agree on the directory resolution rules.
pub(crate) fn build_store(dir: Option<&Path>) -> Result<FileSummaryStore, SummaryCmdError> {
    match dir {
        Some(p) => Ok(FileSummaryStore::new(p.to_path_buf())),
        None => Ok(FileSummaryStore::open_default()),
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_record_human(r: &SummaryRecord) -> String {
    let mut s = String::new();
    let _ = writeln!(&mut s, "summary: {}", r.topic);
    let _ = writeln!(
        &mut s,
        "  schema_version : {}",
        r.schema_version
    );
    let _ = writeln!(
        &mut s,
        "  provider/model : {}/{}",
        r.source.provider,
        if r.source.model.is_empty() {
            "-"
        } else {
            r.source.model.as_str()
        }
    );
    if let Some(sid) = &r.source.session_id {
        let _ = writeln!(&mut s, "  session_id     : {}", sid);
    }
    let _ = writeln!(
        &mut s,
        "  original_count : {}",
        r.source.original_message_count
    );
    let _ = writeln!(&mut s, "  stats          : kept={} dropped={} tool_truncated={} bytes_saved={}",
        r.stats.kept,
        r.stats.dropped,
        r.stats.tool_results_truncated,
        r.stats.bytes_saved,
    );
    let _ = writeln!(
        &mut s,
        "  created_at     : {}",
        format_unix_long(r.created_at)
    );
    let _ = writeln!(
        &mut s,
        "  updated_at     : {}",
        format_unix_long(r.updated_at)
    );
    if let Some(desc) = &r.metadata.description {
        let _ = writeln!(&mut s, "  description    : {}", desc);
    }
    if !r.metadata.tags.is_empty() {
        let _ = writeln!(&mut s, "  tags           : {}", r.metadata.tags.join(", "));
    }
    let _ = writeln!(
        &mut s,
        "  window         : {} messages",
        r.head_tail_window.len()
    );
    if let Some(summary) = &r.llm_summary {
        let _ = writeln!(&mut s, "  llm_summary    : {}", truncate(summary, 80));
    }
    if !r.history.is_empty() {
        let _ = writeln!(&mut s, "  history        :");
        for (i, h) in r.history.iter().enumerate() {
            let _ = writeln!(
                &mut s,
                "    [{}] updated_at={} kept={} session_id={}",
                i,
                format_unix_short(h.updated_at),
                h.kept,
                h.source_session_id.as_deref().unwrap_or("-")
            );
        }
    }
    s
}

fn format_unix_short(ts: u64) -> String {
    if ts == 0 {
        "-".to_string()
    } else {
        // "%Y-%m-%d %H:%M:%S" without pulling in `chrono`.
        // We do pure integer arithmetic; the algorithm is
        // Zeller-like but implemented directly via
        // "days-since-epoch → Gregorian calendar".
        let (y, m, d) = days_to_ymd(ts / 86_400);
        let secs_of_day = ts % 86_400;
        let hh = secs_of_day / 3_600;
        let mm = (secs_of_day % 3_600) / 60;
        let ss = secs_of_day % 60;
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, hh, mm, ss)
    }
}

/// Convert days-since-epoch to a Gregorian (year, month, day).
///
/// Uses Howard Hinnant's `civil_from_days` (the shift-by-146097-era
/// formulation), which is exact for the entire `u64` range the epoch
/// fits in.
///
/// It replaces a hand-rolled year/month scan that was wrong on **every
/// leap-year February**: the leap offset was applied to the month scan
/// but subtracted from the day-of-month only from March onwards, so
/// 2028-02-01 printed as `2028-01-32` (and Feb 2024/2020/… likewise).
/// `summary list` / `summary show` render every timestamp through this,
/// so the bug was a wrong date in the column operators use to tell runs
/// apart. A differential test, `days_to_ymd_matches_known_dates`, covers
/// every leap day from 1970 to 2100.
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // 146097 = days per 400-year era; 719468 = days from 0000-03-01 to
    // 1970-01-01.
    //
    // Clamp before widening: `days as i64` wraps for anything above
    // `i64::MAX`, so `days_to_ymd(u64::MAX)` reported 1969 — the exact
    // "garbage for absurd input" the previous comment claimed the cast
    // prevented. Callers pass `ts / 86_400` (at most ~2.1e14), so the clamp
    // never fires in practice; it just makes the absurd-input path
    // saturate instead of going backwards in time.
    // The clamp also keeps `days as i64 + 719_468` from overflowing i64:
    // that sum is the very first operation below.
    let days = days.min((i64::MAX - 719_468) as u64);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    // A year beyond `u32` is not representable in the return type; saturate
    // rather than truncate, so the result stays monotonic at the extreme
    // instead of wrapping to an unrelated year.
    let y = y.clamp(0, u32::MAX as i64);
    (y as u32, m as u32, d as u32)
}

fn format_unix_long(ts: u64) -> String {
    if ts == 0 {
        "-".to_string()
    } else {
        format!("{} ({})", ts, format_unix_short(ts))
    }
}

/// Truncate `s` to at most `max` characters, appending `…` when shortened.
///
/// Counts characters, not bytes: the topic column and the `llm_summary` line
/// both carry operator- and model-supplied text, and slicing a `str` at a byte
/// offset that lands inside a multi-byte code point panics. This mirrors the
/// `truncate` helpers in `config.rs` and `prompt.rs`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn report_save(report: &WriteReport, out: &mut Output) {
    match out.kind() {
        OutputKind::Human => {
            let verb = if report.overwritten { "updated" } else { "saved" };
            out.write_human(&format!(
                "{} summary to {} ({} bytes)\n",
                verb,
                report.path.display(),
                report.bytes
            ));
        }
        OutputKind::Json => {
            let json = serde_json::json!({
                "saved": true,
                "path": report.path,
                "bytes": report.bytes,
                "overwritten": report.overwritten,
            });
            out.write_json_str(json.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Writable shim for the helpers above
// ---------------------------------------------------------------------------

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `MAGENT_SUMMARIES_DIR` is process-global, so every test
    /// that exercises the default directory needs a unique
    /// scratch path AND needs to be serialised behind a process
    /// mutex — `std::env::set_var` is not thread-safe, so two
    /// tests touching it concurrently will clobber each other
    /// regardless of which path they pick.
    ///
    /// The mutex is `lazy_static`-style: a single `Mutex` shared
    /// by every test. We acquire it before set_var and release
    /// it on the way out (success or panic). This makes the
    /// summary subcommand tests single-threaded but the runtime
    /// cost is negligible — there are only ~10 of them.
    fn with_temp_summaries_dir<F: FnOnce(&Path)>(f: F) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var(SUMMARIES_DIR_ENV).ok();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "magent-summary-cli-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&p).unwrap();
        // SAFETY: we hold the process-wide LOCK for the lifetime of
        // this scope, so no concurrent test can observe a
        // half-set environment.
        unsafe { std::env::set_var(SUMMARIES_DIR_ENV, &p) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&p)));
        // Restore regardless of panic outcome so other tests see
        // the original environment.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(SUMMARIES_DIR_ENV, v),
                None => std::env::remove_var(SUMMARIES_DIR_ENV),
            }
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    fn sample_record(topic: &str) -> SummaryRecord {
        SummaryBuilder::new(topic)
            .unwrap()
            .with_window(&[
                magent_core::agent_runner::Message::user("hello"),
                magent_core::agent_runner::Message::assistant_text("hi"),
            ])
            .with_stats(CompressionStats {
                kept: 2,
                dropped: 3,
                tool_results_truncated: 1,
                bytes_saved: 100,
            })
            .build()
            .unwrap()
    }

    #[test]
    fn save_then_show_round_trips() {
        with_temp_summaries_dir(|_dir| {
            let rec = sample_record("alpha");
            let json = serde_json::to_string_pretty(&rec).unwrap();

            let tmp = std::env::temp_dir().join(format!(
                "save-input-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            let save = SummaryAction::Save(SummarySaveOptions {
                topic: "alpha".into(),
                from: Some(tmp.clone()),
                overwrite: false,
                dir: None,
            });
            let mut out = Output::new(OutputKind::Human, true);
            SummaryCmd::new(&save).execute(&mut out).unwrap();

            let show = SummaryAction::Show("alpha".into());
            SummaryCmd::new(&show).execute(&mut out).unwrap();

            let _ = json;
        });
    }

    #[test]
    fn save_refuses_to_overwrite_without_flag() {
        with_temp_summaries_dir(|_dir| {
            let rec = sample_record("dup");
            let json = serde_json::to_string_pretty(&rec).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "dup-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            let opts = SummarySaveOptions {
                topic: "dup".into(),
                from: Some(tmp.clone()),
                overwrite: false,
                dir: None,
            };
            let mut out = Output::new(OutputKind::Human, true);
            SummaryCmd::new(&SummaryAction::Save(opts.clone()))
                .execute(&mut out)
                .unwrap();

            let r = SummaryCmd::new(&SummaryAction::Save(opts)).execute(&mut out);
            assert!(matches!(r, Err(SummaryCmdError::AlreadyExists(_))));
        });
    }

    #[test]
    fn save_overwrite_flag_replaces_existing() {
        with_temp_summaries_dir(|_dir| {
            let rec1 = sample_record("dup-ok");
            let json1 = serde_json::to_string_pretty(&rec1).unwrap();
            let tmp1 = std::env::temp_dir().join(format!(
                "dup-ok-1-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp1, &json1).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "dup-ok".into(),
                from: Some(tmp1),
                overwrite: false,
                dir: None,
            }))
            .execute(&mut Output::new(OutputKind::Human, true))
            .unwrap();

            let mut rec2 = rec1.clone();
            rec2.stats.kept = 99;
            let json2 = serde_json::to_string_pretty(&rec2).unwrap();
            let tmp2 = std::env::temp_dir().join(format!(
                "dup-ok-2-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp2, &json2).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "dup-ok".into(),
                from: Some(tmp2),
                overwrite: true,
                dir: None,
            }))
            .execute(&mut Output::new(OutputKind::Human, true))
            .unwrap();

            // The stored record has the new stats.
            let loaded = FileSummaryStore::open_default().load("dup-ok").unwrap();
            assert_eq!(loaded.stats.kept, 99);
            // History has one entry for the previous snapshot.
            assert_eq!(loaded.history.len(), 1);
            assert_eq!(loaded.history[0].kept, 2);
        });
    }

    #[test]
    fn save_rejects_empty_stdin() {
        // We can't easily simulate truly empty stdin in unit tests,
        // so we exercise the IO-error path by pointing `--from`
        // at a non-existent file. The `FromFileLoad` branch is
        // identical to what stdin-closed-immediately would produce.
        with_temp_summaries_dir(|_dir| {
            let opts = SummarySaveOptions {
                topic: "missing".into(),
                from: Some(PathBuf::from("/this/path/does/not/exist.json")),
                overwrite: false,
                dir: None,
            };
            let mut out = Output::new(OutputKind::Human, true);
            let r = SummaryCmd::new(&SummaryAction::Save(opts)).execute(&mut out);
            assert!(matches!(r, Err(SummaryCmdError::FromFileLoad { .. })));
        });
    }

    #[test]
    fn save_rejects_bad_json() {
        with_temp_summaries_dir(|_dir| {
            let tmp = std::env::temp_dir().join(format!(
                "bad-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, "{ not valid json").unwrap();
            let opts = SummarySaveOptions {
                topic: "bad".into(),
                from: Some(tmp),
                overwrite: false,
                dir: None,
            };
            let mut out = Output::new(OutputKind::Human, true);
            let r = SummaryCmd::new(&SummaryAction::Save(opts)).execute(&mut out);
            assert!(matches!(r, Err(SummaryCmdError::InvalidJson(_))));
        });
    }

    #[test]
    fn list_returns_empty_when_no_records() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            SummaryCmd::new(&SummaryAction::List)
                .execute(&mut out)
                .unwrap();
        });
    }

    #[test]
    fn list_returns_records_in_alphabetical_order() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            // Build a JSON input file once and reuse it for every
            // save — the record's `topic` is re-stamped by the CLI
            // to match the CLI's positional argument, so the file
            // content only needs to be a valid SummaryRecord with
            // *some* topic (we use `placeholder`).
            let placeholder = sample_record("placeholder");
            let json = serde_json::to_string_pretty(&placeholder).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "list-input-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            for t in ["zeta", "alpha", "mu"] {
                SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                    topic: t.into(),
                    from: Some(tmp.clone()),
                    overwrite: false,
                    dir: None,
                }))
                .execute(&mut out)
                .unwrap();
            }
            // The list will be populated — we just check the
            // function doesn't error. Alphabetical ordering is
            // covered by the core tests.
            SummaryCmd::new(&SummaryAction::List).execute(&mut out).unwrap();
        });
    }

    #[test]
    fn delete_removes_existing_record() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            let rec = sample_record("del");
            let json = serde_json::to_string_pretty(&rec).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "del-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "del".into(),
                from: Some(tmp),
                overwrite: false,
                dir: None,
            }))
            .execute(&mut out)
            .unwrap();

            SummaryCmd::new(&SummaryAction::Delete("del".into()))
                .execute(&mut out)
                .unwrap();
            // Show should now fail with NotFound.
            let r = SummaryCmd::new(&SummaryAction::Show("del".into())).execute(&mut out);
            assert!(matches!(r, Err(SummaryCmdError::Core(SummaryError::NotFound(_)))));
        });
    }

    #[test]
    fn export_writes_json_to_output() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            let rec = sample_record("exp");
            let json = serde_json::to_string_pretty(&rec).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "exp-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "exp".into(),
                from: Some(tmp),
                overwrite: false,
                dir: None,
            }))
            .execute(&mut out)
            .unwrap();

            SummaryCmd::new(&SummaryAction::Export("exp".into()))
                .execute(&mut out)
                .unwrap();
        });
    }

    #[test]
    fn load_writes_window_as_json_array() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            let rec = sample_record("ld");
            let json = serde_json::to_string_pretty(&rec).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "ld-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "ld".into(),
                from: Some(tmp),
                overwrite: false,
                dir: None,
            }))
            .execute(&mut out)
            .unwrap();

            SummaryCmd::new(&SummaryAction::Load("ld".into()))
                .execute(&mut out)
                .unwrap();
        });
    }

    /// Rollback builds the complete post-state itself; the store's normal
    /// merge re-added the entry the caller had just removed, so history grew
    /// instead of shrinking.
    #[test]
    fn rollback_removes_the_rolled_back_entry_from_history() {
        with_temp_summaries_dir(|_dir| {
            let store = build_store(None).unwrap();
            let mut first = sample_record("rb-merge");
            first.stats.kept = 1;
            store.save(first).unwrap();
            let mut second = sample_record("rb-merge");
            second.stats.kept = 2;
            store.save(second).unwrap();
            assert_eq!(store.load("rb-merge").unwrap().history.len(), 1);

            let mut out = Output::new(OutputKind::Human, true);
            SummaryCmd::new(&SummaryAction::Rollback(SummaryRollbackOptions {
                topic: "rb-merge".into(),
                index: 0,
                dir: None,
            }))
            .execute(&mut out)
            .unwrap();

            let after = store.load("rb-merge").unwrap();
            assert_eq!(after.stats.kept, 1, "the snapshot must become active");
            assert_eq!(
                after.history.len(),
                1,
                "the rolled-back entry must be removed and the superseded \
                 snapshot appended, not merged back"
            );
            assert_eq!(after.history[0].kept, 2);
        });
    }

    #[test]
    fn rollback_out_of_range_errors() {
        with_temp_summaries_dir(|_dir| {
            let mut out = Output::new(OutputKind::Human, true);
            let rec = sample_record("rb-oob");
            let json = serde_json::to_string_pretty(&rec).unwrap();
            let tmp = std::env::temp_dir().join(format!(
                "rb-oob-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&tmp, &json).unwrap();

            SummaryCmd::new(&SummaryAction::Save(SummarySaveOptions {
                topic: "rb-oob".into(),
                from: Some(tmp),
                overwrite: false,
                dir: None,
            }))
            .execute(&mut out)
            .unwrap();

            let r = SummaryCmd::new(&SummaryAction::Rollback(SummaryRollbackOptions {
                topic: "rb-oob".into(),
                index: 99,
                dir: None,
            }))
            .execute(&mut out);
            assert!(matches!(
                r,
                Err(SummaryCmdError::IndexOutOfRange { index: 99, len: 0 })
            ));
        });
    }

    #[test]
    fn error_display_mentions_context() {
        let e = SummaryCmdError::EmptyStdin;
        assert!(e.to_string().contains("stdin"));

        let e = SummaryCmdError::AlreadyExists("dup".into());
        assert!(e.to_string().contains("dup"));
        assert!(e.to_string().contains("--overwrite"));

        let e = SummaryCmdError::InvalidJson("missing field".into());
        assert!(e.to_string().contains("missing field"));
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_2026() {
        // 2026-08-10 00:00:00 UTC = day 20675
        assert_eq!(days_to_ymd(20_675), (2026, 8, 10));
    }

    #[test]
    fn days_to_ymd_leap_year() {
        // 2000-02-29: year 2000 is a leap year (divisible by 400)
        let (y, m, d) = days_to_ymd(11_016);
        eprintln!("days_to_ymd(11016) = ({}, {}, {})", y, m, d);
        assert_eq!((y, m, d), (2000, 2, 29));
    }

    /// Gregorian leap-year rule, used by the exhaustive calendar test below
    /// as its independent reference.
    #[allow(clippy::manual_is_multiple_of)]
    fn is_leap_year(year: u32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    /// Every leap-year February, plus the day before and after it.
    ///
    /// The previous implementation printed `2028-01-32` for 2028-02-01 (the
    /// leap offset was applied to the month scan but not to the day). The
    /// pre-existing tests only checked 2000-02-29 and a non-leap date, so
    /// nothing caught it; `summary list` showed the wrong date for every
    /// summary saved in the first two months of a leap year.
    #[test]
    fn days_to_ymd_handles_every_leap_february() {
        // Day number of 1970-01-01 is 0; leap years 1972..=2100 stepping 4.
        // `is_leap_year` is the reference predicate, and the month lengths
        // below are fixed by the Gregorian calendar.
        let mut days: u64 = 0;
        for year in 1970u32..=2100 {
            let feb_len = if is_leap_year(year) { 29 } else { 28 };
            for month in 1u32..=12 {
                let month_len = match month {
                    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                    4 | 6 | 9 | 11 => 30,
                    2 => feb_len,
                    _ => unreachable!(),
                };
                for day in 1..=month_len {
                    assert_eq!(
                        days_to_ymd(days),
                        (year, month, day),
                        "wrong date for day {days}"
                    );
                    days += 1;
                }
            }
        }
        // 1970-01-01 ..= 2100-12-31 inclusive.
        assert_eq!(days, 47_847);
    }

    /// The exact end-to-end symptom: a summary created on 2028-02-01 must
    /// render with that date, not `2028-01-32`.
    #[test]
    fn leap_february_renders_as_a_real_calendar_date() {
        // 2028-02-01T12:00:00Z
        assert_eq!(format_unix_short(1_833_019_200), "2028-02-01 12:00:00");
    }

    /// Absurd input must saturate, not wrap.
    ///
    /// `days as i64` wraps above `i64::MAX`, so `days_to_ymd(u64::MAX)`
    /// reported 1969 — going backwards in time for the largest possible
    /// input. The year must never decrease as `days` increases.
    #[test]
    fn days_to_ymd_saturates_instead_of_wrapping() {
        let (y_max, _, _) = days_to_ymd(u64::MAX);
        let (y_prev, _, _) = days_to_ymd(u64::MAX / 2);
        assert!(
            y_max >= y_prev,
            "larger day count must not yield an earlier year ({y_prev} -> {y_max})"
        );
        // A plausible timestamp still converts exactly.
        assert_eq!(days_to_ymd(20_675), (2026, 8, 10));
    }

    #[test]
    fn format_unix_short_shows_ymd() {
        // 0 is the sentinel for "unset", rendered as "-".
        assert_eq!(format_unix_short(0), "-");
        // 2025-08-03 00:00:00 UTC
        assert_eq!(format_unix_short(1_754_179_200), "2025-08-03 00:00:00");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // The `list` call site truncates the topic column to 24, and
        // `validate_topic` bounds a topic by *bytes* (128), so a topic of 23
        // ASCII bytes plus one 3-byte code point is a legal 26-byte topic.
        // The old byte slice `&s[..24]` split the code point and panicked.
        let cjk = format!("{}日", "a".repeat(23));
        assert_eq!(cjk.len(), 26);
        assert_eq!(truncate(&cjk, 24), cjk);

        // Genuine multi-byte truncation stays on a character boundary.
        assert_eq!(truncate("日本語日本語日本語", 4), "日本語…");

        // A 24-character ASCII string fits and is returned whole.
        assert_eq!(
            truncate("abcdefghijklmnopqrstuvwx", 24),
            "abcdefghijklmnopqrstuvwx"
        );
        // 25 characters: the ellipsis is included in the `max` budget.
        assert_eq!(
            truncate("abcdefghijklmnopqrstuvwxy", 24),
            "abcdefghijklmnopqrstuvw…"
        );
    }
}
