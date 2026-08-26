//! ESP32 BLE GATT server — built on the **safe** `esp-idf-svc` BLE API.
//!
//! This is the "complete the Bluetooth feature" implementation: it
//! registers a *real* GATT service (`0x1850`) with the mAgent
//! configuration characteristics and drives the asynchronous
//! `advertise → connect → read/write/notify` lifecycle through the GAP +
//! GATTS event callbacks (which the raw-FFI `ble_config.rs` never did).
//!
//! # Integration (firmware `Cargo.toml`)
//! * Enable the `bt` feature of `esp-idf-svc` (`features = ["bt", "std"]`)
//!   and the BLE feature of `esp-hal` for the ESP32-C61 (BLUEDROID).
//! * Create an `esp_hal::ble::BtDriver` from the BLE peripherals, wrap it
//!   in `Arc<Mutex<...>>`, and call [`run_server`] on the resulting
//!   [`BleServer`]. See `examples/` in `esp-idf-svc` for the driver wiring.
//!
//! # Read/write model
//! All characteristics use `AutoResponse::ByGatt`: ESP-IDF stores the
//! value in its GATT database and auto-replies to reads/writes. The app
//! updates read-only values (status, device info) via `set_attr` and
//! pushes notifications via `notify`. This avoids hand-building the
//! raw `esp_gatt_rsp_t` read responses.

use core::borrow::Borrow;

use enumset::EnumSet;
use esp_idf_svc::bt::ble::gap::{AdvConfiguration, EspBleGap};
use esp_idf_svc::bt::ble::gatt::server::{EspGatts, GattsEvent};
use esp_idf_svc::bt::ble::gatt::{
    AutoResponse, GattCharacteristic, GattId, GattInterface, GattServiceId, GattStatus, Permission,
    Property,
};
use esp_idf_svc::bt::{BleEnabled, BtDriver, BtUuid};
use esp_idf_svc::sys::EspError;

/// mAgent Configuration Service (matches `ble_config.rs` / `ble_wallet.rs`).
pub const CONFIG_SERVICE_UUID16: u16 = 0x1850;
pub const WIFI_SSID_UUID16: u16 = 0x2A01;
pub const WIFI_PASS_UUID16: u16 = 0x2A02;
pub const STATUS_UUID16: u16 = 0x2A06;
pub const DEVICE_INFO_UUID16: u16 = 0x2A07;
pub const SYS_CMD_UUID16: u16 = 0x2A08;
pub const SYS_RSP_UUID16: u16 = 0x2A09;

const APP_ID: u16 = 0;
const NUM_HANDLES: u16 = 6;
/// Max value length for a characteristic.
const MAX_ATTR_LEN: usize = 256;

/// How far the async GATT setup has progressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleLifecycle {
    Initialising,
    ServiceCreating,
    AddingCharacteristics,
    Advertising,
    Connected,
    Error,
}

/// Characteristic index within the service (stable ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharIdx {
    WifiSsid = 0,
    WifiPass = 1,
    Status = 2,
    DeviceInfo = 3,
    SysCmd = 4,
    SysRsp = 5,
}

/// A running BLE peripheral: a GATT server + GAP advertiser over the same
/// BLE driver. `M` is the BLE mode (from the driver), `T` the driver.
pub struct BleServer<'d, M, T>
where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    gatts: EspGatts<'d, M, T>,
    gap: EspBleGap<'d, M, T>,
}

/// State shared between the ESP-IDF event thread and the main thread.
#[derive(Debug, Default)]
pub struct SharedState {
    pub lifecycle: BleLifecycle,
    pub conn_id: u16,
    /// Characteristic handles, indexed by [`CharIdx`].
    pub handles: [u16; 6],
    pub wifi_ssid: heapless::String<32>,
    pub wifi_pass: heapless::String<64>,
    pub last_sys_cmd: heapless::Vec<u8, MAX_ATTR_LEN>,
}

impl SharedState {
    pub fn new() -> Self {
        Self::default()
    }
}


/// True if `uuid` (from [`BtUuid::as_bytes`]) is the given 16-bit UUID.
fn uuid_is(uuid: &[u8], short: u16) -> bool {
    uuid.len() == 2 && uuid == short.to_le_bytes()
}

