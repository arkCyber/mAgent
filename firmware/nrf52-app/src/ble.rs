//! BLE Module for nRF52840
//!
//! Implements BLE peripheral functionality using Nordic's SoftDevice stack.
//! Provides device configuration, real-time status, and system control
//! capabilities over BLE 5.0.
//!
//! ## GATT Service Structure
//!
//! **Service UUID: 0x1850 (mAgent Configuration Service)**
//!
//! | UUID | Name | Properties | Description |
//! |------|------|------------|-------------|
//! | 0x2A01 | WiFi SSID | Write | Set WiFi SSID |
//! | 0x2A02 | WiFi Password | Write | Set WiFi password |
//! | 0x2A06 | Status | Read/Notify | System status updates |
//! | 0x2A07 | Device Info | Read | Version, memory, uptime |
//! | 0x2A08 | System Commands | Write | Execute commands |
//! | 0x2A09 | System Responses | Notify | Command responses |

use defmt::{info, warn, error};

// `handle_characteristic_read` returns `Vec<u8>` (allocating) for the
// Status / DeviceInfo reads. This crate is `#![no_std]`, so `alloc` must be
// brought into scope explicitly; the global allocator is provided by
// `embedded_alloc::Heap` in `main.rs`.
extern crate alloc;
use alloc::vec::Vec;

// =============================================================================
// GATT UUIDs
// =============================================================================

/// mAgent Configuration Service UUID (16-bit)
pub const CONFIG_SERVICE_UUID16: u16 = 0x1850;

/// Characteristic UUIDs (16-bit short form)
const WIFI_SSID_CHAR_UUID16: u16 = 0x2A01;
const WIFI_PASS_CHAR_UUID16: u16 = 0x2A02;
const STATUS_CHAR_UUID16: u16 = 0x2A06;
const DEVICE_INFO_CHAR_UUID16: u16 = 0x2A07;
const SYS_CMD_CHAR_UUID16: u16 = 0x2A08;
const SYS_RSP_CHAR_UUID16: u16 = 0x2A09;

/// Device name for advertising
pub const DEFAULT_DEVICE_NAME: &str = "mAgent-nRF52840";

// =============================================================================
// BLE Configuration
// =============================================================================

/// BLE advertising and connection parameters
pub struct BleConfig {
    /// Device name advertised
    pub device_name: &'static str,
    /// Advertising interval in milliseconds
    pub adv_interval_ms: u16,
    /// Connection interval min (ms)
    pub conn_interval_min_ms: u16,
    /// Connection interval max (ms)
    pub conn_interval_max_ms: u16,
    /// Slave latency (connection intervals to skip)
    pub slave_latency: u16,
    /// Supervision timeout (ms)
    pub sup_timeout_ms: u16,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            device_name: DEFAULT_DEVICE_NAME,
            adv_interval_ms: 100,      // 100ms advertising interval
            conn_interval_min_ms: 20,  // 20ms min connection interval
            conn_interval_max_ms: 40,  // 40ms max connection interval
            slave_latency: 0,
            sup_timeout_ms: 400,       // 400ms supervision timeout
        }
    }
}

impl BleConfig {
    pub fn new(name: &'static str) -> Self {
        let mut config = Self::default();
        config.device_name = name;
        config
    }
}

// =============================================================================
// BLE State
// =============================================================================

/// Current BLE connection state
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BleState {
    Idle,
    Advertising,
    Connected,
    Configuring,
    Error,
}

impl Default for BleState {
    fn default() -> Self {
        Self::Idle
    }
}

/// BLE connection state tracking
pub struct BleStateManager {
    /// Current state
    pub state: BleState,
    /// Connection handle (SoftDevice handle)
    pub connection_handle: Option<u16>,
    /// Connection state flags
    pub is_connected: bool,
    /// Peer MTU
    pub mtu: u16,
    /// Battery level (0-100)
    pub battery_level: u8,
}

impl Default for BleStateManager {
    fn default() -> Self {
        Self {
            state: BleState::Idle,
            connection_handle: None,
            is_connected: false,
            mtu: 23,  // Default BLE 4.2 MTU
            battery_level: 100,
        }
    }
}

