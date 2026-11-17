//! Regression guard for the CLI's *error-reporting* contract.
//!
//! These tests cover the paths where a command used to report success
//! for work it never did:
//!
//! * a failed write to stdout (full disk, closed stream) exited 0 with
//!   the payload missing;
//! * files in the prompt store that could not be listed vanished from
//!   `set-prompt list` without a word;
//! * `--prompt -` was documented as "read stdin" but failed with
//!   `-: No such file or directory`;
//! * `doctor` under `provider = deepseek` read `OLLAMA_MODEL`, and its
//!   per-check `ok` disagreed with the overall verdict for a skipped
//!   check.
//!
//! Every test drives the real binary in a throwaway HOME.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A throwaway HOME so config/prompt files land in a directory we own.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "magent-errpaths-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join("prompts")).unwrap();
        Self { home }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_magent"));
        c.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("MAGENT_PROMPTS_DIR", self.home.join("prompts"))
            // The developer's own environment must not leak into these
            // assertions (see `doctor` model resolution below).
            .env_remove("MAGENT_PROVIDER")
            .env_remove("OLLAMA_MODEL")
            .env_remove("OLLAMA_HOST")
            .env_remove("DEEPSEEK_MODEL")
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("OLLAMA_API_KEY");
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

/// A file in the prompt store that cannot be parsed as a prompt must be
/// reported, not silently dropped: a user who sees their prompt missing
/// from `list` will conclude it was deleted.
#[test]
fn set_prompt_list_reports_files_it_could_not_parse() {
    let sb = Sandbox::new("skipped");
    std::fs::write(sb.path("prompts/real.json"), prompt_record("real")).unwrap();
    std::fs::write(sb.path("prompts/half.json"), "{\"broken\":").unwrap();
    std::fs::write(
        sb.path("prompts/future.json"),
        r#"{"schema_version":99,"name":"future","prompt":"x"}"#,
    )
    .unwrap();

    let out = run(sb.cmd().args(["set-prompt", "list"]));
    assert!(out.status.success(), "listing must not fail outright");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("half.json"),
        "an unparseable prompt must be reported: {stderr:?}"
    );
    assert!(
        stderr.contains("schema_version 99"),
        "a too-new prompt must say why it was skipped: {stderr:?}"
    );

    // The signing artifact is a *known* non-record; warning about it on
    // every list would drown the warnings that matter. The filter keys on
    // the envelope's own fields (`payload_type` + `signature_hex`), not on
    // the file name — a prompt genuinely named `real.signed` would otherwise
    // be hidden.
    std::fs::write(
        sb.path("prompts/real.signed.json"),
        r#"{"payload_type":"magent-prompt-v1","issued_at_unix":1,"payload":{},"signer":"did:key:z","signature_hex":"00"}"#,
    )
    .unwrap();
    let out = run(sb.cmd().args(["set-prompt", "list"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("real.signed.json"),
        "`.signed.json` envelopes must be filtered silently: {stderr:?}"
    );
}

/// The same skip information must reach a `--json` consumer, where there
/// is no stderr line to notice.
#[test]
fn set_prompt_list_json_carries_skipped_entries() {
    let sb = Sandbox::new("skippedjson");
    std::fs::write(sb.path("prompts/real.json"), prompt_record("real")).unwrap();
    std::fs::write(sb.path("prompts/half.json"), "{\"broken\":").unwrap();

    let out = run(sb.cmd().args(["--json", "set-prompt", "list"]));
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout must be one JSON object: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(v["prompts"].as_array().map(|a| a.len()), Some(1), "got {v}");
    let skipped = v["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("`skipped` must be reported in JSON mode: {v}"));
    assert!(
        skipped
            .iter()
            .any(|s| s.as_str().is_some_and(|s| s.contains("half.json"))),
        "the skipped file must be named: {v}"
    );
}

/// `--prompt -` is the documented way to pipe a system prompt in. It
/// used to be treated as a literal path, so it failed with ENOENT.
#[test]
fn run_accepts_a_system_prompt_on_stdin() {
    let sb = Sandbox::new("stdinprompt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_magent"))
        .args(["run", "--mock", "-p", "-", "hi"])
        .env("HOME", &sb.home)
        .env("XDG_CONFIG_HOME", sb.home.join(".config"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn magent");
    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        stdin
            .write_all(b"You are a terse assistant.\n")
            .expect("write the prompt");
    }
    let out = child.wait_with_output().expect("wait for magent");
    assert!(
        out.status.success(),
        "`magent run -p -` must read the prompt from stdin: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("No such file or directory"),
        "the prompt must not be resolved as a file"
    );
}

/// `set-prompt set --prompt -` uses the same reader.
#[test]
fn set_prompt_accepts_a_body_on_stdin() {
    let sb = Sandbox::new("stdinbody");
    let mut child = Command::new(env!("CARGO_BIN_EXE_magent"))
        .args(["set-prompt", "set", "piped", "--prompt", "-"])
        .env("HOME", &sb.home)
        .env("MAGENT_PROMPTS_DIR", sb.path("prompts"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn magent");
    {
        use std::io::Write as _;
        child
            .stdin
            .as_mut()
            .expect("stdin pipe")
            .write_all(b"body from stdin\n")
            .expect("write the body");
    }
    let out = child.wait_with_output().expect("wait for magent");
    assert!(
        out.status.success(),
        "`--prompt -` must be accepted: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let shown = run(sb.cmd().args(["set-prompt", "show", "piped"]));
    let v: serde_json::Value = serde_json::from_slice(&shown.stdout).expect("show emits JSON");
    assert_eq!(v["prompt"], "body from stdin");
}

/// Under `provider = deepseek`, `doctor` must check the *configured*
/// DeepSeek model. It used to read `OLLAMA_MODEL` and the Ollama model
/// slot, so an unrelated Ollama setting decided which DeepSeek model
/// was reported (and `network = unreachable` hid the mistake).
#[test]
fn doctor_reports_the_provider_specific_model() {
    let sb = Sandbox::new("doctormodel");
    let cfg = sb.path("magent.json");
    std::fs::write(
        &cfg,
        r#"{
          "schema_version": 1,
          "provider": {
            "default": "deepseek",
            "ollama": {"url":"http://127.0.0.1:1","model":"llama3.2","api_key_env":"OLLAMA_API_KEY"},
            "deepseek": {"url":"http://127.0.0.1:1/v1","model":"deepseek-reasoner","api_key_env":"DEEPSEEK_API_KEY"}
          }
        }"#,
    )
    .unwrap();

    let out = run(sb
        .cmd()
        .env("MAGENT_CONFIG_FILE", &cfg)
        .env("DEEPSEEK_API_KEY", "sk-not-a-real-key")
        .env("OLLAMA_MODEL", "llama3.2")
        .args(["--json", "doctor"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json must emit one object: {e}"));
    assert_eq!(
        v["checks"]["model"]["name"], "deepseek-reasoner",
        "the DeepSeek model slot must win over OLLAMA_MODEL: {v}"
    );

    // `DEEPSEEK_MODEL` is the matching override.
    let out = run(sb
        .cmd()
        .env("MAGENT_CONFIG_FILE", &cfg)
        .env("DEEPSEEK_API_KEY", "sk-not-a-real-key")
        .env("DEEPSEEK_MODEL", "ds-override")
        .args(["--json", "doctor"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["checks"]["model"]["name"], "ds-override", "got {v}");
}

/// A skipped check is not a failure: `ok` must agree with the top-level
/// verdict for the provider that does not use that check.
#[test]
fn doctor_json_ok_agrees_with_the_verdict_for_skipped_checks() {
    let sb = Sandbox::new("doctorskip");
    let out = run(sb
        .cmd()
        .env("MAGENT_PROVIDER", "deepseek")
        .env("DEEPSEEK_API_KEY", "sk-not-a-real-key")
        .env("DEEPSEEK_HOST", "http://127.0.0.1:1/v1")
        .args(["--json", "doctor"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json must emit one object: {e}"));
    for check in ["ollama", "model"] {
        let c = &v["checks"][check];
        assert_eq!(
            c["skipped"], true,
            "{check} does not apply to deepseek: {v}"
        );
        assert_eq!(
            c["ok"], true,
            "{check} is skipped, so it must not read as a failure: {v}"
        );
    }
}

/// A corrupt config file must not get a green `doctor`.
///
/// `run` refuses to start on this file (exit 2). `doctor` used to fall back
/// to built-in defaults and exit 0 with `"ok": true` — the two subcommands
/// disagreeing about the very configuration `doctor` is supposed to vet.
#[test]
fn doctor_fails_on_a_config_file_run_refuses() {
    let sb = Sandbox::new("doctorcfg");
    let cfg = sb.path("magent.json");
    std::fs::write(&cfg, "{\"broken\":").unwrap();

    let doctor = run(sb
        .cmd()
        .env("MAGENT_CONFIG_FILE", &cfg)
        .env("MAGENT_PROVIDER", "deepseek")
        .env("DEEPSEEK_API_KEY", "sk-not-a-real-key")
        .args(["--json", "doctor"]));
    assert_eq!(
        doctor.status.code(),
        Some(2),
        "an unusable config is rejected input, not a check result: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|e| panic!("doctor must still emit one JSON object: {e}"));
    assert_eq!(v["ok"], false, "a broken config is not healthy: {v}");
    assert_eq!(v["code"], "config_error", "got {v}");

    // `run` agrees: the same file is exit 2.
    let run_out = run(sb
        .cmd()
        .env("MAGENT_CONFIG_FILE", &cfg)
        .args(["run", "--mock", "hi"]));
    assert_eq!(
        run_out.status.code(),
        Some(2),
        "run and doctor must agree about the same config file: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
}

/// A prompt record as the store writes it.
fn prompt_record(name: &str) -> String {
    format!(
        r#"{{"schema_version":1,"name":"{name}","prompt":"hello","provider":"","model":""
,"metadata":{{"tags":[]}},
"created_at":1,"updated_at":1}}"#
    )
}

/// A stdout write that fails (disk full, closed stream) must produce a
/// non-zero exit and a diagnostic — not exit 0 with the payload missing.
///
/// `config export` is the sharpest case: its whole job is to hand the
/// record to the next process, and every writer on that path used to
/// discard the `io::Result`.
#[test]
fn a_failed_stdout_write_exits_non_zero() {
    let sb = Sandbox::new("stdoutfail");
    let target = sb.path("out.json");

    // `RLIMIT_FSIZE = 0` makes every write to a *regular file* fail with
    // EFBIG. `SIGXFSZ` then has to be ignored, or the kernel would kill
    // the child before it could report anything.
    let mut cmd = sb.cmd();
    cmd.args(["config", "export"]);
    let out = match run_with_fsize_limit(cmd, &target) {
        Some(o) => o,
        // The helper cannot express the limit on this platform; don't
        // fail the suite over a missing capability.
        None => return,
    };

    assert!(
        !out.status.success(),
        "a failed payload write must not exit 0 (stdout: {:?})",
        std::fs::read_to_string(&target).unwrap_or_default(),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not write") && stderr.contains("stdout"),
        "the failure must be explained on stderr: {stderr:?}"
    );
}

/// Run `cmd` with stdout redirected to `target` while `RLIMIT_FSIZE` is
/// zeroed in the child. Returns `None` when the platform cannot express
/// it (there is no POSIX `setrlimit` on Windows).
#[cfg(unix)]
fn run_with_fsize_limit(mut cmd: Command, target: &std::path::Path) -> Option<Output> {
    use std::os::unix::process::CommandExt as _;
    let file = std::fs::File::create(target).ok()?;
    // SAFETY: `setrlimit` only touches this process's own rlimits, and
    // it runs in the child after `fork` / before `exec` (the documented
    // contract of `CommandExt::pre_exec`).
    unsafe {
        cmd.pre_exec(|| {
            let lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            Ok(())
        });
    }
    cmd.stdout(std::process::Stdio::from(file));
    cmd.stderr(std::process::Stdio::piped());
    cmd.output().ok()
}

#[cfg(not(unix))]
fn run_with_fsize_limit(_cmd: Command, _target: &std::path::Path) -> Option<Output> {
    None
}