/// The service id for the mAgent Configuration Service.
pub fn service_id() -> GattServiceId {
    GattServiceId {
        id: GattId {
            uuid: BtUuid::uuid16(CONFIG_SERVICE_UUID16),
            inst_id: 0,
        },
        is_primary: true,
    }
}

/// The six mAgent configuration characteristics, indexed by [`CharIdx`].
pub fn characteristics() -> [GattCharacteristic; 6] {
    let mut defs = [GattCharacteristic::new(
        BtUuid::uuid16(STATUS_UUID16),
        EnumSet::from(Permission::Read),
        EnumSet::from(Property::Read | Property::Notify),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    ); 6];

    defs[CharIdx::WifiSsid as usize] = GattCharacteristic::new(
        BtUuid::uuid16(WIFI_SSID_UUID16),
        EnumSet::from(Permission::Write),
        EnumSet::from(Property::Write),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    );
    defs[CharIdx::WifiPass as usize] = GattCharacteristic::new(
        BtUuid::uuid16(WIFI_PASS_UUID16),
        EnumSet::from(Permission::Write),
        EnumSet::from(Property::Write),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    );
    defs[CharIdx::DeviceInfo as usize] = GattCharacteristic::new(
        BtUuid::uuid16(DEVICE_INFO_UUID16),
        EnumSet::from(Permission::Read),
        EnumSet::from(Property::Read),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    );
    defs[CharIdx::SysCmd as usize] = GattCharacteristic::new(
        BtUuid::uuid16(SYS_CMD_UUID16),
        EnumSet::from(Permission::Write),
        EnumSet::from(Property::Write),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    );
    defs[CharIdx::SysRsp as usize] = GattCharacteristic::new(
        BtUuid::uuid16(SYS_RSP_UUID16),
        EnumSet::from(Permission::Read),
        EnumSet::from(Property::Read | Property::Notify),
        MAX_ATTR_LEN,
        AutoResponse::ByGatt,
    );
    defs
}

/// Map a characteristic UUID to its [`CharIdx`].
pub fn char_index_for_uuid(uuid: BtUuid) -> Option<CharIdx> {
    let b = uuid.as_bytes();
    match () {
        _ if uuid_is(b, WIFI_SSID_UUID16) => Some(CharIdx::WifiSsid),
        _ if uuid_is(b, WIFI_PASS_UUID16) => Some(CharIdx::WifiPass),
        _ if uuid_is(b, STATUS_UUID16) => Some(CharIdx::Status),
        _ if uuid_is(b, DEVICE_INFO_UUID16) => Some(CharIdx::DeviceInfo),
        _ if uuid_is(b, SYS_CMD_UUID16) => Some(CharIdx::SysCmd),
        _ if uuid_is(b, SYS_RSP_UUID16) => Some(CharIdx::SysRsp),
        _ => None,
    }
}

impl<'d, M, T> BleServer<'d, M, T>
where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    /// Create the BLE server over an existing BLE driver. Pass the driver
    /// by shared reference (e.g. `&driver`) so it can be shared between the
    /// GATT server and the GAP advertiser.
    pub fn new(driver: T) -> Result<Self, EspError> {
        let gatts = EspGatts::new(driver)?;
        let gap = EspBleGap::new(driver)?;
        Ok(Self { gatts, gap })
    }

    /// Register the GATT application; the GATTS event handler (installed
    /// by [`run_server`]) then drives `CREATE → ADD_CHAR → START`.
    pub fn start(&self) -> Result<(), EspError> {
        self.gatts.register_app(APP_ID)
    }
}


/// Configure advertising to include the device name + config-service UUID,
/// then start advertising. Called when the service is started and again
/// after a disconnection.
fn start_advertising<'d, M, T>(gap: &EspBleGap<'d, M, T>) -> Result<(), EspError>
where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    let conf = AdvConfiguration {
        include_name: true,
        service_uuid: Some(BtUuid::uuid16(CONFIG_SERVICE_UUID16)),
        ..Default::default()
    };
    gap.set_adv_conf(&conf)?;
    gap.start_advertising()
}

