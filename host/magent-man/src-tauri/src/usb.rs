//! USB-serial transport for talking to the C61 over its UART0 console port.
//!
//! Used INSTEAD of BLE: the C61 firmware is built with the `ble` feature
//! disabled (see firmware/esp32-app) and exposes its AT + agent interface over
//! the USB-UART bridge (`/dev/cu.usbserial-*`). This module opens the serial
//! port, sends AT commands (including `AT+AGENT="..."` to chat with the
//! on-device agent), and returns the device's responses.
//!
//! Kept free of `#[tauri::command]` so the transport itself can be unit-tested;
//! the thin command wrappers live in `commands.rs` / `lib.rs`.

use std::io::ErrorKind;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Baud rate of the C61 UART0 console (the ingress gateway).
pub const USB_BAUD: u32 = 115200;

/// Managed state: the open serial port and the connected device path.
pub struct UsbState {
    pub port: Mutex<Option<Box<dyn serialport::SerialPort>>>,
    pub device: Mutex<Option<String>>,
}

impl Default for UsbState {
    fn default() -> Self {
        Self {
            port: Mutex::new(None),
            device: Mutex::new(None),
        }
    }
}

/// True for a serial line that is a real device response (vs ESP-IDF log noise).
fn is_clean_line(l: &str) -> bool {
    let t = l.trim();
    !t.is_empty()
        && !t.starts_with("I (")
        && !t.starts_with("W (")
        && !t.starts_with("E (")
        && !t.starts_with("D (")
        && !t.contains("magent_esp32_app")
}

/// Read serial lines until `pred` matches or `timeout` elapses. Returns the
/// accumulated (log-filtered) output.
fn read_until(
    port: &mut dyn serialport::SerialPort,
    pred: &dyn Fn(&str) -> bool,
    timeout: Duration,
) -> Result<String, String> {
    let start = Instant::now();
    let mut acc = String::new();
    let mut line = String::new();
    let mut chunk = [0u8; 256];
    while start.elapsed() < timeout {
        match port.read(&mut chunk) {
            Ok(n) => {
                for &b in &chunk[..n] {
                    if b == b'\n' {
                        let l = line.trim_end_matches('\r').to_string();
                        if is_clean_line(&l) {
                            acc.push_str(&l);
                            acc.push('\n');
                        }
                        if pred(&l) {
                            return Ok(acc);
                        }
                        line.clear();
                    } else {
                        line.push(b as char);
                    }
                }
            }
            Err(e) => {
                if e.kind() == ErrorKind::TimedOut {
                    continue;
                }
                return Err(format!("serial read error: {e}"));
            }
        }
    }
    Err("timeout waiting for serial response".to_string())
}

/// Send a raw AT command and return the device's response (lines up to OK/ERROR).
pub fn send_at(port: &mut dyn serialport::SerialPort, cmd: &str) -> Result<String, String> {
    // Drop any stale buffered output (agent heartbeat logs, previous replies)
    // so we only read this command's response.
    let _ = port.clear(serialport::ClearBuffer::Input);
    let mut line = cmd.trim().to_string();
    if !line.ends_with("\r\n") {
        line.push_str("\r\n");
    }
    port.write_all(line.as_bytes())
        .map_err(|e| format!("serial write failed: {e}"))?;
    port.flush().map_err(|e| format!("serial flush failed: {e}"))?;
    let done = |l: &str| l.trim() == "OK" || l.trim().starts_with("ERROR") || l.trim().starts_with("+CME");
    read_until(port, &done, Duration::from_secs(6))
}

/// Sanitise a user message into a safe quoted AT argument: escape quotes and
/// backslashes, strip CR/LF, and cap at 256 bytes on a UTF-8 char boundary
/// (never splitting a multi-byte character — `String::truncate` would panic).
fn sanitize_agent_message(message: &str) -> String {
    let mut msg = String::new();
    for c in message.chars() {
        match c {
            '"' => msg.push_str("\\\""),
            '\\' => msg.push_str("\\\\"),
            '\r' | '\n' => {}
            _ => msg.push(c),
        }
    }
    if msg.len() > 256 {
        let mut end = 256;
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
    }
    msg
}

/// Send `AT+AGENT="..."` to chat with the on-device agent and return its reply.
///
/// The firmware acknowledges the command with `OK`, then streams the agent's
/// answer as `RESULT[<task>]: <reply>`. We wait for that line (the ReAct loop
/// can take a few seconds) and return the reply text.
pub fn send_agent_chat(port: &mut dyn serialport::SerialPort, message: &str) -> Result<String, String> {
    // Sanitise / bound the message for a quoted AT argument (UTF-8 safe).
    let msg = sanitize_agent_message(message);
    let cmd = format!("AT+AGENT=\"{msg}\"");
    let mut line = cmd.clone();
    line.push_str("\r\n");
    // Drop any stale buffered output (agent heartbeat logs, previous replies).
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(line.as_bytes())
        .map_err(|e| format!("serial write failed: {e}"))?;
    port.flush().map_err(|e| format!("serial flush failed: {e}"))?;

    // Wait for the agent's RESULT line (ReAct loop may take up to ~15s).
    let done = |l: &str| l.starts_with("RESULT[");
    let out = read_until(port, &done, Duration::from_secs(20))?;
    // Extract the reply text after the `RESULT[task]: ` prefix.
    for l in out.lines() {
        if let Some(idx) = l.find("]: ") {
            let reply = l[idx + 3..].trim().to_string();
            if !reply.is_empty() {
                return Ok(reply);
            }
        }
    }
    Err(format!("no agent reply in output: {out}"))
}

/// Enumerate available USB-serial ports (candidates for the C61).
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .ok()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.port_name)
        .filter(|n| n.contains("usbserial") || n.contains("usbmodem") || n.contains("SLAB"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_line_keeps_real_at_replies() {
        assert!(is_clean_line("+CWLAP:(3,\"HomeWiFi\",-45,6)"));
        assert!(is_clean_line("OK"));
        assert!(is_clean_line("RESULT[chat]: Hello!"));
    }

    #[test]
    fn clean_line_filters_esp_idf_log_noise() {
        assert!(!is_clean_line(""));
        assert!(!is_clean_line("   "));
        assert!(!is_clean_line("I (123) main_task: app start"));
        assert!(!is_clean_line("W (0) wifi: some warning"));
        assert!(!is_clean_line("E (1) uart: error"));
        assert!(!is_clean_line("D (2) heap: debug"));
        assert!(!is_clean_line("hello magent_esp32_app"));
    }

    #[test]
    fn sanitize_escapes_quotes_and_backslashes() {
        assert_eq!(sanitize_agent_message("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(sanitize_agent_message("a\\b"), "a\\\\b");
    }

    #[test]
    fn sanitize_strips_crlf() {
        assert_eq!(sanitize_agent_message("line1\r\nline2"), "line1line2");
    }

    #[test]
    fn sanitize_truncates_to_256_bytes_on_char_boundary() {
        // 100 "汉" chars = 300 bytes > 256; must truncate to a valid UTF-8 prefix.
        let msg = "汉".repeat(100);
        let out = sanitize_agent_message(&msg);
        assert!(out.len() <= 256);
        // The result must still be valid UTF-8 (no mid-char split).
        assert!(out.is_char_boundary(out.len()));
        assert!(out.chars().all(|c| c == '汉'));
    }
}

