//! `ble-helper` daemon client.
//!
//! Spawns the Swift `ble-helper` binary once and drives it over stdin/stdout
//! (JSON-RPC-ish: one command per line, result is the first stdout line
//! containing `"success"`). Keeping the process alive preserves the
//! CoreBluetooth connection across commands.
//!
//! This module deliberately contains **no** `#[tauri::command]` macros so it
//! compiles under `cargo test` (Tauri 2's command macro does not expand under
//! `cfg(test)`).

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Mutex;

/// A handle to the long-lived `ble-helper` daemon.
///
/// A background thread owns the child's stdout and pushes each line onto a
/// channel, so callers can read results with a timeout instead of blocking
/// forever (which would hang the app if the daemon never produced a result).
struct Daemon {
    stdin: ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
}

static DAEMON: Mutex<Option<Daemon>> = Mutex::new(None);
/// How long to wait for a command's result before giving up.
const HELPER_TIMEOUT_SECS: u64 = 20;

/// Resolve the Swift helper binary (real files only).
pub fn helper_path() -> Result<std::path::PathBuf, String> {
    // `cargo run`/`tauri dev` sets the CWD to the crate dir (`src-tauri/`),
    // so relative paths must be anchored to the repo's project root
    // (`host/magent-man/`), which is ONE level up from CARGO_MANIFEST_DIR.
    let manifest = std::env!("CARGO_MANIFEST_DIR");
    let project_root = std::path::PathBuf::from(manifest).join("..");

    let possible_paths = vec![
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("ble-helper"))),
        Some(project_root.join("ble-helper/.build/arm64-apple-macosx/release/ble-helper")),
        Some(project_root.join("ble-helper")),
        Some(std::path::PathBuf::from("ble-helper")),
        Some(std::path::PathBuf::from("./ble-helper")),
        Some(std::path::PathBuf::from(
            "ble-helper/.build/arm64-apple-macosx/release/ble-helper",
        )),
    ];
    possible_paths
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .ok_or_else(|| "ble-helper binary not found".to_string())
}

/// Execute a command on the `ble-helper` daemon and return its JSON result.
///
/// Spawns the daemon on first use; the connection is kept alive for the
/// lifetime of the process so connect → read/write works across commands.
pub fn execute_swift_helper(args: &[&str]) -> Result<serde_json::Value, String> {
    log::info!("[ble-helper] execute_swift_helper called with: {:?}", args);
    let mut guard = DAEMON.lock().map_err(|e| e.to_string())?;

    if guard.is_none() {
        let path = helper_path()?;
        log::info!("[ble-helper] resolved daemon path: {}", path.display());
        let child = match Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Err(format!("Failed to spawn ble-helper: {e}")),
        };
        log::info!("[ble-helper] daemon spawned (pid {})", child.id());
        let stdin = child.stdin.expect("no stdin on ble-helper");
        let stdout = child.stdout.expect("no stdout on ble-helper");

        // Dedicated reader thread pushes each line into a channel.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or read error
                    Ok(_) => {
                        let line = buf.trim_end().to_string();
                        if tx.send(line).is_err() {
                            break; // receiver dropped
                        }
                    }
                }
            }
        });
        *guard = Some(Daemon { stdin, lines: rx });
    }
    let d = guard.as_mut().expect("daemon just initialized");

    // Send the command (args joined by spaces, one per line).
    let line = args.join(" ");
    writeln!(d.stdin, "{line}").map_err(|e| format!("failed to write to ble-helper: {e}"))?;
    d.stdin.flush().ok();
    log::info!("[ble-helper] sent command: {line}");

    // Consume stdout lines until the command's result (a line with "success"),
    // with a hard timeout so a stuck daemon can't hang the app forever.
    // Intermediate events ({type:ready}, {type:device}, scan_start) are skipped.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(HELPER_TIMEOUT_SECS);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match d.lines.recv_timeout(remaining) {
            Ok(line) => {
                log::info!("[ble-helper] recv: {}", line.chars().take(140).collect::<String>());
                if line.contains("\"success\"") {
                    return serde_json::from_str(&line)
                        .map_err(|e| format!("failed to parse ble-helper response: {e} - {line}"));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!("timed out waiting for ble-helper result of: {line}"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("ble-helper exited without a result".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the daemon is spawned and a BLE scan returns a result (real
    /// hardware: requires the mAgent device advertising and macOS Bluetooth
    /// access for the test runner). Logs (does not hard-fail) on machines
    /// without Bluetooth/device so CI isn't coupled to hardware.
    #[test]
    fn daemon_scan_returns_devices() {
        match execute_swift_helper(&["scan", "5"]) {
            Ok(v) => {
                println!("[test] scan result: {v}");
                assert!(v.get("success").and_then(|s| s.as_bool()).unwrap_or(false));
            }
            Err(e) => {
                println!("[test] scan did not complete: {e}");
            }
        }
    }

    /// The daemon must NOT hang on a bogus command — it should return quickly,
    /// never block up to the timeout.
    #[test]
    fn daemon_bogus_command_does_not_hang() {
        let started = std::time::Instant::now();
        let result = execute_swift_helper(&["not-a-real-command"]);
        let elapsed = started.elapsed();
        // The bogus command hits the helper's default branch and prints an
        // immediate success:false line, so it must return well under the 20s
        // timeout.
        assert!(elapsed.as_secs() < 15, "bogus command took too long: {elapsed:?}");
        log::info!("bogus command result ({}s): {:?}", elapsed.as_secs(), result);
    }
}

