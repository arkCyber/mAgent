//! Regression guard: a command that succeeds under `--json` must put a
//! machine-readable payload on stdout.
//!
//! A cluster of commands used to return rc=0 with **zero bytes** on
//! both streams in JSON mode — either because they wrote through
//! `Output::write_human` (a no-op in JSON mode) or because they had no
//! JSON branch at all. A CI pipeline doing `magent --json … | jq` then
//! saw an empty stream and either errored or silently processed
//! nothing. Each test below drives the real binary in a throwaway HOME
//! so the fix can't regress.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A throwaway HOME so the binary's vault/summary/state files land in a
/// directory we own and delete afterwards.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "magent-it-{tag}-{}-{:?}",
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
            .env_remove("MAGENT_WEB3_PASSPHRASE")
            .env_remove("MAGENT_WEB3_PASSPHRASE_ENV");
        c
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.join(name)
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

/// How many complete JSON values are on stdout.
///
/// `--json` promises stdout is *exactly one* document, so counting is
/// how a test catches a second envelope being appended after the first
/// (`jq` reads only the first, or errors). `serde_json::from_slice` on
/// the whole stream would accept a trailing object, so it can't see this.
fn count_json_values(bytes: &[u8]) -> usize {
    let de = serde_json::Deserializer::from_slice(bytes);
    de.into_iter::<serde_json::Value>().count()
}

/// Assert stdout holds exactly one JSON object and return it.
fn one_json_object(out: &Output, what: &str) -> serde_json::Value {
    let n = count_json_values(&out.stdout);
    assert_eq!(
        n,
        1,
        "{what}: stdout must be exactly one JSON document, found {n}:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{what}: stdout is not valid JSON ({e})"))
}

/// A failing command under `--json` used to write *nothing* to stdout, so
/// `magent --json … | jq` got a parse error instead of the failure the
/// pipeline was trying to detect. It now emits a structured envelope —
/// and the human `error:` line still reaches stderr, so an operator
/// running `--json` by hand is not staring at an empty terminal.
#[test]
fn json_failure_emits_a_single_error_envelope() {
    let sb = Sandbox::new("errjson");
    let out = run(sb.cmd().args(["--json", "set-prompt", "show", "nosuch"]));

    assert!(
        !out.status.success(),
        "`set-prompt show nosuch` must fail, got exit 0",
    );
    let v = one_json_object(&out, "set-prompt show --json (missing)");
    assert_eq!(v["ok"], serde_json::Value::Bool(false), "got {v}");
    assert_eq!(v["code"], "set_prompt_error", "got {v}");
    assert_eq!(v["exit"], serde_json::json!(2), "got {v}");
    assert!(
        v["error"].as_str().is_some_and(|e| e.contains("nosuch")),
        "the envelope must carry the diagnostic: {v}",
    );

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error:") && err.contains("nosuch"),
        "the human diagnostic must survive on stderr: {err:?}",
    );
}

/// Human mode is unchanged: the `error:` line on stderr, nothing on
/// stdout — the envelope is a `--json`-only affordance.
#[test]
fn human_failure_still_writes_nothing_to_stdout() {
    let sb = Sandbox::new("errhuman");
    let out = run(sb.cmd().args(["set-prompt", "show", "nosuch"]));

    assert!(!out.status.success(), "expected failure");
    assert!(
        out.stdout.is_empty(),
        "human mode must not write an envelope: {:?}",
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("error:"),
        "stderr should carry the diagnostic",
    );
}