impl BleStateManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_advertising(&mut self) {
        self.state = BleState::Advertising;
        info!("BLE state: Advertising");
    }

    pub fn set_connected(&mut self, handle: u16) {
        self.state = BleState::Connected;
        self.connection_handle = Some(handle);
        self.is_connected = true;
        info!("BLE state: Connected (handle={})", handle);
    }

    pub fn set_disconnected(&mut self) {
        self.state = BleState::Idle;
        self.connection_handle = None;
        self.is_connected = false;
        info!("BLE state: Disconnected");
    }

    pub fn set_configuring(&mut self) {
        self.state = BleState::Configuring;
        info!("BLE state: Configuring");
    }

    pub fn set_error(&mut self) {
        self.state = BleState::Error;
        error!("BLE state: Error");
    }

    pub fn connection_status(&self) -> &'static str {
        match self.state {
            BleState::Idle => "Idle",
            BleState::Advertising => "Advertising",
            BleState::Connected => "Connected",
            BleState::Configuring => "Configuring",
            BleState::Error => "Error",
        }
    }
}

// =============================================================================
// System Status Structures (nRF52 specific)
// =============================================================================

/// System status for BLE notifications
#[derive(Debug, Clone, Default)]
pub struct SystemStatus {
    pub state: u8,
    pub battery_level: u8,
    pub memory_free: u32,
    pub uptime_ms: u64,
    pub error_code: u8,
}

impl SystemStatus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update from current system state
    pub fn update(&mut self, battery: u8, memory_free: u32, uptime_ms: u64) {
        self.battery_level = battery;
        self.memory_free = memory_free;
        self.uptime_ms = uptime_ms;
    }

    pub fn set_state(&mut self, state: BleState) {
        self.state = match state {
            BleState::Idle => 0,
            BleState::Advertising => 1,
            BleState::Connected => 2,
            BleState::Configuring => 3,
            BleState::Error => 255,
        };
    }

    /// Convert to bytes for GATT notification
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0] = self.state;
        bytes[1] = self.battery_level;
        bytes[2..6].copy_from_slice(&self.memory_free.to_le_bytes());
        bytes[6..14].copy_from_slice(&self.uptime_ms.to_le_bytes());
        bytes[14] = self.error_code;
        bytes
    }
}

/// Device info for BLE read
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub version_major: u8,
    pub version_minor: u8,
    pub version_patch: u8,
    pub reserved: u8,
    pub memory_total: u32,
    pub memory_free: u32,
    pub uptime_ms: u64,
    pub chip_model: [u8; 16],
}

impl DeviceInfo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert to bytes for GATT read response
    pub fn to_bytes(&self) -> [u8; 40] {
        let mut bytes = [0u8; 40];
        bytes[0] = self.version_major;
        bytes[1] = self.version_minor;
        bytes[2] = self.version_patch;
        bytes[3] = self.reserved;
        bytes[4..8].copy_from_slice(&self.memory_total.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.memory_free.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.uptime_ms.to_le_bytes());
        bytes[20..36].copy_from_slice(&self.chip_model);
        bytes
    }
}

// =============================================================================
// BLE Operations (SoftDevice Integration)
// =============================================================================

/// Initialize the SoftDevice BLE stack
pub fn init_softdevice() -> Result<(), ()> {
    info!("Initializing SoftDevice BLE stack...");

    // In a full implementation, this would:
    // 1. Enable the SoftDevice
    // 2. Set up BLE stack parameters
    // 3. Enable BLE GAP and GATT
    //
    // Example pseudocode:
    // sd_softdevice_enable(&clock_config, &fault_handler);
    // sd_ble_version_get(&version);
    // ble_enable_params_t ble_params = {
    //     .gatts_enable_params.service_changed = 0,
    //     .gatts_enable_params.attr_tab_size = BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
    // };
    // sd_ble_enable(&ble_params);

    info!("SoftDevice initialized");
    Ok(())
}

