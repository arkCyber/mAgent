//! mAgent-Man: Tauri Backend Library
//!
//! Provides BLE device management via Swift Helper IPC, plus a USB-serial
//! transport for talking to the C61 over its UART0 console port.

mod ble_daemon;
mod commands;
mod usb;

use commands::*;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Emitter, Window, WindowEvent};

/// BLE Device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BleDevice {
    pub id: String,
    pub name: String,
    pub rssi: i32,
}

/// BLE Configuration data
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    #[serde(rename = "wifi_ssid")]
    pub wifi_ssid: Option<String>,
    #[serde(rename = "wifi_password")]
    pub wifi_password: Option<String>,
    #[serde(rename = "llm_model")]
    pub llm_model: Option<String>,
    #[serde(rename = "llm_api_key")]
    pub llm_api_key: Option<String>,
    pub hostname: Option<String>,
}

/// BLE Operation Result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BleResult {
    pub success: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// Connection state managed by the application
pub struct AppState {
    pub connected_device: Mutex<Option<String>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            connected_device: Mutex::new(None),
        }
    }
}

/// Initialize and run the Tauri application
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("Starting mAgent-Man v{}", env!("CARGO_PKG_VERSION"));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .manage(crate::usb::UsbState::default())
        // Intercept window close so the UI can confirm before the app quits.
        .on_window_event(|window: &Window, event: &WindowEvent| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Prevent the default close and ask the frontend to confirm.
                api.prevent_close();
                let _ = window.emit("magent://close-requested", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            ble_scan,
            ble_connect,
            ble_disconnect,
            ble_read_config,
            ble_write_wifi,
            ble_write_llm,
            ble_write_hostname,
            ble_status,
            ble_get_status,
            ble_get_device_info,
            ble_get_wifi_status,
            ble_get_conversations,
            ble_get_channels,
            ble_reboot,
            ble_export_config,
            ble_get_logs,
            ble_diagnostics,
            ble_exec_command,
            // USB-serial transport (talk to the C61 over UART0 instead of BLE)
            usb_list_ports,
            usb_open,
            usb_close,
            usb_send_at,
            usb_agent_chat,
            usb_get_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}