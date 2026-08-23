//! `magent doctor` — verify the environment is sane.
//!
//! Checks:
//!
//! 1. **Ollama reachability** — can we GET `{url}/api/tags`?
//! 2. **Model availability** — does the requested model appear in the
//!    list returned by `/api/tags`? (Only when we can reach Ollama.)
//! 3. **Tool backend** — can we build a `SimulatorExecutor` and run a
//!    trivial `read_sensor` call against it?
//!
//! Exit code:
//!
//! * `0` — every check passed.
//! * `1` — at least one check failed. Each failure is printed with the
//!   same labelled format as `run`'s trace, so a CI grep still works.
//!
//! In `--json` mode the same checks are emitted as a JSON object with
//! `ok: true|false` per check, plus a top-level `ok` summarising the
//! overall result.

use std::io::Write;

use magent_core::agent_runner::{DeepSeekClient, LlmBackend, OllamaClient, ToolExecutor};
use magent_core::real_tools::SimulatorExecutor;

use crate::output::{Output, OutputKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckResult {
    Pass,
    Fail,
    Skip,
}

impl CheckResult {
    /// Short uppercase label used by the doctor summary line and
    /// the JSON envelope (e.g. `"PASS"`, `"FAIL"`, `"SKIP"`).
    pub fn label(self) -> &'static str {
        match self {
            CheckResult::Pass => "PASS",
            CheckResult::Fail => "FAIL",
            CheckResult::Skip => "SKIP",
        }
    }
}

pub struct DoctorCmd<'a> {
    pub provider: &'a str,
    pub ollama_url: &'a str,
    pub model: &'a str,
    pub deepseek_url: &'a str,
    pub api_key: Option<&'a str>,
}

impl<'a> DoctorCmd<'a> {
    pub fn new(
        provider: &'a str,
        ollama_url: &'a str,
        model: &'a str,
        deepseek_url: &'a str,
        api_key: Option<&'a str>,
    ) -> Self {
        Self {
            provider,
            ollama_url,
            model,
            deepseek_url,
            api_key,
        }
    }

