//! BLE command implementations (Tauri IPC layer).
//!
//! The heavy lifting — spawning the Swift `ble-helper` daemon and reading its
//! JSON results over stdin/stdout — lives in [`crate::ble_daemon`], which is
//! kept free of `#[tauri::command]` so it can be unit-tested.

use crate::ble_daemon::execute_swift_helper;
use crate::{AppState, BleDevice, BleResult, DeviceConfig};
use tauri::State;

#[tauri::command]
pub async fn ble_scan() -> Result<Vec<BleDevice>, String> {
    log::info!("Scanning for BLE devices...");
    let output = execute_swift_helper(&["scan", "5"])?;
    let devices: Vec<BleDevice> = output
        .get("devices")
        .and_then(|d| serde_json::from_value(d.clone()).ok())
        .unwrap_or_default();
    log::info!("Found {} devices", devices.len());
    Ok(devices)
}

#[tauri::command]
pub async fn ble_connect(
    device_id: String,
    state: State<'_, AppState>,
) -> Result<BleResult, String> {
    log::info!("Connecting to device: {}", device_id);
    let output = execute_swift_helper(&["connect", &device_id])?;
    let success = output.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
    if success {
        let mut connected = state.connected_device.lock().unwrap();
        *connected = Some(device_id);
    }
    Ok(BleResult {
        success,
        message: output
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Connected")
            .to_string(),
        data: None,
    })
}

#[tauri::command]
pub async fn ble_disconnect(state: State<'_, AppState>) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(id) => {
            log::info!("Disconnecting from device: {}", id);
            let output = execute_swift_helper(&["disconnect", &id])?;
            let success = output.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
            if success {
                let mut connected = state.connected_device.lock().unwrap();
                *connected = None;
            }
            Ok(BleResult {
                success,
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Disconnected")
                    .to_string(),
                data: None,
            })
        }
        None => Ok(BleResult {
            success: true,
            message: "No device connected".to_string(),
            data: None,
        }),
    }
}

