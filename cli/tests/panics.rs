//! What the CLI does when it crashes.
//!
//! A panic is a bug in `magent`, not a bad command line, and the two used
//! to be indistinguishable from a script's point of view beyond the exit
//! code: Rust's default hook printed a bare `thread 'main' panicked at …`
//! with no hint that the failure was ours, and under `--json` it left
//! stdout empty — so the "stdout is exactly one JSON document" promise
//! broke exactly when something had already gone wrong.
//!
//! `main` installs a hook that keeps the default report, adds a line that
//! says this is a bug and where to report it, and (in JSON mode) writes an
//! `{"ok":false,"code":"internal_error","exit":101}` envelope. The panic
//! itself is triggered through the `MAGENT_TEST_PANIC` test seam in
//! `main`, which is the only way to reach the hook deterministically.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A throwaway HOME so nothing the binary touches lands in the real one.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home =
            std::env::temp_dir().join(format!("magent-panic-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        Self { home }
    }

    /// A `magent` command that will panic, in the given mode.
    fn cmd(&self, json: bool) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_magent"));
        c.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("MAGENT_TEST_PANIC", "1");
        if json {
            c.arg("--json");
        }
        c.arg("version");
        c
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn run(mut cmd: Command) -> Output {
    cmd.output().expect("spawn magent")
}

/// A crash stays a crash — 101, never quietly promoted to "handled
/// failure" (1) or "your input was bad" (2) — and it says whose fault it
/// is and where to complain.
#[test]
fn a_panic_reports_itself_as_a_bug_and_exits_101() {
    let sb = Sandbox::new("human");
    let out = run(sb.cmd(false));

    assert_eq!(
        out.status.code(),
        Some(101),
        "a panic must exit 101; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("is a bug in magent"),
        "the crash must be diagnosed, not just reported: {err}",
    );
    assert!(
        err.contains("github.com/arkCyber/mAgent/issues"),
        "the report must say where to report it: {err}",
    );
    // The default Rust report is kept alongside ours, so the location and
    // the payload are never lost.
    assert!(err.contains("MAGENT_TEST_PANIC"), "payload lost: {err}");
    assert!(err.contains("panicked"), "location report lost: {err}");
}

/// Under `--json`, a crash must still leave stdout parseable — a CI
/// pipeline that pipes through `jq` should see a structured failure
/// rather than an empty stream, which is exactly what it used to get.
#[test]
fn a_panic_under_json_still_writes_one_error_envelope() {
    let sb = Sandbox::new("json");
    let out = run(sb.cmd(true));

    assert_eq!(out.status.code(), Some(101));

    let text = String::from_utf8_lossy(&out.stdout);
    let values: Vec<serde_json::Value> = serde_json::Deserializer::from_slice(&out.stdout)
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {text:?}"));
    assert_eq!(
        values.len(),
        1,
        "a crash must leave exactly one JSON document on stdout: {text:?}",
    );

    let v = &values[0];
    assert_eq!(v["ok"], serde_json::Value::Bool(false), "got {v}");
    assert_eq!(v["code"], "internal_error", "got {v}");
    assert_eq!(v["exit"], serde_json::json!(101), "got {v}");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("MAGENT_TEST_PANIC")),
        "the envelope must carry the panic message: {v}",
    );
}