/// Record a written value into the shared state, keyed by the attribute
/// handle that was just written.
fn handle_write(state: &mut SharedState, handle: u16, value: &[u8]) {
    let idx = match state.handles.iter().position(|&h| h == handle) {
        Some(i) => i,
        None => return,
    };
    let text = core::str::from_utf8(value).unwrap_or("");
    match idx {
        i if i == CharIdx::WifiSsid as usize => {
            state.wifi_ssid.clear();
            let _ = state.wifi_ssid.push_str(text);
        }
        i if i == CharIdx::WifiPass as usize => {
            state.wifi_pass.clear();
            let _ = state.wifi_pass.push_str(text);
        }
        i if i == CharIdx::SysCmd as usize => {
            state.last_sys_cmd.clear();
            let _ = state.last_sys_cmd.extend_from_slice(value);
        }
        _ => {}
    }
}

/// Install the GATTS event handler and begin service bring-up.
///
/// # Safety
///
/// Uses `subscribe_nonstatic`: the callback runs on a hidden ESP-IDF
/// thread while borrowing `server`'s GATT/GAP handles. `server` (and the
/// `BtDriver` it references) MUST outlive `state` and must not be
/// `mem::forget`-ed while the callback is installed.
pub unsafe fn run_server<'d, M, T>(
    server: &BleServer<'d, M, T>,
    state: std::sync::Arc<std::sync::Mutex<SharedState>>,
) -> Result<(), EspError>
where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    let gatts = &server.gatts;
    let gap = &server.gap;
    server.gatts.subscribe_nonstatic(move |(gi, event)| {
        on_event(gatts, gap, gi, &event, &state);
    })?;
    server.gatts.register_app(APP_ID)
}

/// Process one GATTS event, driving the async service lifecycle and
/// read/write/notify handling.
fn on_event<'d, M, T>(
    gatts: &EspGatts<'d, M, T>,
    gap: &EspBleGap<'d, M, T>,
    gi: esp_idf_svc::bt::ble::gatt::GattInterface,
    event: &GattsEvent,
    state: &std::sync::Arc<std::sync::Mutex<SharedState>>,
) where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    let mut st = match state.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    match event {
        GattsEvent::ServiceRegistered { status, .. } => {
            if *status == GattStatus::Ok {
                st.lifecycle = BleLifecycle::ServiceCreating;
                let _ = gatts.create_service(gi, &service_id(), NUM_HANDLES);
            } else {
                st.lifecycle = BleLifecycle::Error;
            }
        }
        GattsEvent::ServiceCreated { status, service_handle, .. } => {
            if *status == GattStatus::Ok {
                st.lifecycle = BleLifecycle::AddingCharacteristics;
                for i in 0..6 {
                    let _ = gatts.add_characteristic(*service_handle, &characteristics()[i], &[]);
                }
            } else {
                st.lifecycle = BleLifecycle::Error;
            }
        }
        GattsEvent::CharacteristicAdded { status, service_handle, attr_handle, char_uuid, .. } => {
            if *status == GattStatus::Ok {
                if let Some(idx) = char_index_for_uuid(*char_uuid) {
                    st.handles[idx as usize] = *attr_handle;
                }
                // All six are added in one burst; when the last one lands,
                // start the service.
                if st.handles.iter().all(|&h| h != 0) {
                    let _ = gatts.start_service(*service_handle);
                }
            }
        }
        GattsEvent::ServiceStarted { status, .. } => {
            if *status == GattStatus::Ok {
                st.lifecycle = BleLifecycle::Advertising;
                let _ = start_advertising(gap);
            } else {
                st.lifecycle = BleLifecycle::Error;
            }
        }
        GattsEvent::Write { conn_id, handle, value, .. } => {
            st.conn_id = *conn_id;
            handle_write(&mut st, *handle, value);
        }
        GattsEvent::PeerConnected { conn_id, .. } => {
            st.lifecycle = BleLifecycle::Connected;
            st.conn_id = *conn_id;
            let _ = gap.stop_advertising();
        }
        GattsEvent::PeerDisconnected { .. } => {
            st.lifecycle = BleLifecycle::Advertising;
            st.conn_id = 0;
            let _ = start_advertising(gap);
        }
        GattsEvent::Mtu { mtu, .. } => {
            log::info!("[ble-gatt] MTU negotiated: {mtu}");
        }
        _ => {}
    }
}