/// `scheduler run-once` writes its result envelope *and* returns
/// `TasksFailed`. The two together used to put two JSON objects on
/// stdout, which `jq` cannot read. The envelope is now the single
/// self-describing document: it carries the failure fields itself, and
/// the envelope latch stops `Output::fail` from appending a second one.
#[test]
fn json_scheduler_failure_is_one_self_describing_envelope() {
    let sb = Sandbox::new("schedfail");
    let tasks = sb.path("tasks.json");
    std::fs::write(
        &tasks,
        r#"{"tasks":[{"name":"bad","task":"say hi","mock":true,"prompt_name":"nosuch"}]}"#,
    )
    .unwrap();

    let out = run(sb.cmd().args([
        "--json",
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));

    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = one_json_object(&out, "scheduler run-once --json (failing)");
    assert_eq!(v["ok"], serde_json::Value::Bool(false), "got {v}");
    assert_eq!(v["code"], "scheduler_tasks_failed", "got {v}");
    assert_eq!(v["exit"], serde_json::json!(1), "got {v}");
    assert_eq!(v["failed"], serde_json::json!(1), "got {v}");
    assert_eq!(v["kind"], "scheduler.run_once", "got {v}");

    // The recorded cause is the child's error, not the ~380-char
    // `[Config] applied …` line every `magent run` prints first — that
    // preamble used to eat the record's whole truncation budget.
    let last_error = v["tasks"]["bad"]["last_error"]
        .as_str()
        .expect("last_error recorded");
    assert!(
        last_error.contains("could not load system prompt"),
        "the cause must survive: {last_error}",
    );
    assert!(
        !last_error.contains("[Config] applied"),
        "the config preamble is noise, not the cause: {last_error}",
    );
}

fn assert_json_stdout(out: &Output, what: &str) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{what}: expected success, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !out.stdout.is_empty(),
        "{what}: `--json` wrote zero bytes to stdout (the bug this guards)",
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{what}: stdout is not valid JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout),
        )
    })
}