#[tauri::command]
pub async fn ble_read_config(state: State<'_, AppState>) -> Result<DeviceConfig, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            log::info!("Reading device configuration...");
            let output = execute_swift_helper(&["read-config"])?;
            Ok(DeviceConfig {
                wifi_ssid: output.get("wifi_ssid").and_then(|s| s.as_str()).map(String::from),
                wifi_password: output.get("wifi_password").and_then(|s| s.as_str()).map(String::from),
                llm_model: output.get("llm_model").and_then(|s| s.as_str()).map(String::from),
                llm_api_key: output.get("llm_api_key").and_then(|s| s.as_str()).map(String::from),
                hostname: output.get("hostname").and_then(|s| s.as_str()).map(String::from),
            })
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_write_wifi(
    ssid: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            log::info!("Writing WiFi config: ssid={}", ssid);
            let output = execute_swift_helper(&["write-wifi", &ssid, &password])?;
            Ok(BleResult {
                success: output.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("WiFi configured")
                    .to_string(),
                data: None,
            })
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_write_llm(
    model: String,
    api_key: String,
    state: State<'_, AppState>,
) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            log::info!("Writing LLM config: model={}", model);
            let output = execute_swift_helper(&["write-llm", &model, &api_key])?;
            Ok(BleResult {
                success: output.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("LLM configured")
                    .to_string(),
                data: None,
            })
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_write_hostname(
    hostname: String,
    state: State<'_, AppState>,
) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            log::info!("Writing hostname: {}", hostname);
            let output = execute_swift_helper(&["write-hostname", &hostname])?;
            Ok(BleResult {
                success: output.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Hostname configured")
                    .to_string(),
                data: None,
            })
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_status(state: State<'_, AppState>) -> Result<BleResult, String> {
    let (connected, device_id) = {
        let connected = state.connected_device.lock().unwrap();
        (connected.is_some(), connected.clone())
    };

    let message = if connected {
        if let Some(ref id) = device_id {
            format!("Connected to {}", id)
        } else {
            "Disconnected".to_string()
        }
    } else {
        "Disconnected".to_string()
    };

    Ok(BleResult {
        success: true,
        message,
        data: device_id.as_ref().map(|id| serde_json::json!({ "device_id": id })),
    })
}

#[tauri::command]
pub async fn ble_get_status() -> Result<serde_json::Value, String> {
    let output = execute_swift_helper(&["get-status"])?;
    Ok(output)
}

#[tauri::command]
pub async fn ble_get_device_info() -> Result<serde_json::Value, String> {
    let output = execute_swift_helper(&["get-device-info"])?;
    Ok(output)
}

#[tauri::command]
pub async fn ble_get_wifi_status() -> Result<serde_json::Value, String> {
    let output = execute_swift_helper(&["get-wifi-status"])?;
    Ok(output)
}

#[tauri::command]
pub async fn ble_get_conversations() -> Result<serde_json::Value, String> {
    let output = execute_swift_helper(&["get-conversations"])?;
    Ok(output)
}

#[tauri::command]
pub async fn ble_get_channels() -> Result<Vec<serde_json::Value>, String> {
    let output = execute_swift_helper(&["get-channels"])?;
    let channels: Vec<serde_json::Value> = output
        .get("channels")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default();
    Ok(channels)
}

#[tauri::command]
pub async fn ble_reboot(state: State<'_, AppState>) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            log::info!("Rebooting device...");
            let output = execute_swift_helper(&["reboot"])?;
            Ok(BleResult {
                success: output.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Reboot command sent")
                    .to_string(),
                data: None,
            })
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_export_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            let output = execute_swift_helper(&["read-config"])?;
            Ok(serde_json::json!({
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "device_id": device_id,
                "config": {
                    "wifi_ssid": output.get("wifi_ssid"),
                    "llm_model": output.get("llm_model"),
                    "hostname": output.get("hostname"),
                }
            }))
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_get_logs(
    lines: Option<u32>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            let line_count = lines.unwrap_or(100).min(1000);
            let output = execute_swift_helper(&["get-logs", &line_count.to_string()])?;
            Ok(output)
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_diagnostics(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            let output = execute_swift_helper(&["diagnostics"])?;
            Ok(output)
        }
        None => Err("No device connected".to_string()),
    }
}

#[tauri::command]
pub async fn ble_exec_command(
    command: String,
    state: State<'_, AppState>,
) -> Result<BleResult, String> {
    let device_id = {
        let connected = state.connected_device.lock().unwrap();
        connected.clone()
    };

    match device_id {
        Some(_) => {
            let output = execute_swift_helper(&["exec", &command])?;
            Ok(BleResult {
                success: output.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: output
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Command executed")
                    .to_string(),
                data: output.get("data").cloned(),
            })
        }
        None => Err("No device connected".to_string()),
    }
}

// ---------------------------------------------------------------------------
// USB-serial transport commands (talk to the C61 over UART0 instead of BLE)
// ---------------------------------------------------------------------------

use crate::usb::{send_agent_chat, send_at, UsbState};
use std::time::Duration;

/// List USB-serial ports that look like the C61 bridge.
#[tauri::command]
pub async fn usb_list_ports() -> Result<Vec<String>, String> {
    Ok(crate::usb::list_ports())
}

/// Open a USB-serial port (e.g. `/dev/cu.usbserial-10`).
#[tauri::command]
pub async fn usb_open(path: String, state: State<'_, UsbState>) -> Result<serde_json::Value, String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    *port_guard = None; // drop any previous connection
    let port = serialport::new(&path, crate::usb::USB_BAUD)
        .timeout(Duration::from_millis(500))
        .open()
        .map_err(|e| format!("failed to open {path}: {e}"))?;
    *port_guard = Some(port);
    *state.device.lock().map_err(|e| e.to_string())? = Some(path.clone());
    log::info!("[usb] opened {path} @ {} baud", crate::usb::USB_BAUD);
    Ok(serde_json::json!({ "success": true, "path": path, "baud": crate::usb::USB_BAUD }))
}

/// Close the USB-serial port.
#[tauri::command]
pub async fn usb_close(state: State<'_, UsbState>) -> Result<serde_json::Value, String> {
    let path = state.device.lock().map_err(|e| e.to_string())?.clone();
    *state.port.lock().map_err(|e| e.to_string())? = None;
    *state.device.lock().map_err(|e| e.to_string())? = None;
    log::info!("[usb] closed {path:?}");
    Ok(serde_json::json!({ "success": true, "disconnected": path }))
}

/// Send a raw AT command over USB and return the device response.
#[tauri::command]
pub async fn usb_send_at(cmd: String, state: State<'_, UsbState>) -> Result<serde_json::Value, String> {
    let mut guard = state.port.lock().map_err(|e| e.to_string())?;
    let port = guard
        .as_mut()
        .ok_or("USB serial port not open — call usb_open first")?;
    let response = send_at(&mut **port, &cmd)?;
    Ok(serde_json::json!({ "success": true, "command": cmd, "response": response }))
}

/// Chat with the on-device agent over USB (`AT+AGENT="..."`), returning its reply.
#[tauri::command]
pub async fn usb_agent_chat(message: String, state: State<'_, UsbState>) -> Result<serde_json::Value, String> {
    let mut guard = state.port.lock().map_err(|e| e.to_string())?;
    let port = guard
        .as_mut()
        .ok_or("USB serial port not open — call usb_open first")?;
    let reply = send_agent_chat(&mut **port, &message)?;
    Ok(serde_json::json!({
        "success": true,
        "response": reply,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Report the current USB-serial connection state (for the UI to display).
#[tauri::command]
pub async fn usb_get_status(state: State<'_, UsbState>) -> Result<serde_json::Value, String> {
    let path = state.device.lock().map_err(|e| e.to_string())?.clone();
    let connected = state.port.lock().map_err(|e| e.to_string())?.is_some();
    Ok(serde_json::json!({ "connected": connected, "path": path }))
}