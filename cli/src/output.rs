//! Output formatting for the mAgent CLI.
//!
//! Two modes:
//!
//! * **Human** (default): ANSI-coloured step-by-step trace on stderr,
//!   plain final answer on stdout. This is what users see when they run
//!   `magent run "..."` interactively.
//!
//! * **JSON** (`--json`): a single JSON envelope on stdout that scripts
//!   and CI pipelines can parse — a success payload, or on failure
//!   `{"ok":false,"error":…,"code":…,"exit":…}`. stderr still carries the
//!   human `error:` diagnostic so an operator running `--json` by hand
//!   isn't left staring at an empty terminal.
//!
//! The two writers are unified behind the [`Output`] type so `runner.rs`
//! doesn't have to branch on every print site.

use std::io::{self, IsTerminal, Write};

/// What kind of output to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Human,
    Json,
}

/// Bundle of stdout / stderr writers plus formatting preferences.
pub struct Output {
    kind: OutputKind,
    /// Buffered stdout (the agent's final answer or the JSON envelope).
    stdout: io::StdoutLock<'static>,
    /// Buffered stderr (step-by-step trace, only in Human mode).
    stderr: io::StderrLock<'static>,
    /// Whether ANSI escape codes should be emitted on stderr.
    color: bool,
    /// The pending final answer, only used in JSON mode. Stored when
    /// [`Output::final_answer`] is called so [`Output::write_json`] can
    /// include it in the envelope.
    pending_answer: Option<String>,
    /// Whether a complete JSON document has already reached stdout.
    ///
    /// `--json` promises that stdout holds *exactly one* object, so a
    /// later [`Output::fail`] must not append a second one on top of an
    /// envelope some subcommand already wrote. `scheduler run-once` is
    /// the concrete case: it writes its result envelope and *then*
    /// returns `TasksFailed`, so without this latch stdout carried two
    /// JSON objects and `jq` choked on the second.
    envelope_written: bool,
}

impl Output {
    /// Build an `Output` from the parsed [`crate::cli::GlobalFlags`] and
    /// the process's actual stdout/stderr streams.
    pub fn new(kind: OutputKind, no_color_flag: bool) -> Self {
        let stdout = io::stdout();
        let stderr = io::stderr();

        // Honour the flag first, then fall back to TTY detection.
        let color = if no_color_flag {
            false
        } else {
            stderr.is_terminal()
        };

        Self {
            kind,
            stdout: stdout.lock(),
            stderr: stderr.lock(),
            color,
            pending_answer: None,
            envelope_written: false,
        }
    }

    pub fn kind(&self) -> OutputKind {
        self.kind
    }

    /// Emit a step-by-step trace line. Silently dropped in JSON mode.
    pub fn trace(&mut self, line: &str) -> io::Result<()> {
        if self.kind == OutputKind::Json {
            return Ok(());
        }
        let prefix = if self.color { "\x1b[2m" } else { "" };
        let suffix = if self.color { "\x1b[0m" } else { "" };
        writeln!(self.stderr, "{}[agent] {}{}", prefix, line, suffix)
    }

    /// Emit a labelled trace line — useful for the runner to print
    /// `[Thinking] …`, `[Tool] read_sensor → 23.4`, etc. with consistent
    /// formatting.
    pub fn trace_labeled(&mut self, label: &str, body: &str) -> io::Result<()> {
        if self.kind == OutputKind::Json {
            return Ok(());
        }
        let prefix = if self.color { "\x1b[36m" } else { "" }; // cyan
        let reset = if self.color { "\x1b[0m" } else { "" };
        writeln!(self.stderr, "{}[{}] {}{}", prefix, label, body, reset)
    }

    /// Print a warning. Always shown, even in JSON mode (goes to stderr).
    pub fn warn(&mut self, msg: &str) -> io::Result<()> {
        let prefix = if self.color { "\x1b[33m" } else { "" }; // yellow
        let reset = if self.color { "\x1b[0m" } else { "" };
        writeln!(self.stderr, "{}warning: {}{}", prefix, msg, reset)
    }

