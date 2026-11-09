//! Regression guard for `magent run --repl`.
//!
//! The REPL had no test coverage at all, which is how two of its
//! commands shipped advertising behaviour that did not exist (arrow-key
//! history navigation, "no task ⇒ REPL"). These tests drive the real
//! binary with piped stdin in `--mock` mode so no LLM is contacted, and
//! assert on the interactive transcript it writes to stdout.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Throwaway HOME so prompt/summary/state files land in a directory we
/// own and delete afterwards.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "magent-repl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        Self { home }
    }

    /// A `magent` invocation wired to this sandbox's HOME.
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_magent"));
        c.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env_remove("MAGENT_WEB3_PASSPHRASE")
            .env_remove("MAGENT_WEB3_PASSPHRASE_ENV");
        c
    }

    /// Replay a scripted session through the REPL with `--mock` and
    /// collect its output.
    fn session(&self, input: &str) -> Output {
        self.session_with(&["--mock"], input)
    }

    /// Like [`Sandbox::session`] but with explicit extra `run` flags.
    fn session_with(&self, args: &[&str], input: &str) -> Output {
        self.session_env(&[], args, input)
    }

    /// Like [`Sandbox::session_with`] but with extra environment variables
    /// set on the child (e.g. an API key to exercise credential resolution).
    fn session_env(&self, envs: &[(&str, &str)], args: &[&str], input: &str) -> Output {
        self.session_env_bytes(envs, args, input.as_bytes())
    }

    /// Like [`Sandbox::session_env`] but with raw bytes on stdin, so a test
    /// can send input that is not valid UTF-8 (which `&str` cannot express).
    fn session_env_bytes(&self, envs: &[(&str, &str)], args: &[&str], input: &[u8]) -> Output {
        let mut cmd = self.cmd();
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd
            .args(["run", "--repl"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn magent");
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(input)
            .expect("write REPL script");
        child.wait_with_output().expect("collect magent output")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Read the integer that follows `label` in `s` (e.g. `"Total iterations:"`).
fn number_after(s: &str, label: &str) -> usize {
    s.split(label)
        .nth(1)
        .unwrap_or_else(|| panic!("`{label}` not found in:\n{s}"))
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("no number after `{label}` in:\n{s}"))
        .parse()
        .unwrap_or_else(|e| panic!("not a number after `{label}`: {e}\n{s}"))
}

/// Every built-in command must respond and the session must exit 0.
#[test]
fn repl_builtin_commands_all_respond() {
    let sb = Sandbox::new("builtins");
    let out = sb.session(
        "help\nstats\nhistory\ncontext\nsession\nbackend\ntools\nalias\nprompt\nenv\nclear\nq\n",
    );
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    for marker in [
        "mAgent REPL Commands", // help
        "Session Statistics",   // stats
        "Task history",         // history
        "Context:",             // context
        "Session info:",        // session
        "Backend:",             // backend
        "Available Tools",      // tools
        "Aliases:",             // alias
        "System prompt",        // prompt
        "Environment:",         // env
        "Goodbye",              // quit
        // `/tools` lists the prompt's tools. It used to read the nRF52-era
        // five-tool catalog, so it named `read_sensor` but not a single health
        // tool — while the model was being told about all of them.
        "start_exercise(type)",
        "get_sleep_quality()",
        "notify_emergency_contact(name, message)",
        // `env` lists every variable the CLI consults; the store dirs were
        // missing, though `MAGENT_PROMPTS_DIR` relocates the prompt store
        // `--prompt-name` reads (which the REPL honours).
        "MAGENT_PROMPTS_DIR",
        "MAGENT_SUMMARIES_DIR",
    ] {
        assert!(s.contains(marker), "missing `{marker}` in:\n{s}");
    }
}

/// `help` is the REPL's contract, so every command the dispatcher accepts
/// has to be in it. `alias` was missing entirely, and `hist` (a working
/// alias of `history`) went unmentioned.
///
/// The check is line-anchored: a bare `contains("alias")` is satisfied by
/// the prose line "(`alias` alone takes an argument.)", which is exactly how
/// the missing command went unnoticed the first time.
#[test]
fn repl_help_lists_every_command() {
    let sb = Sandbox::new("helplist");
    let out = sb.session("/help\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);

    /// True when some box line's *command column* is exactly `entry`,
    /// padded by the column separator. Matching the padded form (not a bare
    /// prefix) keeps `alias` from being satisfied by a longer name and
    /// `reset` from being satisfied by the prose line about it.
    fn listed(s: &str, entry: &str) -> bool {
        s.lines().any(|l| {
            l.trim_start_matches('║')
                .trim_start()
                .strip_prefix(entry)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
    }

    for entry in [
        "help, ?",
        "quit, exit, q",
        "clear, cls",
        "reset",
        "context",
        "prompt",
        "retry, rerun",
        "stats",
        "history, hist",
        "tools",
        "session",
        "backend",
        "env",
        "alias",
    ] {
        assert!(
            listed(&s, entry),
            "`help` omits the `{entry}` command:\n{s}"
        );
    }
    // The `/` form the dispatcher accepts is documented too.
    assert!(
        s.contains("/help"),
        "help does not mention the `/` prefix form:\n{s}"
    );
}

/// A leading `/` is accepted, so `/help` and `/quit` behave like their
/// bare forms (the dispatcher used to match only the bare token and
/// report `/help` as an unknown command).
#[test]
fn repl_slash_prefixed_commands_are_recognised() {
    let sb = Sandbox::new("slash");
    let out = sb.session("/help\n/nosuch\n/\n/quit\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("mAgent REPL Commands"),
        "/help did not show help:\n{s}"
    );
    assert!(
        s.contains("Unknown command: /nosuch"),
        "unknown slash command should name the full token:\n{s}",
    );
    // A lone `/` is the sigil with no command. It used to be the one slash
    // line that did nothing at all, while `/nosuch` reported itself. Match
    // the full sentence — `"Unknown command: /"` alone is also a prefix of
    // the `/nosuch` line above.
    assert!(
        s.contains("Unknown command: /. Type"),
        "a lone `/` was silently ignored:\n{s}",
    );
    assert!(s.contains("Goodbye"), "/quit did not exit:\n{s}");

    // A leading `/` is the command sigil regardless of arguments, so a
    // slash line with arguments is still an error, not a task.
    let with_args = sb.session("/nosuch with args\nq\n");
    assert!(
        stdout(&with_args).contains("/nosuch is not a command form"),
        "a slash command with arguments was not rejected:\n{}",
        stdout(&with_args),
    );
}

/// Every alias `help` advertises has to reach the same arm as its
/// canonical spelling. The aliases live in the `match` patterns, not in a
/// table, so dropping one leaves `help` listing a name that answers
/// "Unknown command" — and nothing else in the suite types them.
#[test]
fn repl_aliases_reach_the_same_arms_as_their_canonical_spellings() {
    let sb = Sandbox::new("aliasarms");
    // `read the temperature` seeds the history so `hist` has a task to
    // show and `rerun` one to re-run.
    let out = sb.session("read the temperature\n?\ncls\nhist\nrerun\nexit\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("mAgent REPL Commands"),
        "`?` did not print help:\n{s}"
    );
    assert!(
        s.contains("Task history"),
        "`hist` did not print the task history:\n{s}"
    );
    assert!(
        s.contains("Retrying:"),
        "`rerun` did not re-run the task:\n{s}"
    );
    assert!(
        s.contains("Goodbye"),
        "`exit` did not end the session:\n{s}"
    );
    assert!(
        !s.contains("\u{1b}[2J"),
        "`cls` wrote a screen-clear escape to a non-terminal stdout:\n{s}"
    );
    assert!(
        !s.contains("Unknown command"),
        "an advertised alias was reported as an unknown command:\n{s}"
    );
}

/// Display width for the glyphs that appear in a box: the box itself is
/// one column, ASCII one, the emoji block two, and `…` one.
fn box_width(line: &str) -> usize {
    line.chars()
        .map(|c| {
            if ('\u{1F300}'..='\u{1FAFF}').contains(&c) {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Assert every box row in `s` ends at the same display column, and that
/// there were enough of them for the check to mean anything.
fn assert_boxes_are_rectangular(s: &str) {
    let mut box_lines = 0;
    for line in s.lines() {
        // Every box row starts at column 0 — the prompt that used to
        // share a line with the first row now goes to stderr (see
        // `repl_prompt_goes_to_stderr_not_stdout`), because this harness
        // makes neither stream a terminal.
        if !line.starts_with(['║', '╔', '╚', '╠']) {
            continue;
        }
        box_lines += 1;
        assert_eq!(
            box_width(line),
            64,
            "box row is {w} columns, not 64: {line:?}",
            w = box_width(line)
        );
    }
    assert!(box_lines > 10, "no box rows found in:\n{s}");
}

/// Every box row must end at the same display column. The printers used
/// `{:>50}` / `{:>6}`, which counts **bytes** and ignores the double-width
/// `🤖`, so measured widths ranged 59–72 against a 64-column border.
#[test]
fn repl_boxes_are_rectangular() {
    let sb = Sandbox::new("boxes");
    let out = sb.session("help\nstats\ntools\nq\n");
    assert!(out.status.success(), "REPL exited non-zero");
    assert_boxes_are_rectangular(&stdout(&out));
}

/// A user-supplied string in a box must not widen that one row past the
/// border. `--model <90 columns>` went straight into the banner's
/// `Model:` row, which then measured 103 columns against a 64-column box
/// — every other row in the session stayed rectangular, so the frame
/// visibly fell apart on the very first screen.
#[test]
fn repl_boxes_survive_a_long_model_name() {
    let sb = Sandbox::new("longmodel");
    let model = "m".repeat(90);
    let out = sb.session_with(&["--mock", "--model", &model], "stats\nq\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert_boxes_are_rectangular(&s);
    // The row is clipped, not dropped: the banner still names the model.
    assert!(
        s.contains("Model:   mmm"),
        "the banner lost the model row entirely:\n{s}"
    );
    assert!(
        s.contains('…'),
        "a clipped row should be marked with an ellipsis:\n{s}"
    );
}

/// The clipping above must not fire on ordinary values — a `…` in the
/// banner of a normal session would be a bug of its own.
#[test]
fn repl_banner_leaves_a_normal_model_name_alone() {
    let sb = Sandbox::new("shortmodel");
    let out = sb.session_with(&["--mock", "--model", "llama3.2"], "q\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("Model:   llama3.2"),
        "the model name was mangled:\n{s}"
    );
    assert!(!s.contains('…'), "a short value was clipped:\n{s}");
}

/// `alias <name>` is the one argument form the whole-line rule allows, so
/// it has to *use* the argument. It used to print the same fixed sentence
/// for every input, which made the exception pointless.
#[test]
fn repl_alias_with_an_argument_resolves_that_name() {
    let sb = Sandbox::new("aliasarg");
    let out = sb.session("alias hist\nalias ?\nalias help\nalias /cls\nalias nosuch\nq\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("hist → history (alias)."),
        "`alias hist` did not resolve the alias:\n{s}"
    );
    assert!(
        s.contains("? → help (alias)."),
        "`alias ?` did not resolve the alias:\n{s}"
    );
    assert!(
        s.contains("cls → clear (alias)."),
        "a slash-prefixed argument was not resolved:\n{s}"
    );
    assert!(
        s.contains("help is a command (no alias needed)."),
        "`alias help` did not recognise a canonical command:\n{s}"
    );
    assert!(
        s.contains("No command or alias named \"nosuch\""),
        "an unknown name was not reported:\n{s}"
    );
}

/// A one-word task that collides with a built-in command is otherwise
/// unreachable: the bare word always dispatches the command, and the
/// whole-line rule only rescues the multi-word form. `!<task>` is the
/// escape hatch.
#[test]
fn repl_bang_prefix_forces_a_builtin_word_to_be_a_task() {
    let sb = Sandbox::new("bangescape");
    let out = sb.session("!stats\nhistory\nq\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        !s.contains("Session Statistics"),
        "`!stats` was dispatched as the command instead of run as a task:\n{s}"
    );
    assert!(
        s.contains("1. stats"),
        "the forced task was not recorded in the history:\n{s}"
    );

    // A bare `!` says what it needs instead of running an empty task.
    let bare = sb.session("!\nq\n");
    assert!(
        stdout(&bare).contains("`!` must be followed by a task"),
        "a bare `!` was silently ignored:\n{}",
        stdout(&bare),
    );
}

/// The dispatcher lowercases the first word before matching (`cmd
/// .to_lowercase()`), so the documented commands work in any case. No test
/// pinned it: every other script types them lowercase.
#[test]
fn repl_commands_are_case_insensitive() {
    let sb = Sandbox::new("replcase");
    let out = sb.session("read the temperature\nSTATS\nHist\nQuit\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("Session Statistics"),
        "`STATS` did not reach the stats arm:\n{s}"
    );
    assert!(
        s.contains("Task history"),
        "`Hist` did not reach the history arm:\n{s}"
    );
    assert!(
        s.contains("Goodbye"),
        "`Quit` did not end the session:\n{s}"
    );
    assert!(
        !s.contains("Unknown command"),
        "an uppercase command fell through to a task:\n{s}"
    );
}

/// `exit` must stop the loop, not merely print the farewell: input after
/// it is never read. Otherwise a scripted session would still run whatever
/// the operator wrote after the exit.
#[test]
fn repl_exit_stops_reading_further_input() {
    let sb = Sandbox::new("exitstop");
    let out = sb.session("exit\nread the temperature\n");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(s.contains("Goodbye"), "no farewell on exit:\n{s}");
    assert!(
        !s.contains("Temperature sensor reading completed"),
        "a task after `exit` was still executed:\n{s}"
    );
}

/// A slash line with arguments is rejected, but the message must not claim
/// the command is *unknown* when it is one of the built-ins — `/help extra`
/// used to answer `Unknown command: /help`.
#[test]
fn repl_slash_command_with_args_names_the_real_rule() {
    let sb = Sandbox::new("slashargs");
    let out = sb.session("/help extra\n/nosuch extra\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        !s.contains("Unknown command: /help"),
        "a known command was reported as unknown:\n{s}"
    );
    for line in [
        "/help is not a command form",
        "/nosuch is not a command form",
    ] {
        assert!(
            s.contains(line),
            "missing `{line}` — the whole-line rule was not explained:\n{s}"
        );
    }
    assert!(
        s.contains("must be on a line by itself"),
        "the rule itself was not stated:\n{s}"
    );
}

/// A built-in command must be the *whole* line (with `alias` the one
/// exception, since it has an argument form). The dispatcher used to match
/// the first word alone, so ordinary tasks were hijacked by a command word —
/// `reset the temperature sensor` silently wiped the conversation, `clear
/// the flash` cleared the screen — and `help`'s "Any other input is treated
/// as a task for the agent" was untrue.
///
/// Each probe input is one that *would* print a recognisable banner if it
/// were still dispatched as a command (`clear` is deliberately absent: it
/// writes only to a terminal, so under a piped stdin it is undetectable
/// either way).
#[test]
fn repl_multi_word_input_is_a_task_not_a_command() {
    let sb = Sandbox::new("hijack");
    let out = sb.session(
        "reset the temperature sensor\nstats for the last hour\ntools list\nprompt the user\nhistory of the device\nsession\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);

    for banner in [
        "Conversation reset. System prompt preserved.",
        "Session Statistics",
        "Available Tools",
        "System prompt (first message)",
        "Task history",
    ] {
        assert!(
            !s.contains(banner),
            "a task was hijacked by a command name (`{banner}`):\n{s}",
        );
    }

    // The five multi-word inputs all ran as tasks; only the bare `session`
    // at the end dispatched as a command.
    assert!(
        s.contains("Session info:"),
        "the bare `session` command stopped dispatching:\n{s}",
    );
    assert!(
        s.contains("Tasks run:    5"),
        "the five multi-word inputs did not all run as tasks:\n{s}",
    );

    // `alias` keeps its argument form — and the argument is now *used*
    // (it resolves against the alias table) instead of being discarded
    // behind a fixed sentence.
    let alias = sb.session("alias foo\nq\n");
    assert!(
        stdout(&alias).contains("No command or alias named \"foo\""),
        "`alias` with an argument no longer dispatches, or ignores it:\n{}",
        stdout(&alias),
    );
}

/// The REPL cannot honour the single-task output flags — `save_summary_`
/// and `sign_after_run` both hang off the one-shot `finalize_report` — so
/// accepting them silently (as it did) is a lie the user cannot see. It now
/// says which flags it is dropping.
#[test]
fn repl_warns_about_flags_it_cannot_honour() {
    let sb = Sandbox::new("ignoreflags");

    let none = sb.session("q\n");
    assert!(
        !stderr(&none).contains("--repl ignores"),
        "the warning fired with no such flag given:\n{}",
        stderr(&none),
    );

    let out = sb.session_with(&["--mock", "--save-summary", "demo"], "q\n");
    assert!(
        stderr(&out).contains("--repl ignores --save-summary"),
        "silently ignored --save-summary:\n{}",
        stderr(&out),
    );

    // `--load-summary` used to be listed here; it is now honoured by
    // `execute_repl` itself, so it must NOT appear in the drop list. (Its
    // own "no summary named …" warning is the one-shot message, not this.)
    let load = sb.session_with(&["--mock", "--load-summary", "nosuchtopic"], "q\n");
    let load_err = stderr(&load);
    assert!(
        !load_err.contains("--repl ignores"),
        "--load-summary is honoured now, so nothing should be dropped:\n{load_err}",
    );
    assert!(
        load_err.contains("no summary named"),
        "a missing topic should still be reported:\n{load_err}",
    );
}

/// `--load-summary` used to be dropped in the REPL with a
/// `--repl ignores` warning — the one-shot path consumed the flag at step
/// 3.5, *after* the REPL had already been entered and returned. It is now
/// honoured, so this pins the plumbing around it: the load is reported,
/// nothing is listed as dropped, and a missing topic warns without killing
/// the session. That the note actually reaches the model is asserted by
/// `repl_keeps_the_loaded_summary_across_a_turn`.
#[test]
fn repl_load_summary_is_honoured_and_reported() {
    let sb = Sandbox::new("loadsummary");

    // A summary of the shape `magent summary save` writes. Written
    // directly: the point under test is the *load* path, and building the
    // record through a mock run would couple this test to the save path.
    let dir = sb.home.join("summaries");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("coach.json"),
        r#"{
  "schema_version": 1,
  "topic": "coach",
  "source": {
    "session_id": null,
    "provider": "ollama",
    "model": "llama3.2",
    "original_message_count": 2,
    "policy": { "max_messages": 32, "tool_content_max_chars": 800 }
  },
  "head_tail_window": [
    { "role": "user", "content": "PREVIOUS_RUN_MARKER please continue", "tool_call_id": null },
    { "role": "assistant", "content": "acknowledged", "tool_call_id": null }
  ],
  "llm_summary": "PRIOR_SUMMARY_MARKER",
  "stats": { "kept": 2, "dropped": 0, "tool_results_truncated": 0, "bytes_saved": 0 },
  "metadata": { "description": null, "author": null, "tags": [] },
  "history": [],
  "created_at": 1754716800,
  "updated_at": 1754800000
}"#,
    )
    .unwrap();

    let out = sb.session_env(
        &[("MAGENT_SUMMARIES_DIR", dir.to_str().unwrap())],
        &["--mock", "--load-summary", "coach"],
        "q\n",
    );
    assert!(out.status.success(), "REPL exited non-zero");
    let err = stderr(&out);
    assert!(
        err.contains("loaded coach window"),
        "the summary should have been loaded:\n{err}",
    );
    assert!(
        !err.contains("--repl ignores"),
        "--load-summary must not be reported as dropped:\n{err}",
    );

    // A topic that is not on disk is a warning, not a dead session: the REPL
    // cannot be in JSON mode (the parser refuses `--repl --json`, and
    // `io.json_default` is rejected on entry), so there is no hard-error case
    // to mirror from the one-shot path.
    let missing = sb.session_env(
        &[("MAGENT_SUMMARIES_DIR", dir.to_str().unwrap())],
        &["--mock", "--load-summary", "nosuchtopic"],
        "q\n",
    );
    assert!(
        missing.status.success(),
        "a missing topic killed the session"
    );
    assert!(
        stderr(&missing).contains("no summary named"),
        "a missing topic should be named:\n{}",
        stderr(&missing),
    );
}

/// The seeded window has to survive the first task, not just sit there
/// until `run_continuing` resets it.
#[test]
fn repl_keeps_the_loaded_summary_across_a_turn() {
    let sb = Sandbox::new("loadsummaryturn");
    let dir = sb.home.join("summaries");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("coach.json"),
        r#"{
  "schema_version": 1,
  "topic": "coach",
  "source": {
    "session_id": null,
    "provider": "ollama",
    "model": "llama3.2",
    "original_message_count": 1,
    "policy": { "max_messages": 32, "tool_content_max_chars": 800 }
  },
  "head_tail_window": [
    { "role": "user", "content": "PREVIOUS_RUN_MARKER please continue", "tool_call_id": null }
  ],
  "llm_summary": null,
  "stats": { "kept": 1, "dropped": 0, "tool_results_truncated": 0, "bytes_saved": 0 },
  "metadata": { "description": null, "author": null, "tags": [] },
  "history": [],
  "created_at": 1754716800,
  "updated_at": 1754800000
}"#,
    )
    .unwrap();

    let env: &[(&str, &str)] = &[("MAGENT_SUMMARIES_DIR", dir.to_str().unwrap())];

    // Differential: the same script with and without the load. An absolute
    // threshold proves nothing here, because the mock's own turns already
    // clear any small number — only the *delta* shows the note survived.
    let plain = sb.session_env(env, &["--mock"], "read the temperature\ncontext\nq\n");
    let loaded = sb.session_env(
        env,
        &["--mock", "--load-summary", "coach"],
        "read the temperature\ncontext\nq\n",
    );

    let plain_msgs = number_after(&stdout(&plain), "Messages:");
    let loaded_msgs = number_after(&stdout(&loaded), "Messages:");
    assert_eq!(
        loaded_msgs,
        plain_msgs + 1,
        "the loaded window was dropped when the first task ran \
         (with: {loaded_msgs}, without: {plain_msgs}):\n{}",
        stdout(&loaded),
    );
}

/// A reader that walks away (`magent run --repl | head`) must not turn
/// into a Rust panic. Rust ignores `SIGPIPE`, so the write fails with
/// `EPIPE` — and `println!` treats a failed stdout write as fatal. The
/// REPL did exactly that: `exit 101` plus a panic backtrace on stderr,
/// where a Unix filter is expected to die on `SIGPIPE` (status 141)
/// silently. `main` now restores the default disposition before doing
/// anything else.
#[cfg(unix)]
#[test]
fn repl_is_not_killed_by_a_rust_panic_when_stdout_closes() {
    let sb = Sandbox::new("brokenpipe");
    let mut cmd = sb.cmd();
    let mut child = cmd
        .args(["run", "--repl", "--mock"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn magent");

    // Enough long output (300 × the help box) to exceed the 64 KiB pipe
    // buffer, so the child is guaranteed to still be writing when the read
    // end goes away — otherwise it would finish into the buffer and exit 0
    // without ever hitting the broken pipe.
    let script: String = std::iter::repeat_n("help\n", 300).collect();
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(script.as_bytes())
        .expect("write REPL script");

    // Walk away mid-write, the way `| head` does.
    drop(child.stdout.take());

    let out = child.wait_with_output().expect("collect magent output");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(101),
        "the REPL panicked on a closed stdout:\n{err}",
    );
    assert!(
        !err.contains("panicked"),
        "a panic escaped even though the exit code was not 101:\n{err}",
    );
}

/// `--probe-ollama` promised "probe on every run() call" but the REPL
/// eagerly probed once at startup and then either latched the live
/// verdict or called `force_disable_backend()` — which also clears
/// `probe_ollama_on_run`, freezing the whole session into simulation.
/// Either way the flag did nothing. It must now survive a dropped
/// backend by reporting the failure per task instead of simulating.
#[test]
fn repl_probe_ollama_refuses_to_simulate_a_dropped_backend() {
    let sb = Sandbox::new("probe");
    // Port 1 has no listener, so the probe is refused immediately.
    let dead = ["--ollama", "http://127.0.0.1:1"];

    // Baseline: without the flag the session falls back to the canned
    // responder, so the assertions below cannot pass by accident.
    let plain = sb.session_with(&dead, "read the temperature\nq\n");
    let plain_out = stdout(&plain);
    assert!(
        plain_out.contains("mock fallback"),
        "the unreachable backend should have been latched as a fallback:\n{plain_out}"
    );
    assert!(
        plain_out.contains("Temperature sensor reading completed"),
        "the simulated responder should have answered:\n{plain_out}"
    );

    let out = sb.session_with(
        &[dead[0], dead[1], "--probe-ollama"],
        "read the temperature\nq\n",
    );
    // The *session* must survive the failed task — it keeps reading stdin
    // and quits cleanly. But this session is scripted (piped stdin), so it
    // must also report the failure through its exit code: see
    // `scripted_repl_exits_non_zero_after_a_failed_task`.
    assert_ne!(
        out.status.code(),
        Some(0),
        "a scripted session with a failed task must exit non-zero"
    );
    let s = stdout(&out);
    let err = stderr(&out);

    assert!(
        s.contains("probing each task"),
        "the banner should say the backend is being re-probed:\n{s}"
    );
    assert!(
        err.contains("LLM backend unreachable"),
        "the task should have reported the unreachable backend:\n{err}"
    );
    assert!(
        !s.contains("Temperature sensor reading completed"),
        "a simulated answer leaked into a --probe-ollama session:\n{s}"
    );
    // A failed task is not fatal: the session is still there to retry.
    assert!(s.contains("Goodbye"), "the session ended early:\n{s}");
}

/// The `❯ ` prompt is terminal UI, not data: when no stream is a
/// terminal it belongs on the diagnostic one (stderr), so a redirected
/// stdout holds the transcript and nothing else.
///
/// This harness pipes both streams, so it pins the "neither is a
/// terminal" arm of `lineedit::ui_on_stdout` for the fallback read; the
/// TTY arm makes the same choice through the same helper. Writing the
/// prompt to stdout unconditionally is what made
/// `magent run --repl > session.log` (stdin still a terminal) log one
/// line per keystroke — `❯ h`, `❯ he`, `❯ hel`, … — each with the
/// redraw's `\r\x1b[K` in front of it.
#[test]
fn repl_prompt_goes_to_stderr_not_stdout() {
    let sb = Sandbox::new("prompt-stream");
    let out = sb.session("help\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(!s.contains("❯"), "the prompt must not reach stdout:\n{s}");
    let err = stderr(&out);
    assert!(
        err.contains("❯"),
        "the prompt should be visible on stderr:\n{err}"
    );
    // The transcript itself is still whole on stdout.
    assert!(
        s.contains("mAgent REPL Commands"),
        "the `help` box should still be part of the transcript:\n{s}"
    );
}

/// A scripted REPL session (stdin is not a terminal) has no human to read
/// the transcript, so its exit code is the only failure signal a caller
/// gets. A task that fails must therefore end the process non-zero — even
/// though the session itself survives the failure.
#[test]
fn scripted_repl_exits_non_zero_after_a_failed_task() {
    let sb = Sandbox::new("scripted-fail");
    // Port 1 has no listener; `--probe-ollama` makes the task error instead
    // of falling back to the simulator, so exactly one task fails.
    let out = sb.session_with(
        &["--ollama", "http://127.0.0.1:1", "--probe-ollama"],
        "read the temperature\nq\n",
    );
    assert!(
        !out.status.success(),
        "a scripted session with a failed task must exit non-zero, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out),
    );
    let err = stderr(&out);
    assert!(
        err.contains("non-interactive stdin"),
        "the exit-code reason must be reported on stderr:\n{err}"
    );
    assert!(
        err.contains("1 of 1 task"),
        "the message must count the failed tasks:\n{err}"
    );
    // The session still reached its normal end — the failure is reported,
    // not fatal mid-session.
    assert!(
        stdout(&out).contains("Goodbye"),
        "the session should still quit cleanly:\n{}",
        stdout(&out),
    );
}

/// The other half of the scripted contract: a session whose tasks all
/// succeed must stay at exit 0, so the non-zero above is a real signal and
/// not just "any scripted session exits 1".
#[test]
fn scripted_repl_exits_zero_when_every_task_succeeds() {
    let sb = Sandbox::new("scripted-ok");
    let out = sb.session("read the temperature\nq\n");
    assert!(
        out.status.success(),
        "a scripted session with no failures must exit 0, got {:?}\nstderr:\n{}",
        out.status,
        stderr(&out),
    );
}

/// Every signing flag the REPL cannot honour must be named in the warning.
/// Only `--sign` / `--signed-output` were listed; `--not-before` /
/// `--not-after` and `--passphrase-env` — all read only inside
/// `sign_after_run`, which the REPL never reaches — vanished without a
/// word, even though a caller on the signing path is likely to pass them
/// next to `--sign`.
#[cfg(feature = "web3_app")]
#[test]
fn repl_names_every_signing_flag_it_drops() {
    let sb = Sandbox::new("signflags");
    let out = sb.session_with(
        &[
            "--mock",
            "--sign",
            "--signer",
            "agent-1",
            "--signed-output",
            "/tmp/never-written.json",
            "--not-before",
            "100",
            "--not-after",
            "200",
            "--passphrase-env",
            "MAGENT_NEVER_READ",
        ],
        "q\n",
    );
    let err = stderr(&out);
    assert!(
        err.contains("--repl ignores"),
        "no `--repl ignores` warning was emitted:\n{err}",
    );
    for flag in [
        "--sign/--signer",
        "--signed-output",
        "--not-before",
        "--not-after",
        "--passphrase-env",
    ] {
        assert!(
            err.contains(flag),
            "the `--repl ignores` warning omitted `{flag}`:\n{err}",
        );
    }
}

/// `retry` re-runs the last task, and reports clearly when there is
/// none. Commands themselves are never pushed onto the task history.
#[test]
fn repl_retry_reruns_last_task_and_handles_empty() {
    let sb = Sandbox::new("retry");
    let empty = sb.session("retry\nq\n");
    assert!(stdout(&empty).contains("No previous task to retry."));

    let seeded = sb.session("/help\nread the temperature\nretry\nhistory\nq\n");
    let s = stdout(&seeded);
    assert!(
        s.contains("Retrying: read the temperature"),
        "retry lost the task:\n{s}"
    );
    // Only the task is in history — the `/help` command is not.
    assert!(
        s.contains("1. read the temperature"),
        "history should list the task:\n{s}"
    );
}

/// `reset` clears the conversation but keeps the system prompt.
#[test]
fn repl_reset_keeps_system_prompt() {
    let sb = Sandbox::new("reset");
    let out = sb.session("read the temperature\nreset\ncontext\nq\n");
    let s = stdout(&out);
    assert!(s.contains("Conversation reset. System prompt preserved."));
    assert!(
        s.contains("Messages:     1"),
        "reset should leave 1 message (the system prompt):\n{s}"
    );
}

/// EOF on stdin (piped input, or Ctrl-D) must end the session cleanly
/// rather than spin or panic.
#[test]
fn repl_exits_cleanly_on_eof() {
    let sb = Sandbox::new("eof");
    let out = sb.session("");
    assert!(out.status.success(), "EOF exit was non-zero");
    assert!(stdout(&out).contains("Goodbye"));
}

/// A pipe that ends without a trailing newline must still run its last
/// command, and only then report end-of-input. `printf 'help' | magent
/// run --repl` is an ordinary invocation, and before line editing the
/// `read_line` that served it returned those partial bytes and the REPL
/// ran them; introducing an editor must not quietly change that.
///
/// Note this drives a *pipe*, so it exercises the non-TTY fallback, not
/// the raw-mode loop — the pty path is covered by `cli/src/lineedit.rs`'s
/// own tests over `tty::edit`. This test is the guard that the fallback
/// kept the old semantics.
#[test]
fn repl_runs_a_piped_command_without_a_trailing_newline() {
    let sb = Sandbox::new("nonewline");
    let out = sb.session("help");
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("mAgent REPL Commands"),
        "`help` was dropped:\n{s}"
    );
    assert!(
        s.contains("Goodbye"),
        "the session did not end cleanly after the partial line:\n{s}"
    );
}

/// A read error is not EOF. `read_line` consumes the line's bytes before it
/// validates them, so a stray non-UTF-8 byte is a *per-line* error: the REPL
/// must skip that line and carry on, not silently end the session. The old
/// `Err(_) => None` mapped every error to EOF, so one bad byte threw away the
/// rest of the input with no message.
#[test]
fn repl_skips_a_non_utf8_line_and_continues() {
    let sb = Sandbox::new("badutf8");
    let mut input = b"\xff\xfe not utf-8\n".to_vec();
    input.extend_from_slice(b"help\n"); // must still be reached
    let out = sb.session_env_bytes(&[], &["--mock"], &input);
    assert!(out.status.success(), "REPL exited non-zero");
    let s = stdout(&out);
    assert!(
        s.contains("mAgent REPL Commands"),
        "the line after the bad byte was not processed:\n{s}"
    );
    assert!(
        s.contains("Goodbye"),
        "the session did not end with a clean exit:\n{s}"
    );
    assert!(
        stderr(&out).contains("Input error"),
        "the bad line was skipped silently:\n{}",
        stderr(&out),
    );
}

/// The REPL must see the same config-file layer as `magent run <task>`.
/// It used to short-circuit before `config::load()`, so a provider set in
/// `$XDG_CONFIG_HOME/magent/magent.json` was ignored under `--repl` only.
///
/// `--mock` is deliberately *not* passed here — it would override the
/// provider and hide the bug. `deepseek` without an API key performs no
/// network I/O.
#[test]
fn repl_honours_config_file_provider() {
    let sb = Sandbox::new("cfg");
    let set = sb
        .cmd()
        .args(["config", "set", "provider.default", "deepseek"])
        .output()
        .expect("config set");
    assert!(set.status.success(), "config set failed");

    let out = sb.session_with(&[], "backend\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("deepseek"),
        "REPL ignored the config-file provider (expected deepseek):\n{s}",
    );
}

/// `tools` is generated from the real tool catalog, so it must not
/// advertise a tool the host executor cannot run. `ble_scan` was listed
/// here for a long time but exists nowhere in the codebase.
#[test]
fn repl_tools_lists_only_real_tools() {
    let sb = Sandbox::new("tools");
    let out = sb.session("tools\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    for real in [
        "read_sensor",
        "write_gpio",
        "flash_read",
        "flash_write",
        "ble_send",
    ] {
        assert!(s.contains(real), "tools omitted `{real}`:\n{s}");
    }
    assert!(
        !s.contains("ble_scan"),
        "tools still advertises the fictional `ble_scan`:\n{s}",
    );
}

/// `--quiet` drops the per-step trace on the one-shot path; the REPL must
/// honour it too (it used to install its trace sink unconditionally, so
/// `--repl --quiet` still printed `→ tool(...)` lines to stderr).
#[test]
fn repl_quiet_suppresses_tool_trace() {
    let sb = Sandbox::new("quiet");

    let loud = sb.session_with(&["--mock"], "read the temperature\nq\n");
    assert!(
        stderr(&loud).contains("→ read_sensor"),
        "--mock task produced no trace to compare against:\n{}",
        stderr(&loud),
    );

    let quiet = sb.session_with(&["--mock", "--quiet"], "read the temperature\nq\n");
    assert!(
        !stderr(&quiet).contains("→ read_sensor"),
        "--quiet still emitted the tool trace:\n{}",
        stderr(&quiet),
    );
}

/// The REPL resolves the DeepSeek API key the same way `run "<task>"` does:
/// `--api-key`, then the configured `api_key_env`, then the historical
/// `OLLAMA_API_KEY`. It used to check `--api-key` alone, so a key supplied
/// via `DEEPSEEK_API_KEY` made it announce "no API key" and silently
/// simulate while the identical one-shot invocation connected.
///
/// Network-independent: whatever `check_connection()` says, the banner
/// must not claim the key is missing when one was supplied.
#[test]
fn repl_honours_env_deepseek_api_key() {
    let sb = Sandbox::new("dskey");

    let with = sb.session_env(
        &[("DEEPSEEK_API_KEY", "sk-test-not-used")],
        &["--provider", "deepseek"],
        "q\n",
    );
    assert!(
        !stdout(&with).contains("no API key"),
        "REPL ignored DEEPSEEK_API_KEY:\n{}",
        stdout(&with),
    );

    let without = sb.session_with(&["--provider", "deepseek"], "q\n");
    assert!(
        stdout(&without).contains("no API key"),
        "sanity: without a key the banner should say so:\n{}",
        stdout(&without),
    );
}

/// `env` promises "every variable the CLI consults", but the two passphrase
/// sources `sign_after_run` reads were absent while `MAGENT_AGENT_IDENTITY`
/// — consulted at the same place — was listed. The value of the *indirection*
/// var is a name, so it is shown; the passphrase itself is redacted like the
/// other secrets.
#[test]
fn repl_env_lists_passphrase_vars() {
    let sb = Sandbox::new("envpass");
    let out = sb.session_env(
        &[
            ("MAGENT_WEB3_PASSPHRASE", "hunter2"),
            ("MAGENT_WEB3_PASSPHRASE_ENV", "MY_PASSPHRASE"),
        ],
        &["--mock"],
        "env\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("MAGENT_WEB3_PASSPHRASE_ENV:") && s.contains("MY_PASSPHRASE"),
        "`env` omitted the passphrase-indirection variable:\n{s}",
    );
    assert!(
        s.contains("MAGENT_WEB3_PASSPHRASE:") && s.contains("set (***)"),
        "`env` omitted the passphrase or failed to redact it:\n{s}",
    );
    assert!(
        !s.contains("hunter2"),
        "`env` printed the passphrase in the clear:\n{s}",
    );
}

/// `backend` / `stats` must agree with the welcome banner. Deriving the
/// label from `using_ollama()` made a live DeepSeek session report
/// "mock/deepseek", i.e. indistinguishable from the simulated fallback.
#[test]
fn repl_backend_command_matches_banner() {
    let sb = Sandbox::new("backend");
    let out = sb.session_with(&["--mock"], "backend\nstats\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("🔧 Backend: mock"),
        "`backend` disagreed with the banner:\n{s}",
    );
    assert!(
        !s.contains("mock/deepseek"),
        "`backend`/`stats` still use the old derived label:\n{s}",
    );
}

/// `--repl` is documented as "maintains conversation context across
/// multiple turns" (see the `repl_mode` field and `run --help`). The REPL
/// called `RealAgentRunner::run`, which clears the message history at the
/// top of every call, so each turn was a context-free conversation and
/// `context` reported the same size every time.
#[test]
fn repl_preserves_context_across_tasks() {
    let sb = Sandbox::new("context");
    let out = sb.session("read the temperature\ncontext\nread the humidity\ncontext\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);

    let counts: Vec<usize> = s
        .match_indices("Messages:")
        .map(|(i, _)| number_after(&s[i..], "Messages:"))
        .collect();
    assert_eq!(counts.len(), 2, "expected two `context` reports:\n{s}");
    assert!(
        counts[1] > counts[0],
        "the second task did not see the first turn (Messages {} -> {}):\n{s}",
        counts[0],
        counts[1],
    );
}

/// `prompt` claims to show the system prompt, so it must do that even
/// before the first task — the prompt is installed into the conversation
/// on the first `think()`, so a fresh session reported "(no messages)".
#[test]
fn repl_prompt_shows_the_prompt_before_the_first_task() {
    let sb = Sandbox::new("promptfirst");
    let out = sb.session("prompt\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        !s.contains("(no messages)"),
        "`prompt` reported no messages in a session that has a prompt:\n{s}"
    );
    assert!(
        s.contains("You are mAgent"),
        "`prompt` did not print the configured system prompt:\n{s}"
    );
}

/// `--max-messages` must actually bound a long session. The core slicer
/// short-circuited when a single system prompt plus the task anchor already
/// filled the budget, so `--max-messages 2` dropped nothing and the live
/// context grew turn after turn (a four-task session reached ~17 messages).
#[test]
fn repl_honours_the_max_messages_cap() {
    let sb = Sandbox::new("cap");
    let out = sb.session_with(
        &["--mock", "--max-messages", "2"],
        "task a\ncontext\ntask b\ncontext\ntask c\ncontext\ntask d\ncontext\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);
    let last = s
        .match_indices("Messages:")
        .last()
        .map(|(i, _)| number_after(&s[i..], "Messages:"))
        .expect("no `context` report:\n{s}");
    assert!(
        last <= 4,
        "the cap was not enforced: the live context held {last} messages:\n{s}"
    );
}

/// The tight cap above must not be enforced by pinning the *first* task.
/// With `--max-messages 2` the slicer kept the session's oldest user message
/// (plus the system prompt) and dropped the current one, so from the second
/// turn on the model was asked the stale task. The mock reply is keyword
/// driven, which makes the difference visible: the second task mentions
/// "temperature" and only reaches the temperature branch if *it* survived.
#[test]
fn repl_max_messages_keeps_the_current_task() {
    let sb = Sandbox::new("capanchor");
    let out = sb.session_with(
        &["--mock", "--max-messages", "2"],
        "hello there\nread temperature\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("Temperature sensor reading completed"),
        "the current turn's task never reached the model (the cap kept the \
         session's first task instead):\n{s}"
    );
}

/// The parser refuses `--repl` with the explicit `--json` flag, but
/// `io.json_default = true` reaches the same state without any flag: the
/// REPL writes its transcript straight to stdout and never emits an
/// envelope, so JSON mode would promise JSON and deliver a banner. The
/// runtime guard rejects it before the transcript starts.
#[test]
fn repl_is_rejected_when_config_defaults_to_json() {
    let sb = Sandbox::new("jsondefault");
    // Seed the config the way a user would, through the real command.
    let seeded = sb
        .cmd()
        .args(["config", "set", "io.json_default", "true"])
        .output()
        .expect("run `magent config set`");
    assert!(
        seeded.status.success(),
        "could not seed the config: {}",
        String::from_utf8_lossy(&seeded.stderr),
    );

    let out = sb.session("q\n");
    assert!(
        !out.status.success(),
        "the REPL ran despite JSON output mode:\n{}",
        stdout(&out),
    );
    let err = stderr(&out);
    assert!(
        err.contains("json_default"),
        "the error must name the config key to change:\n{err}"
    );
    assert!(
        !stdout(&out).contains("mAgent REPL"),
        "the banner leaked to stdout before the rejection:\n{}",
        stdout(&out),
    );
}

/// `context` and `stats` must report the context size in bytes. Both used to
/// print `tokens * 4` under a "chars" label — but `approx_tokens` is
/// `bytes / 4`, so that figure is a byte count, and calling it "chars" is
/// wrong for any multi-byte text (~3x over for CJK).
#[test]
fn repl_context_size_is_labelled_bytes() {
    let sb = Sandbox::new("sizeunit");
    let out = sb.session("读取温度\ncontext\nstats\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("Size:") && s.contains("bytes"),
        "the context size was not reported in bytes:\n{s}"
    );
    assert!(
        !s.contains("chars"),
        "the context size is still mislabelled as chars:\n{s}"
    );
}

/// `history` records tasks, not commands. The store only ever receives
/// tasks (built-in commands are never pushed — which is what lets `retry`
/// treat the last entry as the previous task), yet the help text promised
/// "list previous commands" and the listing header said "Command history".
/// Both now say task history, and a built-in command must not appear in the
/// listing.
#[test]
fn repl_history_lists_tasks_not_commands() {
    let sb = Sandbox::new("histlabel");
    let out = sb.session("read the temperature\nhelp\nstats\nhistory\nq\n");
    assert!(out.status.success());
    let s = stdout(&out);

    let listing = s
        .split("Task history:")
        .nth(1)
        .expect("`history` did not print a task-history listing:\n{s}");
    assert!(
        listing.contains("1. read the temperature"),
        "the task was not recorded in the history:\n{s}"
    );
    assert!(
        !listing.contains("2. "),
        "a built-in command was recorded as history:\n{s}"
    );
    assert!(
        !s.contains("Command history") && !s.contains("previous commands"),
        "the help still promises a command history the store does not keep:\n{s}"
    );
}

/// `stats` labels its counters "Total", so they must span the whole
/// session — the runner zeroes its own counters at the top of every
/// `run()`, and the REPL used to print those, showing only the last task.
#[test]
fn repl_stats_accumulates_across_tasks() {
    let sb = Sandbox::new("totals");

    let one = sb.session("read the temperature\nstats\nq\n");
    let one_iters = number_after(&stdout(&one), "Total iterations:");
    let one_tools = number_after(&stdout(&one), "Tool calls:");
    assert!(one_iters > 0, "sanity: a task should cost some iterations");
    assert!(one_tools > 0, "sanity: a task should call a tool");

    let two = sb.session("read the temperature\nread the temperature\nstats\nq\n");
    let s = stdout(&two);
    assert_eq!(
        number_after(&s, "Total iterations:"),
        one_iters * 2,
        "`Total iterations` did not accumulate:\n{s}"
    );
    assert_eq!(
        number_after(&s, "Tool calls:"),
        one_tools * 2,
        "`Tool calls` did not accumulate:\n{s}"
    );
}

/// `clear` writes `ESC[2J` to move the cursor; when stdout is a pipe (as
/// in tests, or `magent run --repl | tee log`) that would just litter the
/// transcript with escape bytes.
#[test]
fn repl_clear_writes_no_escape_when_piped() {
    let sb = Sandbox::new("clear");
    let out = sb.session("clear\nq\n");
    assert!(out.status.success());
    assert!(
        !stdout(&out).contains('\u{1b}'),
        "piped `clear` emitted an escape sequence:\n{:?}",
        stdout(&out),
    );
}

/// The `tools` command lists the optional Email/Blockchain tools, so the
/// model has to be handed them too. The REPL used to build its
/// `RunnerConfig` with an empty `tool_descriptions`, so it advertised those
/// tools in `tools` while the prompt the model actually received never
/// mentioned them — the agent could not have called one.
///
/// `## Available tools` is the header `RunnerConfig::tool_prompt()` is
/// appended under (distinct from the built-in prompt's `Available tools:`),
/// and it is only appended when `tool_descriptions` is non-empty — so its
/// presence is the signal that the REPL installed a catalog at all.
///
/// `--email-tools` is required because that flag is what wires the optional
/// executors: the blockchain backend is auto-wired inside that branch, and
/// without it the executor is `SimulatorOnly` and the REPL rightly tells the
/// model about nothing. The `prompt` command previews only the first 300
/// bytes of the system prompt, so the email tools (listed first) are what
/// lands in view; the blockchain entries are asserted by
/// `build_runner_injects_blockchain_tool_descriptions` in `runner.rs`.
#[cfg(all(feature = "web3", feature = "email-tools"))]
#[test]
fn repl_hands_optional_tools_to_the_model() {
    let sb = Sandbox::new("toolprompt");
    let prompt_file = sb.home.join("tiny.txt");
    std::fs::write(&prompt_file, "Be terse.").unwrap();

    let out = sb.session_with(
        &[
            "--mock",
            "--email-tools",
            "--prompt",
            prompt_file.to_str().unwrap(),
        ],
        "read the temperature\nprompt\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        s.contains("## Available tools"),
        "the REPL never injected the optional tool catalog into the prompt:\n{s}",
    );
    assert!(
        s.contains("mcp__email__list_inbox"),
        "email tools were listed by `tools` but absent from the prompt:\n{s}",
    );
}

/// Without `--email-tools` the executor is `SimulatorOnly`: no blockchain
/// backend exists to route `get_balance` to, so telling the model about it
/// only led it to call a tool that answers with a `ConfigurationError`.
#[cfg(feature = "web3")]
#[test]
fn repl_hides_blockchain_tools_without_email_tools() {
    let sb = Sandbox::new("nooptools");
    let prompt_file = sb.home.join("tiny.txt");
    std::fs::write(&prompt_file, "Be terse.").unwrap();

    let out = sb.session_with(
        &["--mock", "--prompt", prompt_file.to_str().unwrap()],
        "read the temperature\nprompt\nq\n",
    );
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        !s.contains("## Available tools"),
        "no optional executor was wired, yet the model was handed a catalog:\n{s}",
    );
    assert!(
        !s.contains("get_balance"),
        "an unrunnable blockchain tool reached the model's prompt:\n{s}",
    );
}
