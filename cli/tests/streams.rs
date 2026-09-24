//! Regression guard for the CLI's output-stream convention.
//!
//! **Data goes to stdout; diagnostics go to stderr.** `config where`,
//! `show`, `list` and `get` (and `set-prompt show` / `list`) used to
//! render their human-mode payload through `Output::stderr_fmt_line`, so
//! `magent config show > backup.json` produced a **zero-byte** file while
//! `--json` worked. Each test below drives the real binary in a throwaway
//! HOME and asserts on the two streams separately, which is the only way
//! to observe the bug — the unit tests can't, because `Output` holds the
//! process's real `StdoutLock` / `StderrLock`.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A throwaway HOME so config/prompt files land in a directory we own.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "magent-streams-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        Self { home }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_magent"));
        c.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env_remove("MAGENT_WEB3_PASSPHRASE")
            .env_remove("MAGENT_WEB3_PASSPHRASE_ENV");
        c
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("spawn magent")
}

/// Assert the command succeeded and split its streams for the caller.
fn streams(out: &Output, what: &str) -> (String, String) {
    assert!(
        out.status.success(),
        "{what}: expected success, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The four read-only `config` actions are data: their payload must be
/// redirectable. Before the fix every one of these put its payload on
/// stderr and left stdout empty (0 bytes).
#[test]
fn human_mode_config_data_goes_to_stdout() {
    let sb = Sandbox::new("config-data");
    for args in [
        vec!["config", "where"],
        vec!["config", "show"],
        vec!["config", "list"],
        vec!["config", "get", "provider.default"],
    ] {
        let mut c = sb.cmd();
        c.args(&args);
        let out = run(&mut c);
        let what = format!("magent {}", args.join(" "));
        let (stdout, stderr) = streams(&out, &what);
        assert!(
            !stdout.trim().is_empty(),
            "{what}: stdout must carry the data, got {stdout:?} (stderr: {stderr:?})"
        );
        assert!(
            stderr.trim().is_empty(),
            "{what}: stderr must stay clean in human mode, got {stderr:?}"
        );
    }
}

/// The real user scenario: redirect `config show` to a file and parse it.
/// This is the exact command that produced an empty file.
#[test]
fn config_show_redirect_is_a_parseable_record() {
    let sb = Sandbox::new("config-redirect");
    let mut c = sb.cmd();
    c.args(["config", "show"]);
    let out = run(&mut c);
    let (stdout, _) = streams(&out, "config show");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("`config show > backup.json` must be valid JSON: {e}\n{stdout}")
    });
    assert!(
        value.get("provider").is_some(),
        "the record should have a `provider` section: {value}"
    );
}

/// Confirmations are diagnostics, not data — they belong on stderr, and
/// stdout must stay empty so `magent config set … > file` isn't polluted.
#[test]
fn human_mode_confirmations_stay_on_stderr() {
    let sb = Sandbox::new("config-confirm");
    let mut c = sb.cmd();
    c.args(["config", "set", "provider.default", "ollama"]);
    let out = run(&mut c);
    let (stdout, stderr) = streams(&out, "config set");
    assert_eq!(stdout, "", "a confirmation must not reach stdout");
    assert!(
        !stderr.trim().is_empty(),
        "the confirmation should be visible on stderr"
    );
}

/// `set-prompt show` is documented as pipeable JSON; it used to write to
/// stderr, so `… | jq` got nothing.
#[test]
fn human_mode_set_prompt_show_is_pipeable() {
    let sb = Sandbox::new("prompt-show");
    let mut seed = sb.cmd();
    seed.args(["set-prompt", "set", "demo", "--prompt", "hello prompt"]);
    let seed_out = run(&mut seed);
    assert!(
        seed_out.status.success(),
        "seeding a prompt failed: {}",
        String::from_utf8_lossy(&seed_out.stderr)
    );

    let mut c = sb.cmd();
    c.args(["set-prompt", "show", "demo"]);
    let out = run(&mut c);
    let (stdout, stderr) = streams(&out, "set-prompt show");
    assert!(
        stderr.trim().is_empty(),
        "stderr must stay clean, got {stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`set-prompt show` must be valid JSON: {e}\n{stdout}"));
    assert_eq!(value["prompt"], "hello prompt");

    // `list` is the same family — a data table on stdout.
    let mut c = sb.cmd();
    c.args(["set-prompt", "list"]);
    let out = run(&mut c);
    let (stdout, _) = streams(&out, "set-prompt list");
    assert!(
        stdout.contains("demo"),
        "the list table should name the stored prompt: {stdout:?}"
    );
}

/// `--json` is unchanged: exactly one JSON object on stdout, nothing on
/// stderr. This is the invariant the human-mode fix had to preserve.
#[test]
fn json_mode_puts_only_the_envelope_on_stdout() {
    let sb = Sandbox::new("json-purity");
    let mut c = sb.cmd();
    c.args(["--json", "config", "show"]);
    let out = run(&mut c);
    let (stdout, stderr) = streams(&out, "config show --json");
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`--json` stdout must be one JSON object: {e}\n{stdout}"));
    assert!(value.is_object(), "envelope must be an object, got {value}");
    assert!(
        stderr.is_empty(),
        "--json mode must keep stderr empty, got {stderr:?}"
    );
}