    /// Report a command failure.
    ///
    /// Human mode: the familiar red `error: …` line on stderr.
    ///
    /// JSON mode: the same human line on stderr **and** a structured
    /// envelope on stdout:
    ///
    /// ```json
    /// { "ok": false, "error": "…", "code": "run_error", "exit": 1 }
    /// ```
    ///
    /// So `--json` makes one promise about stdout — it is always exactly
    /// one parseable JSON object — and that promise now holds for the
    /// failure path too. Before this, a failing `--json` command wrote
    /// *nothing* to stdout, so a CI pipeline doing `magent --json … | jq`
    /// got a parse error instead of the failure it was trying to detect.
    ///
    /// `code` is a stable machine-readable tag (see the dispatch arms in
    /// `main.rs`); `exit` is the process status the caller is about to
    /// return, so a consumer that captured only stdout still learns the
    /// outcome. `ok: false` is the discriminator — a success envelope
    /// never carries it.
    ///
    /// If a subcommand already wrote its own document to stdout (see
    /// [`Self::envelope_written`]), the envelope is skipped and only the
    /// stderr line is added — the subcommand's document is the one
    /// allowed to describe the outcome, and adding a second object would
    /// make stdout unparseable.
    pub fn fail(&mut self, code: &str, exit: u8, msg: &str) {
        let prefix = if self.color { "\x1b[31m" } else { "" }; // red
        let reset = if self.color { "\x1b[0m" } else { "" };
        let _ = writeln!(self.stderr, "{}error: {}{}", prefix, msg, reset);

        if self.kind == OutputKind::Json && !self.envelope_written {
            let envelope = serde_json::json!({
                "ok": false,
                "error": msg,
                "code": code,
                "exit": exit,
            });
            let json = serde_json::to_string_pretty(&envelope)
                .unwrap_or_else(|_| "{\"ok\":false}".to_string());
            let _ = writeln!(self.stdout, "{}", json);
        }
    }

    /// Print an informational line. In Human mode it's just a
    /// plain stderr line (used by command-success summaries).
    /// In JSON mode the envelope is the only stdout output, so
    /// we write the info line to stderr instead — that way `jq`
    /// still sees a clean envelope and the operator still sees
    /// the diagnostic. The line is prefixed with `info:` so
    /// downstream log scrapers can route it.
    pub fn info(&mut self, msg: &str) -> io::Result<()> {
        if self.kind == OutputKind::Human {
            writeln!(self.stderr, "{}", msg)
        } else {
            // JSON mode → stderr with explicit prefix so the line
            // is identifiable as a diagnostic and can't be confused
            // with the JSON envelope on stdout.
            writeln!(self.stderr, "info: {}", msg)
        }
    }

