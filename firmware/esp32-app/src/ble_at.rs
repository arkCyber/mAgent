//! BLE AT Command Bridge
//!
//! Provides AT command interface for BLE status queries.
//!
//! ## Supported Commands
//!
//! - `AT+BLE?` — query current BLE state
//! - `AT+BLE=ON` — start advertising (initialization if needed)
//! - `AT+BLE=OFF` — stop advertising
//! - `AT+BLE=STATE` — print advertising state

// The AT-bridge helpers are kept for wiring into the active `ble_config`
// command dispatch; until then they are intentionally unused (reserved feature).
#![allow(dead_code)]

use crate::ble_config::{BleError, BleServer, BleState};

/// Check if a command string is a BLE command
pub fn is_ble_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    trimmed == "AT+BLE?" || trimmed.starts_with("AT+BLE=")
}

/// Handle a BLE command and return a response string (no trailing newline).
///
/// On success returns `Ok(response)`; on failure returns `Err(BleError)`.
pub fn handle_ble_command(server: &mut BleServer, cmd: &str) -> Result<String, BleError> {
    let trimmed = cmd.trim();

    if trimmed == "AT+BLE?" {
        return Ok(format_ble_state(server.get_state()));
    }

    if let Some(rest) = trimmed.strip_prefix("AT+BLE=") {
        match rest.trim() {
            "ON" | "on" => {
                server.init()?;
                server.start_advertising()?;
                return Ok("OK".to_string());
            }
            "OFF" | "off" => {
                server.stop_advertising()?;
                return Ok(format_ble_state(server.get_state()));
            }
            "STATE" | "state" => {
                return Ok(format_ble_state(server.get_state()));
            }
            other => {
                log::warn!("[ble] unknown AT+BLE subcommand: {other}");
                return Ok("ERR=unknown_subcmd".to_string());
            }
        }
    }

    Ok("ERR=invalid".to_string())
}

fn format_ble_state(state: BleState) -> String {
    match state {
        BleState::Idle => "STATE=idle".to_string(),
        BleState::Initializing => "STATE=initializing".to_string(),
        BleState::Advertising => "STATE=advertising".to_string(),
        BleState::Connected => "STATE=connected".to_string(),
        BleState::Error => "STATE=error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_question_form() {
        assert!(is_ble_command("AT+BLE?"));
        assert!(is_ble_command("  AT+BLE?  "));
    }

    #[test]
    fn recognizes_set_form() {
        assert!(is_ble_command("AT+BLE=ON"));
        assert!(is_ble_command("AT+BLE=STATE"));
    }

    #[test]
    fn rejects_unrelated_commands() {
        assert!(!is_ble_command("AT+WIFI?"));
        assert!(!is_ble_command("AT+BLE"));
    }
}
