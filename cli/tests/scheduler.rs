//! End-to-end guard for `magent scheduler run-once`.
//!
//! `run-once` is the unattended entry point — CI runs it, nobody reads
//! the transcript — so two things have to be true: the exit code has to
//! distinguish "the queue ran" from "the queue failed", and the recorded
//! reason has to name the task that failed. It used to exit 0 with a
//! `finished N task(s)` line no matter what, printing nothing at all for
//! the failed task, and record the cause as a *state file* problem.
//!
//! Every task here fails through `--prompt-name <missing>`, which is a
//! deterministic offline failure of the real `magent run` subprocess —
//! no backend, no network, no timing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway HOME so the scheduler state file lands in a directory we
/// own and delete afterwards.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "magent-sched-it-{tag}-{}-{:?}",
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
            .env_remove("MAGENT_SCHEDULER_STATE");
        c
    }

    /// Write a tasks file into the sandbox and return its path.
    fn tasks(&self, name: &str, body: &str) -> PathBuf {
        let p = self.home.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    /// The state file the scheduler resolves under this sandbox's HOME.
    fn state_path(&self) -> PathBuf {
        self.home
            .join(".local")
            .join("state")
            .join("magent")
            .join("scheduler.json")
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read_state(path: &Path) -> serde_json::Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("no state file at {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("state file is not JSON: {e}"))
}

/// A queue where every task fails must not look like a clean run.
#[test]
fn run_once_exits_non_zero_when_a_task_fails() {
    let sb = Sandbox::new("fail");
    let tasks = sb.tasks(
        "tasks.json",
        r#"{"tasks":[
             {"name":"ok","task":"say hi","mock":true},
             {"name":"bad","task":"say hi","mock":true,"prompt_name":"nosuch"}
           ]}"#,
    );

    let out = run(sb.cmd().args([
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));

    assert_eq!(
        out.status.code(),
        Some(1),
        "run-once must exit 1 when a task failed; stderr:\n{}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains("1 of 2 scheduled task(s) failed"),
        "the failure was not summarised:\n{err}"
    );
    // The failing task names itself and carries the child's real error
    // line, not the `[Config] applied …` preamble it arrives wrapped in.
    assert!(
        err.contains("\"bad\" FAILED: exit 2: error: could not load system prompt"),
        "the transcript does not say which task failed, or why:\n{err}"
    );
    assert!(
        err.contains("finished 2 task(s), 1 FAILED"),
        "the summary line hides the failure:\n{err}"
    );
}

/// A clean queue must still exit 0 — otherwise the signal above is noise.
#[test]
fn run_once_exits_zero_when_every_task_succeeds() {
    let sb = Sandbox::new("ok");
    let tasks = sb.tasks(
        "tasks.json",
        r#"{"tasks":[{"name":"a","task":"say hi","mock":true}]}"#,
    );

    let out = run(sb.cmd().args([
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));

    assert_eq!(
        out.status.code(),
        Some(0),
        "a successful run must exit 0; stderr:\n{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("finished 1 task(s);"),
        "unexpected summary:\n{}",
        stderr(&out)
    );
}

/// A task whose text begins with `-` must reach the agent, not the child's
/// option parser. `task="--help"` used to print usage and exit 0.
#[test]
fn run_once_keeps_dash_leading_tasks_as_text() {
    let sb = Sandbox::new("dash");
    let tasks = sb.tasks(
        "tasks.json",
        r#"{"tasks":[{"name":"h","task":"--help","mock":true}]}"#,
    );
    let out = run(sb.cmd().args([
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr:\n{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("USAGE:") && !stdout(&out).contains("USAGE:"),
        "the child printed help instead of running the task:\n{}{}",
        stdout(&out),
        stderr(&out)
    );
}

/// The state file is the durable record of *why* a task failed, so it
/// must blame the task — not the state file, which is healthy — and
/// carry the schema version the daemon claims to write.
#[test]
fn state_file_records_a_task_failure_against_the_task() {
    let sb = Sandbox::new("state");
    let tasks = sb.tasks(
        "tasks.json",
        r#"{"tasks":[{"name":"bad","task":"say hi","mock":true,"prompt_name":"nosuch"}]}"#,
    );

    let _ = run(sb.cmd().args([
        "scheduler",
        "run-once",
        "--tasks-file",
        tasks.to_str().unwrap(),
    ]));

    let state = read_state(&sb.state_path());
    assert_eq!(
        state["schema_version"],
        serde_json::json!(1),
        "a freshly written state file must carry the current schema version: {state}"
    );
    let entry = &state["tasks"]["bad"];
    assert_eq!(entry["failure_count"], serde_json::json!(1), "got {state}");
    let last_error = entry["last_error"].as_str().expect("last_error recorded");
    assert!(
        last_error.contains("scheduled task \"bad\" failed"),
        "the error does not name the task: {last_error}"
    );
    assert!(
        !last_error.contains("scheduler state file"),
        "a task failure was reported as a state-file problem: {last_error}"
    );
    assert!(
        last_error.contains("could not load system prompt"),
        "the recorded error lost its cause: {last_error}"
    );
}

/// The `--json` envelope is what a pipeline reads instead of the
/// transcript, so it has to carry the failure count too.
#[test]
fn json_envelope_reports_the_failed_count() {
    let sb = Sandbox::new("json");
    let tasks = sb.tasks(
        "tasks.json",
        r#"{"tasks":[
             {"name":"ok","task":"say hi","mock":true},
             {"name":"bad","task":"say hi","mock":true,"prompt_name":"nosuch"}
           ]}"#,
    );

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
        "the exit code must not depend on --json; stderr:\n{}",
        stderr(&out)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e})"));
    assert_eq!(v["kind"], "scheduler.run_once", "got {v}");
    assert_eq!(v["ran"], serde_json::json!(2), "got {v}");
    assert_eq!(v["failed"], serde_json::json!(1), "got {v}");
    // The envelope is still emitted despite the non-zero exit: a CI
    // caller that pipes through `jq` must not get an empty stream.
    assert!(
        stdout(&out).contains("\"state_path\""),
        "the envelope was truncated:\n{}",
        stdout(&out)
    );
}