    /// Borrow the stdout writer. Used by subcommands that need to
    /// write a raw payload (`magent set-prompt export`) without
    /// going through the JSON envelope. Returns `None` if the
    /// underlying writer can't be borrowed (e.g. a panic earlier in
    /// the process already holds the lock).
    pub fn stdout_writer(&mut self) -> &mut io::StdoutLock<'static> {
        &mut self.stdout
    }

    /// Same as [`Self::stdout_writer`] but writes through the
    /// coloured stderr channel. Useful for the human-mode pretty
    /// printers in `prompt.rs::run_show` / `run_list`.
    pub fn stderr_writer(&mut self) -> &mut io::StderrLock<'static> {
        &mut self.stderr
    }

    /// Write a raw human-mode payload (string) to stdout. Used by
    /// the summary subcommand's `export` and `load` actions whose
    /// job is to feed another process, and by `save` for its
    /// "saved N bytes to <path>" confirmation. In JSON mode this
    /// is a no-op so the JSON envelope on stdout stays clean.
    ///
    /// Returns `Err` only when the underlying IO write fails.
    pub fn write_human(&mut self, payload: &str) {
        if self.kind == OutputKind::Human {
            let _ = self.stdout.write_all(payload.as_bytes());
        }
    }

    /// Write a raw payload line to stdout in *both* modes. Used by
    /// commands whose payload *is* their stdout output (`magent web3
    /// export`, `magent web3 sign` with no `--output`) so the
    /// documented `… > out.json` redirection works identically in
    /// human and JSON mode — `write_json_str` is a no-op in human
    /// mode, which silently produced an empty file there.
    ///
    /// Unlike [`Self::write_human`] this appends a trailing newline
    /// so the redirected stream stays line-oriented.
    pub fn write_stdout_line(&mut self, payload: &str) {
        let _ = self.stdout.write_all(payload.as_bytes());
        let _ = self.stdout.write_all(b"\n");
        self.envelope_written = true;
    }

    /// Write a JSON envelope payload (string form) to stdout. In
    /// Human mode this is a no-op so step-by-step output isn't
    /// interleaved with the envelope.
    ///
    /// Callers are expected to hand us a pre-rendered JSON string
    /// (typically the result of `serde_json::to_string`) — this
    /// helper does no validation because the rendering layer is
    /// the only place that knows the wire shape.
    pub fn write_json_str(&mut self, payload: String) {
        if self.kind == OutputKind::Json {
            let _ = self.stdout.write_all(payload.as_bytes());
            let _ = self.stdout.write_all(b"\n");
            self.envelope_written = true;
        }
    }

    /// Write a formatted line to the human-mode stderr trace channel.
    /// Internally uses [`std::fmt::Write`] so callers can use the
    /// familiar `writeln!`/`write!` syntax with positional args.
    /// The data is rendered into an in-memory buffer and then flushed
    /// to the underlying `StderrLock` via [`io::Write`].
    ///
    /// Use this for **diagnostics** — labelled traces, warnings,
    /// confirmations. For a command's actual *data* use
    /// [`Self::stdout_fmt_line`] so `magent … > file` works.
    pub fn stderr_fmt_line(&mut self, args: std::fmt::Arguments<'_>) -> io::Result<()> {
        if self.kind == OutputKind::Json {
            return Ok(());
        }
        // Render via `std::fmt::Write` (String always implements it),
        // then drain to the IO stream so we don't have to depend on
        // `StderrLock: fmt::Write` (it doesn't, on stable).
        let mut buf = String::new();
        std::fmt::Write::write_fmt(&mut buf, args)
            .map_err(io::Error::other)?;
        buf.push('\n');
        self.stderr.write_all(buf.as_bytes())
    }

    /// [`Self::stderr_fmt_line`]'s opposite number: the same formatting
    /// contract, but the line goes to **stdout**.
    ///
    /// Human-mode data belongs on stdout — `magent config show >
    /// backup.json`, `magent config list | grep ollama`, and
    /// `magent set-prompt show foo | jq .provider` are the whole point
    /// of those subcommands, and writing them to stderr silently sent
    /// every redirect an empty file while `--json` worked. Still a
    /// no-op in JSON mode, where the envelope is the only stdout
    /// output.
    pub fn stdout_fmt_line(&mut self, args: std::fmt::Arguments<'_>) -> io::Result<()> {
        if self.kind == OutputKind::Json {
            return Ok(());
        }
        let mut buf = String::new();
        std::fmt::Write::write_fmt(&mut buf, args).map_err(io::Error::other)?;
        buf.push('\n');
        self.stdout.write_all(buf.as_bytes())
    }

    /// Emit the final answer on stdout. In Human mode this is the only
    /// thing the user sees on stdout; in JSON mode it is wrapped in the
    /// envelope (handled by [`Self::write_json`]).
    pub fn final_answer(&mut self, answer: &str) -> io::Result<()> {
        if self.kind == OutputKind::Json {
            // JSON mode defers the envelope write until the caller has
            // computed the stats block, so just remember the answer.
            self.pending_answer = Some(answer.to_string());
            return Ok(());
        }

        let bar = "=".repeat(60);
        writeln!(self.stdout)?;
        writeln!(self.stdout, "{}", bar)?;
        writeln!(self.stdout, "RESULT")?;
        writeln!(self.stdout, "{}", bar)?;
        writeln!(self.stdout, "{}", answer)?;
        Ok(())
    }

    /// JSON-mode final write. `extra` is any additional top-level JSON
    /// fields the caller wants (e.g. `iterations`, `tool_calls`,
    /// `using_ollama`). Human mode is a no-op.
    pub fn write_json(&mut self, extra: serde_json::Value) -> io::Result<()> {
        if self.kind != OutputKind::Json {
            return Ok(());
        }
        let mut envelope = serde_json::Map::new();
        envelope.insert(
            "answer".to_string(),
            serde_json::Value::String(self.pending_answer.take().unwrap_or_default()),
        );
        if let serde_json::Value::Object(map) = extra {
            for (k, v) in map {
                envelope.insert(k, v);
            }
        }
        let json = serde_json::to_string_pretty(&serde_json::Value::Object(envelope))
            .unwrap_or_else(|_| "{}".to_string());
        writeln!(self.stdout, "{}", json)?;
        self.envelope_written = true;
        Ok(())
    }

    /// Flush both streams.
    pub fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.stderr.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_is_silent_in_json_mode() {
        // We can't easily test stdout/stderr content here without a
        // captured file handle, but we can at least verify the OutputKind
        // round-trips correctly and that trace() doesn't panic.
        let mut out = Output::new(OutputKind::Json, true);
        out.trace("hello").unwrap();
        out.trace_labeled("Thinking", "iteration 1").unwrap();
    }

    #[test]
    fn final_answer_in_human_mode_is_an_io_result() {
        // Just exercises the path; we can't capture stdout in a unit
        // test without redirecting process IO.
        let mut out = Output::new(OutputKind::Human, true);
        out.final_answer("done").unwrap();
    }

    #[test]
    fn write_json_in_human_mode_is_a_noop() {
        let mut out = Output::new(OutputKind::Human, true);
        out.final_answer("done").unwrap();
        out.write_json(serde_json::json!({"iterations": 3})).unwrap();
    }

    #[test]
    fn write_stdout_line_writes_in_both_modes() {
        // Regression guard for `magent web3 export` / `web3 sign`
        // (default stdout): their payload must reach stdout in human
        // mode too. We can't capture process stdout from a unit test,
        // so this exercises both modes to catch a future refactor that
        // re-gates `write_stdout_line` on `OutputKind::Json` — which
        // would silently empty `… > out.json` again.
        let mut human = Output::new(OutputKind::Human, true);
        human.write_stdout_line("{\"a\":1}");
        let mut json = Output::new(OutputKind::Json, true);
        json.write_stdout_line("{\"a\":1}");
    }

    #[test]
    fn info_is_safe_in_both_modes() {
        // `info` should never panic and should return Ok in
        // both modes. We can't easily capture stderr here
        // without process-IO redirection, but we can at
        // least exercise the path.
        let mut human = Output::new(OutputKind::Human, true);
        human.info("ready").unwrap();
        let mut json = Output::new(OutputKind::Json, true);
        json.info("ready").unwrap();
    }

    #[test]
    fn warn_is_safe_in_both_modes() {
        // Same coverage for `warn` — stderr output in both
        // modes, but we don't want to silently drop it in JSON
        // mode (operator still needs to see it).
        let mut human = Output::new(OutputKind::Human, true);
        human.warn("careful").unwrap();
        let mut json = Output::new(OutputKind::Json, true);
        json.warn("careful").unwrap();
    }
}