/// Seed a real summary record by driving `run --mock --save-summary`,
/// which is the only supported way to create one without hand-rolling a
/// `SummaryRecord` JSON (its `stats`/`source` fields are non-optional).
fn seed_summary(sb: &Sandbox, topic: &str) {
    let out = run(sb
        .cmd()
        .args(["run", "--mock", "--save-summary", topic, "hello"]));
    assert!(
        out.status.success(),
        "seed `run --mock --save-summary {topic}` failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn summary_export_load_delete_emit_json_in_json_mode() {
    let sb = Sandbox::new("summary");
    seed_summary(&sb, "t1");

    // export: its job is to feed another command, so the payload must
    // reach stdout in *both* modes (`write_stdout_line`).
    let export = run(sb.cmd().args(["--json", "summary", "export", "t1"]));
    let v = assert_json_stdout(&export, "summary export --json");
    assert!(v.is_object(), "export should emit a record object, got {v}");

    // load: same contract — the head/tail window is the payload.
    let load = run(sb.cmd().args(["--json", "summary", "load", "t1"]));
    assert_json_stdout(&load, "summary load --json");

    // delete: must report the outcome, not go silent.
    let del = run(sb.cmd().args(["--json", "summary", "delete", "t1"]));
    let v = assert_json_stdout(&del, "summary delete --json");
    assert_eq!(v["deleted"], serde_json::Value::Bool(true), "got {v}");
}

/// `summary rollback` had only a Human-mode branch, so under `--json` it
/// exited 0 with zero bytes on stdout.
#[test]
fn summary_rollback_emits_json_in_json_mode() {
    let sb = Sandbox::new("rollback");
    seed_summary(&sb, "t1");

    // Re-saving an exported record with --overwrite merges with the previous
    // snapshot and therefore creates one history entry to roll back to.
    let export = run(sb.cmd().args(["summary", "export", "t1"]));
    assert!(export.status.success());
    let rec = sb.path("rec.json");
    std::fs::write(&rec, &export.stdout).unwrap();
    let save = run(sb.cmd().args([
        "summary",
        "save",
        "t1",
        "--from",
        rec.to_str().unwrap(),
        "--overwrite",
    ]));
    assert!(
        save.status.success(),
        "seed with history failed: {}",
        String::from_utf8_lossy(&save.stderr)
    );

    let out = run(sb.cmd().args(["--json", "summary", "rollback", "t1", "0"]));
    let v = assert_json_stdout(&out, "summary rollback --json");
    assert_eq!(v["topic"], "t1", "got {v}");
    assert_eq!(v["rolled_back_to_index"], 0, "got {v}");
}

/// `run --save-summary-overwrite` used to delete the previous record before
/// saving, so run-produced summaries never accumulated history and
/// `summary rollback` could never target them.
#[test]
fn run_overwrite_keeps_history_for_rollback() {
    let sb = Sandbox::new("runhist");
    seed_summary(&sb, "t1");
    let second = run(sb.cmd().args([
        "run",
        "--mock",
        "--save-summary",
        "t1",
        "--save-summary-overwrite",
        "again",
    ]));
    assert!(
        second.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let rollback = run(sb.cmd().args(["--json", "summary", "rollback", "t1", "0"]));
    let v = assert_json_stdout(&rollback, "rollback after run --save-summary-overwrite");
    assert_eq!(v["topic"], "t1", "got {v}");
    assert_eq!(v["rolled_back_to_index"], 0, "got {v}");
}

#[test]
fn scheduler_run_once_emits_envelope_for_empty_task_list() {
    let sb = Sandbox::new("sched");
    let tasks = sb.path("tasks.json");
    std::fs::write(&tasks, r#"{"tasks":[]}"#).unwrap();

    let out = run(sb.cmd().args([
        "--json",
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));
    let v = assert_json_stdout(&out, "scheduler run-once --json (empty)");
    assert_eq!(v["ran"], serde_json::json!(0), "got {v}");
    assert_eq!(v["kind"], "scheduler.run_once", "got {v}");
}

/// Write a non-empty payload file and return its path.
#[cfg(feature = "web3")]
fn payload_file(sb: &Sandbox, name: &str, bytes: &[u8]) -> PathBuf {
    let p = sb.path(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[cfg(feature = "web3")]
#[test]
fn web3_verify_and_delete_emit_json_in_json_mode() {
    let sb = Sandbox::new("web3");
    const PASS_VAR: &str = "MAGENT_IT_PASS";

    let created = run(sb.cmd().env(PASS_VAR, "it-passphrase").args([
        "web3",
        "new",
        "signer",
        "--passphrase-env",
        PASS_VAR,
    ]));
    assert!(
        created.status.success(),
        "web3 new failed: {}",
        String::from_utf8_lossy(&created.stderr),
    );

    let payload = payload_file(&sb, "payload.txt", b"payload for signing\n");
    let envelope = sb.path("env.json");
    let signed = run(sb.cmd().env(PASS_VAR, "it-passphrase").args([
        "web3",
        "sign",
        "signer",
        "--payload",
        payload.to_str().unwrap(),
        "--output",
        envelope.to_str().unwrap(),
        "--passphrase-env",
        PASS_VAR,
    ]));
    assert!(
        signed.status.success(),
        "web3 sign failed: {}",
        String::from_utf8_lossy(&signed.stderr),
    );

    // sign --output under `--json` used to write nothing to stdout; the
    // envelope goes to the file, so a confirmation must take its place.
    let signed_json = run(sb.cmd().env(PASS_VAR, "it-passphrase").args([
        "--json",
        "web3",
        "sign",
        "signer",
        "--payload",
        payload.to_str().unwrap(),
        "--output",
        sb.path("env2.json").to_str().unwrap(),
        "--passphrase-env",
        PASS_VAR,
    ]));
    let v = assert_json_stdout(&signed_json, "web3 sign --output --json");
    assert_eq!(v["action"], "sign", "got {v}");

    // verify: used to emit nothing at all under `--json`.
    let verify = run(sb.cmd().args([
        "--json",
        "web3",
        "verify",
        "--payload",
        payload.to_str().unwrap(),
        "--envelope",
        envelope.to_str().unwrap(),
    ]));
    let v = assert_json_stdout(&verify, "web3 verify --json");
    assert_eq!(v["ok"], serde_json::Value::Bool(true), "got {v}");

    // delete: same.
    let del = run(sb.cmd().args(["--json", "web3", "delete", "signer"]));
    let v = assert_json_stdout(&del, "web3 delete --json");
    assert_eq!(v["removed"], serde_json::Value::Bool(true), "got {v}");
}

/// `run --sign --passphrase-env <VAR>` must resolve the passphrase from
/// the named variable. The parser used to reject `--passphrase-env`
/// with `unknown flag`, even though the signing code documented it.
#[cfg(feature = "web3_app")]
#[test]
fn run_sign_accepts_passphrase_env_flag() {
    let sb = Sandbox::new("runsign");
    const PASS_VAR: &str = "MAGENT_IT_PASS";
    let pass_value = "it-passphrase";

    let created = run(sb.cmd().env(PASS_VAR, pass_value).args([
        "web3",
        "new",
        "signer",
        "--passphrase-env",
        PASS_VAR,
    ]));
    assert!(
        created.status.success(),
        "web3 new failed: {}",
        String::from_utf8_lossy(&created.stderr),
    );

    let signed_out = sb.path("signed.json");
    let out = run(sb.cmd().env(PASS_VAR, pass_value).args([
        "run",
        "--mock",
        "--sign",
        "--signer",
        "signer",
        "--passphrase-env",
        PASS_VAR,
        "--signed-output",
        signed_out.to_str().unwrap(),
        "hello",
    ]));
    assert!(
        out.status.success(),
        "run --sign failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        signed_out.exists(),
        "signed envelope was not written to {}",
        signed_out.display(),
    );

    // Without the flag and without the env var, signing must fail with a
    // non-zero exit rather than silently producing an unsigned run.
    let missing = run(sb
        .cmd()
        .args(["run", "--mock", "--sign", "--signer", "signer", "hello"]));
    assert!(
        !missing.status.success(),
        "run --sign without a passphrase should fail",
    );
}

/// `magent --json version` used to print the bare text `magent 0.1.0`,
/// which is not JSON at all.
#[test]
fn version_emits_valid_json_in_json_mode() {
    let sb = Sandbox::new("version");
    let out = run(sb.cmd().args(["--json", "version"]));
    let v = assert_json_stdout(&out, "version --json");
    assert!(v["version"].is_string(), "got {v}");

    // Human mode is unchanged.
    let human = run(sb.cmd().args(["version"]));
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).starts_with("magent "));
}

/// `config export` is the redirect-safe counterpart to `config show`
/// (whose human output goes to stderr), so it must write to stdout in
/// *both* modes.
#[test]
fn config_export_writes_stdout_in_both_modes() {
    let sb = Sandbox::new("cfgexport");
    let init = run(sb.cmd().args(["config", "init"]));
    assert!(
        init.status.success(),
        "config init failed: {}",
        String::from_utf8_lossy(&init.stderr),
    );

    let human = run(sb.cmd().args(["config", "export"]));
    assert!(
        human.status.success() && !human.stdout.is_empty(),
        "config export (human) produced no stdout: {}",
        String::from_utf8_lossy(&human.stderr),
    );
    let v: serde_json::Value =
        serde_json::from_slice(&human.stdout).expect("config export stdout must be JSON");
    assert!(v["provider"].is_object(), "got {v}");

    let json = run(sb.cmd().args(["--json", "config", "export"]));
    assert_json_stdout(&json, "config export --json");
}

/// `set-prompt export` keeps its raw-text contract in human mode but
/// must emit valid JSON under `--json`.
#[test]
fn set_prompt_export_emits_valid_json_in_json_mode() {
    let sb = Sandbox::new("spx");
    let set = run(sb
        .cmd()
        .args(["set-prompt", "set", "p1", "--prompt", "You are terse."]));
    assert!(
        set.status.success(),
        "set-prompt set failed: {}",
        String::from_utf8_lossy(&set.stderr),
    );

    // Human: raw text, byte-for-byte.
    let human = run(sb.cmd().args(["set-prompt", "export", "p1"]));
    assert_eq!(String::from_utf8_lossy(&human.stdout), "You are terse.");

    // JSON: a parseable envelope carrying the same text.
    let json = run(sb.cmd().args(["--json", "set-prompt", "export", "p1"]));
    let v = assert_json_stdout(&json, "set-prompt export --json");
    assert_eq!(v["prompt"], "You are terse.", "got {v}");
}

/// The REPL streams an interactive transcript straight to stdout, so
/// `--json` cannot be honoured. Rather than emit prose under `--json`
/// (breaking the "stdout is always valid JSON" contract), the CLI
/// rejects the combination up front.
#[test]
fn repl_is_rejected_under_json_mode() {
    let sb = Sandbox::new("repljson");
    let out = run(sb.cmd().args(["--json", "run", "--repl", "--mock"]));
    assert!(
        !out.status.success(),
        "`--json run --repl` must fail; got success with stdout: {}",
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        out.stdout.is_empty(),
        "nothing must reach stdout under --json; got: {}",
        String::from_utf8_lossy(&out.stdout),
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--repl") && err.contains("--json"),
        "error should name both flags; got: {err}",
    );
}

/// `--repl` takes its input interactively, so a positional task would be
/// silently dropped. It is now a usage error.
#[test]
fn repl_with_task_is_rejected() {
    let sb = Sandbox::new("repltask");
    let out = run(sb.cmd().args(["run", "--repl", "--mock", "read the temp"]));
    assert!(
        !out.status.success(),
        "`run --repl <TASK>` must fail; got stdout: {}",
        String::from_utf8_lossy(&out.stdout),
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--repl"), "got: {err}");
}