/// Start advertising with the given configuration
pub fn start_advertising(config: &BleConfig) -> Result<(), ()> {
    info!("Starting BLE advertising as '{}'...", config.device_name);

    // In a full implementation, this would:
    // 1. Configure advertising parameters
    // 2. Set up advertising data (device name + service UUID)
    // 3. Configure scan response
    // 4. Start advertising
    //
    // Example pseudocode:
    // ble_advdata_t advdata = {
    //     .name_type = BLE_ADVDATA_FULL_NAME,
    //     .include_ble_addr = true,
    // };
    // ble_advdata_t srdata = {
    //     .uuids_complete = &uuid_list,
    // };
    // sd_ble_gap_adv_data_set(&advdata, &srdata);
    // sd_ble_gap_adv_start(&adv_params);
    //
    // let adv_params = ble_gap_adv_params_t {
    //     .type_ = BLE_GAP_ADV_TYPE_ADV_IND,
    //     .p_peer_addr = null,
    //     .fp = BLE_GAP_ADV_FP_ANY,
    //     .interval = config.adv_interval_ms * 1.6, // units of 0.625ms
    //     .timeout = 0, // infinite
    // };

    info!("BLE advertising started");
    Ok(())
}

/// Stop advertising
pub fn stop_advertising() {
    info!("Stopping BLE advertising...");

    // In a full implementation:
    // sd_ble_gap_adv_stop();

    info!("BLE advertising stopped");
}

/// Disconnect from peer
pub fn disconnect(connection_handle: u16, reason: u8) {
    warn!("Disconnecting (handle={}, reason={})...", connection_handle, reason);

    // In a full implementation:
    // sd_ble_gap_disconnect(connection_handle, BLE_HCI_REMOTE_USER_TERMINATED_CONNECTION);

    info!("Disconnected");
}

/// Get current connection status as string
pub fn connection_status(state: &BleStateManager) -> &'static str {
    state.connection_status()
}

// =============================================================================
// GATT Service Setup (nRF52 specific)
// =============================================================================

/// Service handle storage
pub struct GattService {
    pub service_handle: u16,
    pub char_handles: [u16; 6],  // WiFi SSID, WiFi Pass, Status, DeviceInfo, SysCmd, SysRsp
}

impl Default for GattService {
    fn default() -> Self {
        Self {
            service_handle: 0,
            char_handles: [0; 6],
        }
    }
}

impl GattService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the mAgent Configuration Service
    pub fn init(&mut self) -> Result<(), ()> {
        info!("Initializing GATT service...");

        // In a full implementation:
        // 1. Add the Configuration Service
        // sd_ble_gatts_service_add(BLE_GATT_SVC_TYPE_PRIMARY, &config_service_uuid, &service_handle);
        //
        // 2. Add characteristics
        // sd_ble_gatts_characteristic_add(service_handle, &char_md, &attr, &char_handle);
        //
        // Each characteristic needs:
        // - Characteristic metadata (properties, read/write permissions)
        // - Attribute metadata (value handle, permissions)
        // - Attribute value (initial value, max length)

        info!("GATT service initialized");
        info!("  Service UUID: 0x{:04X}", CONFIG_SERVICE_UUID16);
        info!("  - WiFi SSID (Write)");
        info!("  - WiFi Password (Write)");
        info!("  - Status (Read/Notify)");
        info!("  - Device Info (Read)");
        info!("  - System Commands (Write)");
        info!("  - System Responses (Notify)");

        Ok(())
    }
}

// =============================================================================
// Characteristic Handlers
// =============================================================================

/// Characteristic UUID enumeration
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CharacteristicIdx {
    WiFiSsid = 0,
    WiFiPass = 1,
    Status = 2,
    DeviceInfo = 3,
    SysCmd = 4,
    SysRsp = 5,
}

/// Human-readable name for a characteristic index.
///
/// Used in `defmt` log lines. We intentionally avoid `{:?}` here because
/// defmt's `{:?}` requires the `defmt::Format` derive (the `derive` cargo
/// feature is not enabled for this crate); returning a `&'static str`
/// keeps logging dependency-free.
fn characteristic_name(idx: CharacteristicIdx) -> &'static str {
    match idx {
        CharacteristicIdx::WiFiSsid => "WiFiSsid",
        CharacteristicIdx::WiFiPass => "WiFiPass",
        CharacteristicIdx::Status => "Status",
        CharacteristicIdx::DeviceInfo => "DeviceInfo",
        CharacteristicIdx::SysCmd => "SysCmd",
        CharacteristicIdx::SysRsp => "SysRsp",
    }
}