    /// Run every check, write human-readable status to `out`, and return
    /// `true` iff every check passed.
    pub fn execute(&self, out: &mut Output) -> bool {
        if let Err(e) = out.trace_labeled("doctor", "checking environment") {
            let _ = writeln!(
                std::io::stderr().lock(),
                "doctor: cannot write header: {}",
                e
            );
            return false;
        }

        // I/O failures from Output are best-effort: we still try to
        // surface *some* result so a CI script sees a non-zero exit
        // instead of a panic. The check results themselves are
        // captured in the tuple; the boolean returned here is the
        // overall verdict.
        let (provider_check, ollama_check, model_check) = match self.check_provider_chain(out) {
            Ok(t) => t,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "doctor: I/O error during checks: {}",
                    e
                );
                (CheckResult::Fail, CheckResult::Fail, CheckResult::Fail)
            }
        };
        let backend_check = match self.check_tool_backend(out) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "doctor: I/O error during tool backend check: {}",
                    e
                );
                CheckResult::Fail
            }
        };

        let overall = matches!(provider_check, CheckResult::Pass | CheckResult::Skip)
            && matches!(ollama_check, CheckResult::Pass | CheckResult::Skip)
            && matches!(model_check, CheckResult::Pass | CheckResult::Skip)
            && backend_check == CheckResult::Pass;

        if out.kind() == OutputKind::Json {
            // Emit a structured JSON envelope. The `result` field
            // is a short uppercase label ("PASS" / "FAIL" / "SKIP")
            // so a CI script can grep on it without parsing the
            // boolean; the boolean is still there for back-compat.
            let envelope = serde_json::json!({
                "ok": overall,
                "checks": {
                    "provider": {
                        "ok": provider_check == CheckResult::Pass,
                        "skipped": provider_check == CheckResult::Skip,
                        "result": provider_check.label(),
                        "name": self.provider,
                    },
                    "ollama": {
                        "ok": ollama_check == CheckResult::Pass,
                        "skipped": ollama_check == CheckResult::Skip,
                        "result": ollama_check.label(),
                        "url": self.ollama_url,
                    },
                    "model": {
                        "ok": model_check == CheckResult::Pass,
                        "skipped": model_check == CheckResult::Skip,
                        "result": model_check.label(),
                        "name": self.model,
                    },
                    "tool_backend": {
                        "ok": backend_check == CheckResult::Pass,
                        "result": backend_check.label(),
                    },
                }
            });
            // Bypass `Output::write_json` (that one is for run reports)
            // and write the envelope directly to stdout.
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(
                stdout,
                "{}",
                serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".into())
            );
        } else {
            // Per-check summary line. Each check gets its own
            // labelled status so the user can spot which one
            // failed without re-reading the trace above.
            let _ = out.trace_labeled(
                "summary",
                &format!(
                    "provider={} ollama={} model={} tool_backend={}",
                    provider_check.label(),
                    ollama_check.label(),
                    model_check.label(),
                    backend_check.label(),
                ),
            );
            if let Err(e) = out.trace_labeled(
                "doctor",
                if overall {
                    "all checks passed"
                } else {
                    "one or more checks failed (see above)"
                },
            ) {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "doctor: cannot write summary: {}",
                    e
                );
            }
        }
        let _ = out.flush();
        overall
    }

    /// Provider-aware chain: depending on `self.provider`, run either
    /// the Ollama checks or the DeepSeek checks. For Ollama, both
    /// `check_ollama` and `check_model` fire; for DeepSeek we check
    /// the API key + a `/models` reachability probe and skip the
    /// Ollama-specific ones (model list lives behind `/api/tags`,
    /// which DeepSeek doesn't have).
    ///
    /// Returns `Err` only on I/O failures from `Output`; the check
    /// outcomes themselves come back as `(CheckResult, CheckResult,
    /// CheckResult)`. The caller swallows I/O errors (a write failure
    /// to stderr shouldn't crash doctor).
    fn check_provider_chain(
        &self,
        out: &mut Output,
    ) -> std::io::Result<(CheckResult, CheckResult, CheckResult)> {
        match self.provider {
            "deepseek" => {
                let key = self
                    .api_key
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
                    .or_else(|| std::env::var("OLLAMA_API_KEY").ok());
                let key = key.and_then(|s| {
                    let t = s.trim().to_string();
                    if t.is_empty() { None } else { Some(t) }
                });
                let pass = match &key {
                    Some(key) => {
                        // `try_with_endpoint` re-validates the key
                        // (defence in depth: the `Some(key)` branch
                        // above already trimmed, but `try_*` is the
                        // public API for fallible construction).
                        let Some(ds) = DeepSeekClient::try_with_endpoint(
                            self.deepseek_url,
                            self.model,
                            key,
                        ) else {
                            out.warn(
                                "DeepSeek API key is empty after trimming — refusing to connect",
                            )?;
                            return Ok((CheckResult::Fail, CheckResult::Skip, CheckResult::Skip));
                        };
                        if ds.check_connection() {
                            out.trace_labeled(
                                "deepseek",
                                &format!(
                                    "reachable at {} (model: {})",
                                    self.deepseek_url, self.model
                                ),
                            )?;
                            CheckResult::Pass
                        } else {
                            out.warn(&format!(
                                "could not reach DeepSeek at {} (is the API key valid?)",
                                self.deepseek_url
                            ))?;
                            CheckResult::Fail
                        }
                    }
                    None => {
                        out.warn(
                            "no DeepSeek API key (use --api-key or DEEPSEEK_API_KEY)",
                        )?;
                        CheckResult::Fail
                    }
                };
                Ok((pass, CheckResult::Skip, CheckResult::Skip))
            }
            _ => {
                // Default: Ollama (or any unknown provider — we log a
                // warning and run the Ollama checks anyway because it's
                // the most useful fallback).
                if self.provider != "ollama" {
                    out.warn(&format!(
                        "unknown provider '{}' — falling back to Ollama checks",
                        self.provider
                    ))?;
                }
                let ollama = self.check_ollama(out)?;
                let model = self.check_model(out, ollama == CheckResult::Pass)?;
                Ok((CheckResult::Pass, ollama, model))
            }
        }
    }

    fn check_ollama(&self, out: &mut Output) -> std::io::Result<CheckResult> {
        out.trace_labeled("ollama", &format!("checking {}", self.ollama_url))?;
        let client = OllamaClient::new(self.ollama_url, self.model);
        if client.check_connection() {
            out.trace_labeled("ollama", "reachable")?;
            Ok(CheckResult::Pass)
        } else {
            out.warn(&format!(
                "could not reach Ollama at {} (is it running?)",
                self.ollama_url
            ))?;
            Ok(CheckResult::Fail)
        }
    }

    fn check_model(
        &self,
        out: &mut Output,
        ollama_ok: bool,
    ) -> std::io::Result<CheckResult> {
        if !ollama_ok {
            out.trace_labeled("model", "skipped (ollama not reachable)")?;
            return Ok(CheckResult::Skip);
        }
        let client = OllamaClient::new(self.ollama_url, self.model);
        let models = client.get_models();
        if models.iter().any(|m| m == self.model) {
            out.trace_labeled("model", &format!("'{}' is installed", self.model))?;
            Ok(CheckResult::Pass)
        } else {
            out.warn(&format!(
                "model '{}' not found on server; run `ollama pull {}`",
                self.model, self.model
            ))?;
            Ok(CheckResult::Fail)
        }
    }

    fn check_tool_backend(&self, out: &mut Output) -> std::io::Result<CheckResult> {
        out.trace_labeled("tool backend", "smoke-testing SimulatorExecutor")?;
        let mut executor = SimulatorExecutor::new();
        executor.connect_ble();
        match executor.execute("read_sensor", r#"{"sensor":"temperature"}"#) {
            Ok(content) if !content.is_empty() => {
                out.trace_labeled("tool backend", &format!("read_sensor ok → {}", content))?;
                Ok(CheckResult::Pass)
            }
            Ok(_) => {
                out.warn("read_sensor returned an empty result")?;
                Ok(CheckResult::Fail)
            }
            Err(e) => {
                out.warn(&format!("read_sensor failed: {}", e))?;
                Ok(CheckResult::Fail)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// If Ollama is not reachable, the overall doctor run must report
    /// `false` regardless of what the tool backend does.
    #[test]
    fn overall_false_when_ollama_down() {
        let mut out = Output::new(OutputKind::Human, true);
        let cmd = DoctorCmd::new(
            "ollama",
            "http://127.0.0.1:1",
            "llama3.2",
            "https://api.deepseek.com/v1",
            None,
        );
        // Port 1 is reserved; the connection will be refused.
        assert!(!cmd.execute(&mut out));
    }

    /// With no API key and provider=deepseek, doctor must report false
    /// (we don't silently fall back to Ollama).
    #[test]
    fn overall_false_when_deepseek_missing_key() {
        // Make sure no env vars leak in from the test runner.
        // SAFETY: tests in a single process are safe to mutate env vars
        // because `cargo test` runs them serially by default within a
        // binary. If we later move to `--test-threads`, we'll need a
        // serial_test crate or per-test child processes.
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("OLLAMA_API_KEY");
        }
        let mut out = Output::new(OutputKind::Human, true);
        let cmd = DoctorCmd::new(
            "deepseek",
            "http://localhost:11434",
            "deepseek-chat",
            "https://api.deepseek.com/v1",
            None,
        );
        assert!(!cmd.execute(&mut out));
    }

    /// With an API key but an unreachable endpoint, doctor must
    /// report false (the key alone is not enough; the server must
    /// also respond).
    #[test]
    fn overall_false_when_deepseek_unreachable() {
        // Use a definitely-unreachable port and a syntactically valid
        // key so the failure mode is "network", not "auth".
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("OLLAMA_API_KEY");
        }
        let mut out = Output::new(OutputKind::Human, true);
        let cmd = DoctorCmd::new(
            "deepseek",
            "http://localhost:11434",
            "deepseek-chat",
            "http://127.0.0.1:1/v1",
            Some("sk-fake-but-non-empty"),
        );
        assert!(!cmd.execute(&mut out));
    }

    #[test]
    fn check_result_label_is_stable() {
        // The labels are part of the doctor JSON contract — CI
        // scripts grep on `result == "PASS"`, so the strings
        // must not drift.
        assert_eq!(CheckResult::Pass.label(), "PASS");
        assert_eq!(CheckResult::Fail.label(), "FAIL");
        assert_eq!(CheckResult::Skip.label(), "SKIP");
    }
}