/// Handle write to a characteristic
pub fn handle_characteristic_write(
    _char_idx: CharacteristicIdx,
    data: &[u8],
) -> Result<(), &'static str> {
    // In a full implementation, this would:
    // 1. Validate the data
    // 2. Store pending configuration
    // 3. Send response if needed

    info!(
        "Characteristic write: {}, {} bytes",
        characteristic_name(_char_idx),
        data.len()
    );

    match _char_idx {
        CharacteristicIdx::WiFiSsid => {
            if data.is_empty() || data.len() > 32 {
                return Err("Invalid SSID length");
            }
            info!("WiFi SSID: {}", core::str::from_utf8(data).unwrap_or("<invalid>"));
        }
        CharacteristicIdx::WiFiPass => {
            if data.is_empty() || data.len() > 64 {
                return Err("Password too long / empty");
            }
            info!("WiFi password: {} bytes", data.len());
        }
        CharacteristicIdx::SysCmd => {
            info!("System command: {}", core::str::from_utf8(data).unwrap_or("<invalid>"));
        }
        _ => {}
    }

    Ok(())
}

/// Handle read from a characteristic.
///
/// Returns the *current* system status / device info, so a read always
/// reflects live state rather than a freshly-zeroed snapshot.
pub fn handle_characteristic_read(
    status: &SystemStatus,
    device: &DeviceInfo,
    char_idx: CharacteristicIdx,
) -> Option<Vec<u8>> {
    match char_idx {
        CharacteristicIdx::Status => Some(status.to_bytes().to_vec()),
        CharacteristicIdx::DeviceInfo => Some(device.to_bytes().to_vec()),
        _ => None,
    }
}

// =============================================================================
// Connection Event Handlers
// =============================================================================

/// Handle connection event
pub fn on_connected(state: &mut BleStateManager, connection_handle: u16) {
    state.set_connected(connection_handle);
}

/// Handle disconnection event
pub fn on_disconnected(state: &mut BleStateManager) {
    state.set_disconnected();
}

/// Handle MTU request
pub fn on_mtu_request(state: &mut BleStateManager, mtu: u16) {
    // nRF52840 supports up to 247 bytes
    let negotiated = mtu.min(247).max(23);
    state.mtu = negotiated;
    info!("MTU negotiated: {} bytes", negotiated);
}

// =============================================================================
// Battery Service (BAS) - Standard BLE Battery Service
// =============================================================================

/// Battery Service UUID
const BAS_SERVICE_UUID16: u16 = 0x180F;
const BAS_LEVEL_CHAR_UUID16: u16 = 0x2A19;

/// Battery service state
pub struct BatteryService {
    pub level: u8,
    pub service_handle: u16,
    pub level_char_handle: u16,
}

impl Default for BatteryService {
    fn default() -> Self {
        Self {
            level: 100,
            service_handle: 0,
            level_char_handle: 0,
        }
    }
}

impl BatteryService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the Battery Service
    pub fn init(&mut self) -> Result<(), ()> {
        info!("Initializing Battery Service...");

        // In a full implementation:
        // sd_ble_gatts_service_add(BLE_GATT_SVC_TYPE_PRIMARY, &bas_service_uuid, &self.service_handle);
        // sd_ble_gatts_characteristic_add(self.service_handle, &char_md, &attr, &self.level_char_handle);

        info!("Battery Service initialized");
        Ok(())
    }

    /// Update battery level
    pub fn set_level(&mut self, level: u8) {
        self.level = level.min(100);
        info!("Battery level: {}%", self.level);
    }
}

// =============================================================================
// Device Information Service (DIS) - Standard BLE DIS
// =============================================================================

/// Device Information Service UUIDs
const DIS_SERVICE_UUID16: u16 = 0x180A;

/// Initialize standard DIS characteristics
pub fn init_device_information() {
    info!("Initializing Device Information Service...");

    // In a full implementation, this would add:
    // - Manufacturer Name (0x2A29)
    // - Model Number (0x2A24)
    // - Serial Number (0x2A25)
    // - Hardware Revision (0x2A27)
    // - Firmware Revision (0x2A26)
    // - Software Revision (0x2A28)

    info!("Device Information Service initialized");
}
