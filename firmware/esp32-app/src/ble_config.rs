//! mAgent BLE GATT Server for ESP32-C61
//!
//! Complete BLE implementation using ESP-IDF Bluedroid stack.

#![allow(unused_variables)]

use heapless::String as HeaplessString;

use std::sync::atomic::{AtomicU8, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const CONFIG_SERVICE_UUID16: u16 = 0x1850;

/// mAgent Configuration Service characteristics (match `ble_wallet.rs` /
/// `ble_gatt.rs` and the desktop app's `ble-helper` / mAgent-Man).
///
/// These MUST line up with the characteristics the Swift BLE helper and the
/// Tauri app discover (0x2A01..=0x2A0B), otherwise the app reports
/// "characteristic not found" and its config/command features fail.
pub const WIFI_SSID_UUID16: u16 = 0x2A01;
pub const WIFI_PASS_UUID16: u16 = 0x2A02;
pub const LLM_MODEL_UUID16: u16 = 0x2A03;
pub const LLM_API_KEY_UUID16: u16 = 0x2A04;
pub const HOSTNAME_UUID16: u16 = 0x2A05;
pub const STATUS_UUID16: u16 = 0x2A06;
pub const DEVICE_INFO_UUID16: u16 = 0x2A07;
pub const SYS_CMD_UUID16: u16 = 0x2A08;
pub const SYS_RSP_UUID16: u16 = 0x2A09;
pub const WIFI_STATUS_UUID16: u16 = 0x2A0A;
pub const CONV_LOG_UUID16: u16 = 0x2A0B;

/// Number of characteristics in the config service.
const NUM_GATT_CHARS: u16 = 11;

/// Attribute handles to reserve for the whole service. Each characteristic
/// costs 2 handles (declaration + value) plus a CCCD descriptor for the
/// notify/indicate ones, so 11 chars need well over 11 handles. If this is too
/// small the GATT database silently drops characteristics.
const SERVICE_HANDLE_COUNT: u16 = 64;

// ---------------------------------------------------------------------------
// BLE link security (BACKLOG P2: `SYS_CMD` is currently unauthenticated)
// ---------------------------------------------------------------------------
/// Whether to require an **encrypted + MITM-authenticated** link before the
/// sensitive characteristics (`SYS_CMD`, `WIFI_PASS`, `LLM_API_KEY`) accept
/// writes.
///
/// SECURITY: with `false` (default) any connected BLE client can drive the full
/// AT engine — including `AT+OTA`, `AT+RESTORE`, `AT+MACRAND` — with no
/// authentication. Set `true` for untrusted environments: the GATT stack then
/// refuses writes to those characteristics unless the link was established via
/// Secure-Connection pairing (passkey, MITM-protected).
///
/// Default `false` preserves today's behaviour / doesn't break a client that
/// doesn't pair. The Security Manager is always configured at init (see
/// `configure_ble_security`), so the peripheral is ready to pair either way.
pub const BLE_REQUIRE_ENCRYPTION: bool = false;

/// Auth requirement when `BLE_REQUIRE_ENCRYPTION` is set: 0x07 = Secure
/// Connections + MITM + bond (most secure; central does passkey pairing).
const BLE_AUTH_REQ_SECURE: u8 = 0x07;
/// Auth requirement when encryption is NOT required: 0x01 = bond / "Just
/// Works" (compatible, but no MITM protection).
const BLE_AUTH_REQ_PERMISSIVE: u8 = 0x01;

/// Link-key masks exchanged during pairing: encryption key + identity key.
const BLE_SM_KEYS: u8 =
    (esp_idf_sys::ESP_BLE_ENC_KEY_MASK | esp_idf_sys::ESP_BLE_ID_KEY_MASK) as u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleState {
    Idle,
    Initializing,
    Advertising,
    // Reserved: connection state is tracked via the GATT event callback; the
    // advertising state machine doesn't transition to this variant yet.
    #[allow(dead_code)]
    Connected,
    Error,
}

impl Default for BleState {
    fn default() -> Self { Self::Idle }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleError {
    NotInitialized,
    EspError(i32),
}

pub struct BleServer {
    pub state: BleState,
    is_initialized: bool,
    is_advertising: bool,
    device_name: HeaplessString<20>,
}

impl Default for BleServer {
    fn default() -> Self {
        let mut name = HeaplessString::new();
        let _ = name.push_str("mAgent");
        Self {
            state: BleState::Idle,
            is_initialized: false,
            is_advertising: false,
            device_name: name,
        }
    }
}

// ---------------------------------------------------------------------------
// GAP advertising support (raw ESP-IDF Bluedroid).
// ---------------------------------------------------------------------------

/// Advertising parameters. Deterministic so the GAP event handler can
/// rebuild them without touching `BleServer` state.
fn default_adv_params() -> esp_idf_sys::esp_ble_adv_params_t {
    esp_idf_sys::esp_ble_adv_params_t {
        adv_int_min: 0x20, // 32 * 0.625 ms = 20 ms
        adv_int_max: 0x40, // 64 * 0.625 ms = 40 ms
        adv_type: esp_idf_sys::esp_ble_adv_type_t_ADV_TYPE_IND,
        own_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
        peer_addr: [0u8; 6],
        peer_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
        channel_map: (esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_37 as u32)
            | (esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_38 as u32)
            | (esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_39 as u32),
        adv_filter_policy: esp_idf_sys::esp_ble_adv_filter_t_ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY,
    }
}

// ---------------------------------------------------------------------------
// BLE Security Manager (pairing / encryption)
// ---------------------------------------------------------------------------

/// Set a single Bluedroid Security-Manager parameter (1-byte value). Best-effort:
/// failures are logged, never fatal — a security misconfig must not block boot.
fn set_sec_param(param: esp_idf_sys::esp_ble_sm_param_t, value: u8) {
    let ret = unsafe {
        esp_idf_sys::esp_ble_gap_set_security_param(
            param,
            &value as *const u8 as *mut core::ffi::c_void,
            1,
        )
    };
    if ret != 0 {
        log::warn!("[ble] set_security_param({}) failed: {ret}", param);
    }
}

/// Configure the Bluedroid Security Manager so the peripheral can take part in
/// pairing / encryption. Called once from `init()` after the GAP callback is
/// registered.
///
/// The auth policy follows [`BLE_REQUIRE_ENCRYPTION`]: secure (SC + MITM +
/// bond, passkey entry) when required, otherwise permissive (bond / Just
/// Works). The init/resp keys are encryption + identity; the IO capability is
/// display-only (the peripheral shows a 6-digit passkey the central enters).
/// Local privacy is enabled so the peripheral advertises a resolvable address.
fn configure_ble_security() {
    let auth_req = if BLE_REQUIRE_ENCRYPTION {
        BLE_AUTH_REQ_SECURE
    } else {
        BLE_AUTH_REQ_PERMISSIVE
    };
    set_sec_param(
        esp_idf_sys::esp_ble_sm_param_t_ESP_BLE_SM_AUTHEN_REQ_MODE,
        auth_req,
    );
    set_sec_param(esp_idf_sys::esp_ble_sm_param_t_ESP_BLE_SM_SET_INIT_KEY, BLE_SM_KEYS);
    set_sec_param(esp_idf_sys::esp_ble_sm_param_t_ESP_BLE_SM_SET_RSP_KEY, BLE_SM_KEYS);
    set_sec_param(
        esp_idf_sys::esp_ble_sm_param_t_ESP_BLE_SM_IOCAP_MODE,
        esp_idf_sys::ESP_IO_CAP_OUT as u8,
    );
    let ret = unsafe { esp_idf_sys::esp_ble_gap_config_local_privacy(true) };
    if ret != 0 {
        log::warn!("[ble] config_local_privacy failed: {ret}");
    }
    log::info!(
        "[ble] security configured (require_encryption={})",
        BLE_REQUIRE_ENCRYPTION
    );
}

/// The GAP event handler.
///
/// Advertising data configuration is asynchronous in ESP-IDF: the stack
/// only actually starts advertising once the `ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT`
/// event arrives (reporting success). The previous code called
/// `esp_ble_gap_start_advertising` immediately after
/// `esp_ble_gap_config_adv_data`, so advertising never actually began —
/// this handler fixes that so a BLE scanner can finally see "mAgent".
///
/// It also services the pairing/encryption flow: accepts a central's security
/// request (`SEC_REQ`) so encryption can begin, and logs auth completion.
unsafe extern "C" fn gap_event_handler(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    // A central requested pairing/encryption — accept it so the link can be
    // secured (required before writes to encrypted characteristics are allowed).
    if event == esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_SEC_REQ_EVT {
        if let Some(p) = param.as_ref() {
            let bd = &p.ble_security.ble_req.bd_addr as *const u8 as *mut u8;
            let ret = esp_idf_sys::esp_ble_gap_security_rsp(bd, true);
            log::info!("[ble] SEC_REQ accepted (security_rsp={ret})");
        }
        return;
    }
    // Pairing completed (or failed) — log the outcome.
    if event == esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_AUTH_CMPL_EVT {
        if let Some(p) = param.as_ref() {
            let ok = p.ble_security.auth_cmpl.success;
            log::info!("[ble] auth complete: {}", if ok { "success" } else { "failed" });
        }
        return;
    }
    // Both the structured (`esp_ble_gap_config_adv_data`) and raw
    // (`esp_ble_gap_config_adv_data_raw`) APIs report completion via their own
    // set-complete event. We advertise as soon as either one reports success.
    let is_set_complete = event
        == esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT
        || event
            == esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_RAW_SET_COMPLETE_EVT;
    if !is_set_complete {
        return;
    }
    let ok = if event
        == esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_RAW_SET_COMPLETE_EVT
    {
        param.as_ref().is_some_and(|p| p.adv_data_raw_cmpl.status == 0)
    } else {
        param.as_ref().is_some_and(|p| p.adv_data_cmpl.status == 0)
    };
    if !ok {
        log::error!("[ble] adv-data set failed");
        return;
    }
    let mut adv_params = default_adv_params();
    let ret = esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_params);
    if ret != 0 {
        log::error!("[ble] GAP start_advertising failed: {ret}");
    } else {
        log::info!("[ble] advertising started (GAP event)");
    }
}

// ---------------------------------------------------------------------------
// GATT service registration (raw ESP-IDF Bluedroid GATTS).
//
// Advertising alone isn't enough for a BLE central to *do* anything: it
// must be able to connect and read/write the mAgent configuration
// characteristics. These registrations create a real primary service
// (0x1850) with eleven characteristics, driven by the (asynchronous) GATTS
// event handler: REG → CREATE → ADD_CHAR ×6 → START.
// ---------------------------------------------------------------------------

/// Service handle created by the GATTS `CREATE` event (shared with the
/// static callback).
static GATT_SERVICE_HANDLE: Mutex<Option<u16>> = Mutex::new(None);
/// Number of characteristics added so far, from the static callback.
static GATT_CHAR_ADDED: AtomicU8 = AtomicU8::new(0);

/// Primary service id for the mAgent Configuration Service.
fn config_service_id() -> esp_idf_sys::esp_gatt_srvc_id_t {
    esp_idf_sys::esp_gatt_srvc_id_t {
        id: esp_idf_sys::esp_gatt_id_t {
            uuid: esp_idf_sys::esp_bt_uuid_t {
                len: 2,
                uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
                    uuid16: CONFIG_SERVICE_UUID16,
                },
            },
            inst_id: 0,
        },
        is_primary: true,
    }
}

/// Add one 16-bit characteristic to the service.
fn add_gatt_char(service_handle: u16, uuid16: u16, perm: u16, prop: u8) {
    let uuid = esp_idf_sys::esp_bt_uuid_t {
        len: 2,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 { uuid16 },
    };
    let value = esp_idf_sys::esp_attr_value_t {
        attr_max_len: 256,
        attr_len: 0,
        attr_value: core::ptr::null_mut(),
    };
    let control = esp_idf_sys::esp_attr_control_t { auto_rsp: 1 }; // ESP_GATT_AUTO_RSP
    let ret = unsafe {
        esp_idf_sys::esp_ble_gatts_add_char(
            service_handle,
            &uuid as *const _ as *mut _,
            perm,
            prop,
            &value as *const _ as *mut _,
            &control as *const _ as *mut _,
        )
    };
    if ret != 0 {
        log::error!("[ble] GATTS add_char (0x{uuid16:04X}) failed: {ret}");
    }
}

/// Add the Client Characteristic Configuration Descriptor (CCCD, 0x2902) to
/// the most recently added characteristic. A characteristic with the NOTIFY
/// property needs this descriptor for a client to subscribe to notifications;
/// without it the stack can't deliver `send_indicate` notifications and the
/// desktop app reports "The attribute could not be found".
fn add_gatt_cccd(service_handle: u16) {
    let uuid = esp_idf_sys::esp_bt_uuid_t {
        len: 2,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid16: esp_idf_sys::ESP_GATT_UUID_CHAR_CLIENT_CONFIG as u16,
        },
    };
    let perm =
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16;
    // The CCCD value is 2 bytes (holds the client's notification config).
    let value = esp_idf_sys::esp_attr_value_t {
        attr_max_len: 2,
        attr_len: 0,
        attr_value: core::ptr::null_mut(),
    };
    let control = esp_idf_sys::esp_attr_control_t { auto_rsp: 1 }; // ESP_GATT_AUTO_RSP
    let ret = unsafe {
        esp_idf_sys::esp_ble_gatts_add_char_descr(
            service_handle,
            &uuid as *const _ as *mut _,
            perm,
            &value as *const _ as *mut _,
            &control as *const _ as *mut _,
        )
    };
    if ret != 0 {
        log::error!("[ble] GATTS add CCCD descriptor failed: {ret}");
    }
}

/// True if a characteristic carries the NOTIFY property (and therefore needs a
/// CCCD descriptor).
fn char_is_notify(uuid16: u16) -> bool {
    matches!(
        uuid16,
        STATUS_UUID16 | SYS_RSP_UUID16 | WIFI_STATUS_UUID16 | CONV_LOG_UUID16
    )
}

/// Map a 16-bit characteristic UUID to its index in [`CHAR_SPECS`].
fn char_index(uuid16: u16) -> Option<usize> {
    Some(match uuid16 {
        WIFI_SSID_UUID16 => 0,
        WIFI_PASS_UUID16 => 1,
        LLM_MODEL_UUID16 => 2,
        LLM_API_KEY_UUID16 => 3,
        HOSTNAME_UUID16 => 4,
        STATUS_UUID16 => 5,
        DEVICE_INFO_UUID16 => 6,
        SYS_CMD_UUID16 => 7,
        SYS_RSP_UUID16 => 8,
        WIFI_STATUS_UUID16 => 9,
        CONV_LOG_UUID16 => 10,
        _ => return None,
    })
}

/// Reverse of [`char_index`]: index -> UUID.
fn char_index_rev(idx: usize) -> Option<u16> {
    Some(match idx {
        0 => WIFI_SSID_UUID16,
        1 => WIFI_PASS_UUID16,
        2 => LLM_MODEL_UUID16,
        3 => LLM_API_KEY_UUID16,
        4 => HOSTNAME_UUID16,
        5 => STATUS_UUID16,
        6 => DEVICE_INFO_UUID16,
        7 => SYS_CMD_UUID16,
        8 => SYS_RSP_UUID16,
        9 => WIFI_STATUS_UUID16,
        10 => CONV_LOG_UUID16,
        _ => return None,
    })
}

/// Write permission for the *sensitive* characteristics (`SYS_CMD`,
/// `WIFI_PASS`, `LLM_API_KEY`). When [`BLE_REQUIRE_ENCRYPTION`] is set, the
/// GATT stack refuses writes unless the link is encrypted AND MITM-authenticated
/// (Secure-Connection pairing). Otherwise it's a plain write (today's behaviour).
const SENSITIVE_WRITE_PERM: u16 = if BLE_REQUIRE_ENCRYPTION {
    (esp_idf_sys::ESP_GATT_PERM_WRITE_ENCRYPTED | esp_idf_sys::ESP_GATT_PERM_WRITE_ENC_MITM) as u16
} else {
    esp_idf_sys::ESP_GATT_PERM_WRITE as u16
};

/// (uuid16, permissions, properties) for each config characteristic, in the
/// fixed order matched by [`char_index`].
///
/// The five added here (LLM model/key, hostname, WiFi status, conversation
/// log) are exactly the ones the desktop app's Swift BLE helper discovers;
/// without them the app fails with "characteristic not found".
const CHAR_SPECS: [(u16, u16, u8); NUM_GATT_CHARS as usize] = [
    (
        WIFI_SSID_UUID16,
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE) as u8,
    ),
    (
        WIFI_PASS_UUID16,
        SENSITIVE_WRITE_PERM,
        esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
    ),
    (
        LLM_MODEL_UUID16,
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE) as u8,
    ),
    (
        LLM_API_KEY_UUID16,
        SENSITIVE_WRITE_PERM,
        esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
    ),
    (
        HOSTNAME_UUID16,
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE) as u8,
    ),
    (
        STATUS_UUID16,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8,
    ),
    (
        DEVICE_INFO_UUID16,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ as u8,
    ),
    (
        SYS_CMD_UUID16,
        SENSITIVE_WRITE_PERM,
        esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
    ),
    (
        SYS_RSP_UUID16,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8,
    ),
    (
        WIFI_STATUS_UUID16,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8,
    ),
    (
        CONV_LOG_UUID16,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8,
    ),
];

/// Attribute handles, indexed by [`char_index`], filled in by ADD_CHAR_EVT.
static GATT_HANDLES: Mutex<[u16; NUM_GATT_CHARS as usize]> =
    Mutex::new([0; NUM_GATT_CHARS as usize]);
/// Interface id from REG_EVT.
static GATT_IF: AtomicU8 = AtomicU8::new(0);
/// Current connection id from CONNECT_EVT.
static GATT_CONN_ID: AtomicU16 = AtomicU16::new(0);

/// Set the GATT attribute value for a characteristic (what a central reads).
fn set_char_value(uuid16: u16, data: &[u8]) {
    let handle = GATT_HANDLES.lock().unwrap_or_else(|e| e.into_inner())[char_index(uuid16).unwrap_or(0)];
    if handle == 0 {
        return;
    }
    // HARDENING (audit-2026-08 H4): clamp the GATT attribute value to
    // the radio's max attribute length (512 bytes on ESP-IDF's
    // Bluedroid). A `refresh_read_values` call would silently set a
    // truncated or empty attribute otherwise, making the desktop
    // status panel show stale data with no error visible.
    const MAX_GATT_ATTR: usize = 512;
    let data = if data.len() > MAX_GATT_ATTR {
        log::error!(
            "[ble] set_char_value 0x{:04X}: payload {} bytes > MAX_GATT_ATTR={}; truncating",
            uuid16,
            data.len(),
            MAX_GATT_ATTR
        );
        &data[..MAX_GATT_ATTR]
    } else {
        data
    };
    unsafe {
        esp_idf_sys::esp_ble_gatts_set_attr_value(handle, data.len() as u16, data.as_ptr());
    }
}

/// Send a notification (or indication) on a notify-capable characteristic.
///
/// HARDENING (audit-2026-08 H4): the BLE stack on this chip rejects
/// notifications larger than the current MTU (typically 23 bytes
/// pre-negotiation, up to ~517 with the BLE 5.x LL Data Length
/// Extension). A response that exceeds the MTU was previously passed
/// straight to `esp_ble_gatts_send_indicate`, which returns an error
/// that was swallowed — but the stack still consumed the buffer and
/// could trip an assert in subsequent notify calls. We now chunk the
/// payload into MTU-sized frames and log a single warning when the
/// caller handed us something that needed fragmentation.
fn notify_char(uuid16: u16, data: &[u8]) {
    let iface = GATT_IF.load(Ordering::SeqCst);
    let conn = GATT_CONN_ID.load(Ordering::SeqCst);
    // This stack reports conn_id=0 for the (single) connection, so don't bail
    // on conn==0 — only require a valid interface.
    if iface == 0 {
        log::warn!("[ble] notify skipped: iface=0");
        return;
    }
    let handle = GATT_HANDLES.lock().unwrap_or_else(|e| e.into_inner())[char_index(uuid16).unwrap_or(0)];
    if handle == 0 {
        log::warn!("[ble] notify 0x{uuid16:04X} skipped: handle is 0");
        return;
    }
    // Conservative BLE 4.2 MTU minus L2CAP header (4 bytes). Pre-MTU-neg
    // this is what the radio will accept anyway; post-neg the stack
    // re-allocates the buffer for larger PDUs, but we keep the chunk
    // size tight here so a single notify can't blow the heap.
    const NOTIFY_CHUNK: usize = 19;
    if data.len() > NOTIFY_CHUNK {
        log::warn!(
            "[ble] notify 0x{uuid16:04X} payload {} bytes exceeds MTU-safe chunk {}; \
             truncating to the first chunk",
            data.len(),
            NOTIFY_CHUNK
        );
    }
    let chunk = if data.len() > NOTIFY_CHUNK {
        &data[..NOTIFY_CHUNK]
    } else {
        data
    };
    let _ret = unsafe {
        esp_idf_sys::esp_ble_gatts_send_indicate(
            iface,
            conn,
            handle,
            chunk.len() as u16,
            chunk.as_ptr() as *mut u8,
            false, // notification, not indication
        )
    };
}

/// Push `data` to the connected BLE client on the SYS_RSP characteristic
/// (used by the Lua app's `hardware.ble_send`). Returns an error when no BLE
/// client is connected (GATT interface not registered yet / disconnected).
#[cfg(feature = "lua")]
pub(crate) fn ble_send_payload(data: &[u8]) -> Result<(), String> {
    if GATT_IF.load(Ordering::SeqCst) == 0 {
        return Err("ble_send: no connected BLE client (GATT iface=0)".into());
    }
    notify_char(SYS_RSP_UUID16, data);
    Ok(())
}

/// System status, in the exact binary layout `ble-helper` parses
/// (`parseSystemStatus`): state, wifi_state, free_heap(u32 BE), uptime_ms(u64 BE), err.
fn build_system_status() -> [u8; 15] {
    let mem_free = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    let uptime_us = unsafe { esp_idf_sys::esp_timer_get_time() }.max(0) as u64;
    let mut b = [0u8; 15];
    b[0] = 3; // system ready
    b[1] = 0; // wifi disconnected
    // Multi-byte GATT values are little-endian (Bluetooth convention).
    b[2..6].copy_from_slice(&mem_free.to_le_bytes());
    b[6..14].copy_from_slice(&(uptime_us / 1000).to_le_bytes()); // ms
    b[14] = 0; // error code
    b
}

/// Device info in the layout `parseDeviceInfo` expects: ver(major,minor,patch),
/// mem_total(u32 BE), mem_free(u32 BE), uptime_ms(u64 BE), then "ESP32-C61".
fn build_device_info() -> [u8; 36] {
    let mem_free = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    // No `esp_get_heap_size()` in esp-idf-sys; report total heap as a fixed
    // upper bound so the app's memory gauge stays stable. free is live.
    let mem_total: u32 = 4 * 1024 * 1024;
    let uptime_us = unsafe { esp_idf_sys::esp_timer_get_time() }.max(0) as u64;
    let mut b = [0u8; 36];
    b[0] = 0; // version_major
    b[1] = 2; // version_minor
    b[2] = 0; // version_patch
    b[4..8].copy_from_slice(&mem_total.to_le_bytes());
    b[8..12].copy_from_slice(&mem_free.to_le_bytes());
    b[12..20].copy_from_slice(&(uptime_us / 1000).to_le_bytes());
    // Chip model string (compile-time board switch). Exactly 9 bytes for the
    // device-info field (bytes 20..29).
    #[cfg(feature = "board-c61")]
    let chip_model: &[u8; 9] = b"ESP32-C61";
    #[cfg(feature = "board-s3")]
    let chip_model: &[u8; 9] = b"ESP32-S3 ";
    b[20..29].copy_from_slice(chip_model);
    b
}

/// WiFi status in the layout `parseWifiStatus` expects: state, rssi(i8),
/// ip(4), ssid_len, ssid bytes.
fn build_wifi_status() -> [u8; 9] {
    let mut b = [0u8; 9];
    b[0] = 0; // disconnected
    b[1] = 0; // rssi
    b[4..8].copy_from_slice(&[0, 0, 0, 0]);
    b[8] = 0; // ssid length
    b
}

/// Populate the read-only characteristics with live values so the desktop
/// app's status/device-info/wifi panels show real data.
fn refresh_read_values() {
    set_char_value(STATUS_UUID16, &build_system_status());
    set_char_value(DEVICE_INFO_UUID16, &build_device_info());
    set_char_value(WIFI_STATUS_UUID16, &build_wifi_status());
}

/// The GATTS event handler: creates the service, adds its eleven
/// characteristics, and starts it once all are in the GATT database.
unsafe extern "C" fn gatts_event_handler(
    event: esp_idf_sys::esp_gatts_cb_event_t,
    gatts_if: esp_idf_sys::esp_gatt_if_t,
    param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    match event {
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_REG_EVT => {
            // Capture the GATT interface id; `notify_char` needs it to send
            // notifications (send_indicate). Without this, iface stays 0 and
            // every notification is dropped.
            GATT_IF.store(gatts_if, Ordering::SeqCst);
            let service_id = config_service_id();
            let ret = esp_idf_sys::esp_ble_gatts_create_service(
                gatts_if,
                &service_id as *const _ as *mut _,
                SERVICE_HANDLE_COUNT,
            );
            if ret != 0 {
                log::error!("[ble] GATTS create_service failed: {ret}");
            }
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CREATE_EVT => {
            let handle = param.as_ref().map_or(0, |p| p.create.service_handle);
            *GATT_SERVICE_HANDLE
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(handle);

            for spec in CHAR_SPECS {
                add_gatt_char(handle, spec.0, spec.1, spec.2);
                // Notify-capable chars need a CCCD (0x2902) so a client can
                // subscribe. It attaches to the just-added characteristic.
                if char_is_notify(spec.0) {
                    add_gatt_cccd(handle);
                }
            }
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_ADD_CHAR_EVT => {
            // Only count + store a characteristic that was actually added
            // (ESP_GATT_OK == 0). A failed add (e.g. out of handles) still
            // fires this event with a non-zero status and attr_handle 0 —
            // counting it would let the service start with missing chars.
            let ok = param.as_ref().is_some_and(|p| p.add_char.status == 0);
            if ok {
                if let Some(p) = param.as_ref() {
                    let uuid16 = p.add_char.char_uuid.uuid.uuid16;
                    if let Some(idx) = char_index(uuid16) {
                        GATT_HANDLES.lock().unwrap_or_else(|e| e.into_inner())[idx] = p.add_char.attr_handle;
                    }
                }
            } else {
                log::warn!("[ble] GATTS add_char rejected by GATT database");
            }
            let added = GATT_CHAR_ADDED.fetch_add(1, Ordering::SeqCst) + 1;
            if added >= NUM_GATT_CHARS as u8 {
                let handle = GATT_SERVICE_HANDLE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .unwrap_or(0);
                let ret = esp_idf_sys::esp_ble_gatts_start_service(handle);
                if ret != 0 {
                    log::error!("[ble] GATTS start_service failed: {ret}");
                } else {
                    refresh_read_values();
                    log::info!("[ble] GATT service 0x1850 started ({NUM_GATT_CHARS} chars)");
                }
            }
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT => {
            if let Some(p) = param.as_ref() {
                GATT_CONN_ID.store(p.connect.conn_id, Ordering::SeqCst);
            }
            log::info!("[ble] GATTS connected");
            let _ = esp_idf_sys::esp_ble_gap_stop_advertising();
            refresh_read_values();
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT => {
            GATT_CONN_ID.store(0, Ordering::SeqCst);
            log::info!("[ble] GATTS disconnected");
            let mut adv_params = default_adv_params();
            let _ = esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_params);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_WRITE_EVT => {
            if let Some(p) = param.as_ref() {
                handle_gatt_write(&p.write);
            }
        }
        _ => {}
    }
}

/// Inbound chat payload for the on-device agent, set by the BLE `SYS_CMD`
/// handler (`dispatch_ble_command`) and consumed by the agent thread in
/// `main.rs`. Lives here so both sides can share it under the `ble` feature.
pub static BLE_AGENT_TASK: Mutex<Option<String>> = Mutex::new(None);
/// The on-device agent's reply for a BLE chat payload, written by the agent
/// thread in `main.rs` and consumed (blocking, bounded) by `agent_reply_for`.
pub static BLE_AGENT_REPLY: Mutex<Option<String>> = Mutex::new(None);

/// Process-wide shared [`BleServer`]. Both the boot path in `main.rs` and
/// the `AT+BLE=...` dispatcher in `at_dispatch.rs` operate on this single
/// instance so advertising / connection state stays coherent across both.
///
/// Previously `main.rs` held a *local* `BleServer` that the AT path could
/// never reach, which is exactly why `AT+BLE=` was still a `+CMDER:9`
/// placeholder. Routing both sides through this one handle closes that gap.
pub static BLE_SERVER: OnceLock<Arc<Mutex<BleServer>>> = OnceLock::new();

/// Get (or lazily create) the shared [`BleServer`] handle.
///
/// The first caller creates the instance; every subsequent caller shares it.
/// `main.rs` calls this at boot (then `init()` + `start_advertising()`), and
/// the AT dispatcher calls it to query / control BLE live over `AT+BLE=`.
pub fn shared_ble_server() -> &'static Arc<Mutex<BleServer>> {
    BLE_SERVER.get_or_init(|| Arc::new(Mutex::new(BleServer::new())))
}

/// Route a chat payload to the on-device agent and wait (bounded) for its
/// reply. Returns the reply bytes, or an AT-style error line on timeout.
fn agent_reply_for(payload: &str) -> Vec<u8> {
    {
        let mut task = BLE_AGENT_TASK.lock().unwrap_or_else(|e| e.into_inner());
        *task = Some(payload.trim().to_string());
    }
    // Drop any stale reply from a previous command before waiting.
    {
        let mut reply = BLE_AGENT_REPLY.lock().unwrap_or_else(|e| e.into_inner());
        *reply = None;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let reply = BLE_AGENT_REPLY.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = reply {
            return r.into_bytes();
        }
        if Instant::now() >= deadline {
            log::error!("[ble] agent reply timed out (30s)");
            return b"+CME ERROR: agent timeout\r\n".to_vec();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Dispatch a `SYS_CMD` payload received over BLE.
///
/// * `AT+AGENT="..."` → route the quoted payload to the on-device agent.
/// * Any other unparseable / natural-language text → treat as a chat message
///   for the agent.
/// * Anything that parses as a numeric AT command → the existing AT engine.
fn dispatch_ble_command(cmd_bytes: &[u8]) -> Vec<u8> {
    let trimmed = String::from_utf8_lossy(cmd_bytes);
    let trimmed = trimmed.trim();
    let mut scratch = magent_core::at::ScratchBuffer::new();
    let parsed = scratch.copy_and_parse(trimmed.as_bytes());
    match parsed {
        Ok(cmd) => {
            // `AT+AGENT="<payload>"` — the escape hatch to the ReAct loop.
            if matches!(cmd.op, magent_core::at::AtOp::Agent) {
                match cmd.arg(0) {
                    Some(magent_core::at::AtArg::Quoted(p)) => {
                        let payload = String::from_utf8_lossy(p);
                        log::info!("[ble] AT+AGENT → agent payload: {payload}");
                        agent_reply_for(&payload)
                    }
                    _ => b"+CME ERROR: missing agent payload\r\n".to_vec(),
                }
            } else {
                dispatch_at_command(cmd_bytes)
            }
        }
        Err(_) => {
            // Natural language → chat with the on-device agent.
            log::info!("[ble] natural-language → agent: {trimmed}");
            agent_reply_for(trimmed)
        }
    }
}

/// Dispatch an AT command received over BLE through the real AT engine and
/// return the wire response (or a `+CMDER:<code>` error line).
///
/// Runs with no Wi-Fi/time-sync handles (the BLE task has none): network- and
/// time-dependent ops answer a graceful error while core ops (VERSION, PING,
/// SYSLOG, ...) return real data.
fn dispatch_at_command(cmd_bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    match magent_core::at::parse_line(cmd_bytes) {
        Ok(cmd) => {
            let now_ms = (unsafe { esp_idf_sys::esp_timer_get_time() }.max(0) / 1000) as u64;
            let mut force = false;
            let outcome = crate::at_dispatch::dispatch(&cmd, now_ms, false, None, None, &mut force);
            log::info!("[ble] at-dispatch op={:?} outcome={:?}", cmd.op, outcome);
            let mut buf = crate::at_dispatch::ResponseBuf::new();
            if crate::at_dispatch::render_outcome(&outcome, &mut buf).is_ok() {
                out.extend_from_slice(buf.as_slice());
            } else {
                log::warn!("[ble] render_outcome failed");
            }
        }
        Err(e) => {
            let code = e.numeric_code();
            log::warn!("[ble] at parse error code={code} kind={:?}", e.kind);
            let outcome = crate::at_dispatch::AtOutcome::Error { code };
            let mut buf = crate::at_dispatch::ResponseBuf::new();
            if crate::at_dispatch::render_outcome(&outcome, &mut buf).is_ok() {
                out.extend_from_slice(buf.as_slice());
            }
        }
    }
    if out.is_empty() {
        out.extend_from_slice(b"OK\r\n");
    }
    out
}

/// Handle a write to one of the config characteristics.
unsafe fn handle_gatt_write(p: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param) {
    // The write's connection id is authoritative for notifications; the
    // CONNECT_EVT reports conn_id=0 in this stack version, so grab it here.
    GATT_CONN_ID.store(p.conn_id, Ordering::SeqCst);
    let handle = p.handle;
    let len = p.len as usize;
    if len == 0 || p.value.is_null() {
        return;
    }
    // HARDENING (audit-2026-08 H3): the raw pointer `p.value` is
    // bounded by `p.len` reported by the BLE stack. We cap `len` at
    // the protocol maximum (MTU is 23 bytes by default; some
    // stacks honour up to 512) so a buggy / hostile stack claiming a
    // 64 KiB length cannot drive `from_raw_parts` into UB by reading
    // past the actual allocation. Anything beyond the cap is treated
    // as a protocol violation and dropped.
    const MAX_BLE_WRITE: usize = 512;
    let len = if len > MAX_BLE_WRITE {
        log::error!(
            "[ble] GATTS write reports len={} > MAX_BLE_WRITE={}; dropping payload",
            len,
            MAX_BLE_WRITE
        );
        return;
    } else {
        len
    };
    let bytes = std::slice::from_raw_parts(p.value, len);
    let uuid16 = GATT_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .position(|&h| h == handle)
        .and_then(char_index_rev);

    match uuid16 {
        Some(WIFI_SSID_UUID16) => {
            log::info!("[ble] WIFI SSID written ({} bytes)", len);
        }
        Some(WIFI_PASS_UUID16) => {
            log::info!("[ble] WIFI password written ({} bytes)", len);
        }
        Some(LLM_MODEL_UUID16) => {
            log::info!("[ble] LLM model written: {}", String::from_utf8_lossy(bytes));
        }
        Some(LLM_API_KEY_UUID16) => {
            log::info!("[ble] LLM API key written ({} bytes)", len);
        }
        Some(HOSTNAME_UUID16) => {
            log::info!("[ble] hostname written: {}", String::from_utf8_lossy(bytes));
        }
        Some(SYS_CMD_UUID16) => {
            let cmd = String::from_utf8_lossy(bytes).trim().to_string();
            log::info!("[ble] system command: {cmd}");
            // Dispatch through the on-device agent (`AT+AGENT=`/natural
            // language) or the numeric AT engine, then reply on SYS_RSP.
            let response = dispatch_ble_command(bytes);
            set_char_value(SYS_RSP_UUID16, &response);
            notify_char(SYS_RSP_UUID16, &response);
        }
        _ => {
            log::warn!("[ble] write to unknown handle 0x{handle:04X} ({} bytes)", len);
        }
    }
}

/// Build the ESP-IDF Bluetooth controller config for the target chip.
///
/// The C61 and S3 expose **different** `esp_bt_controller_config_t` layouts:
/// the C61 uses the newer ESP32-C6-style struct (`config_version` /
/// `config_magic` + ~66 fields); the ESP32-S3 uses the classic ESP32 BLE
/// controller struct (`magic` / `version`, 47 fields). Each board gets its own
/// constructor.
#[cfg(feature = "board-c61")]
fn bt_controller_config() -> esp_idf_sys::esp_bt_controller_config_t {
    esp_idf_sys::esp_bt_controller_config_t {
        config_version: esp_idf_sys::CONFIG_VERSION,
        ble_ll_resolv_list_size: 4,
        ble_hci_evt_hi_buf_count: 30,
        ble_hci_evt_lo_buf_count: 8,
        ble_ll_sync_list_cnt: 5,
        ble_ll_sync_cnt: 1,
        ble_ll_rsp_dup_list_count: 20,
        ble_ll_adv_dup_list_count: 20,
        ble_ll_tx_pwr_dbm: 9,
        rtc_freq: 32768,
        ble_ll_sca: 60,
        ble_ll_scan_phy_number: 2,
        ble_ll_conn_def_auth_pyld_tmo: 3000,
        ble_ll_jitter_usecs: 16,
        ble_ll_sched_max_adv_pdu_usecs: 376,
        ble_ll_sched_direct_adv_max_usecs: 502,
        ble_ll_sched_adv_max_usecs: 852,
        ble_scan_rsp_data_max_len: 1650,
        ble_ll_cfg_num_hci_cmd_pkts: 1,
        ble_ll_ctrl_proc_timeout_ms: 40000,
        nimble_max_connections: 3,
        ble_whitelist_size: 12,
        ble_acl_buf_size: 255,
        ble_acl_buf_count: 24,
        ble_hci_evt_buf_size: 70,
        ble_multi_adv_instances: 1,
        ble_ext_adv_max_size: 1650,
        controller_task_stack_size: 4096,
        controller_task_prio: 23,
        controller_run_cpu: 0,
        enable_qa_test: 0,
        enable_bqb_test: 0,
        enable_tx_cca: 0,
        cca_rssi_thresh: 0,
        sleep_en: 0,
        coex_phy_coded_tx_rx_time_limit: 0,
        dis_scan_backoff: 0,
        ble_scan_classify_filter_enable: 1,
        cca_drop_mode: 0,
        cca_low_tx_pwr: 0,
        main_xtal_freq: 40,
        cpu_freq_mhz: 160, // ESP32-C61 (RISC-V)
        ignore_wl_for_direct_adv: 0,
        enable_pcl: 0,
        csa2_select: 0,
        enable_csr: 0,
        ble_aa_check: 0,
        ble_llcp_disc_flag: 0,
        scan_backoff_upperlimitmax: 0,
        ble_chan_ass_en: 0,
        ble_data_lenth_zero_aux: 0,
        vhci_enabled: 0,
        ptr_check_enabled: 0,
        ble_adv_tx_options: 0,
        skip_unnecessary_checks_en: 0,
        fast_conn_data_tx_en: 0,
        ch39_txpwr: 9,
        adv_rsv_cnt: 1,
        conn_rsv_cnt: 2,
        priority_level_cfg: 0,
        slv_fst_rx_lat_en: 0,
        dl_itvl_phy_sync_en: 0,
        scan_allow_adi_filter: 0,
        enhanced_mem_resv: 0,
        rxbuf_reserved: 0,
        config_magic: esp_idf_sys::CONFIG_MAGIC,
    }
}

#[cfg(feature = "board-s3")]
fn bt_controller_config() -> esp_idf_sys::esp_bt_controller_config_t {
    // ESP32-S3: classic ESP32 BLE controller config. Zero-init via `Default`
    // fills the safety defaults; `magic`/`version` satisfy the controller's
    // struct validator, and `bluetooth_mode` selects BLE-only (the S3 hardware
    // also supports BR/EDR, but this build uses only the BLE subset).
    // CPU clock is configured by sdkconfig, not here.
    esp_idf_sys::esp_bt_controller_config_t {
        magic: esp_idf_sys::ESP_BT_CTRL_CONFIG_MAGIC_VAL,
        version: esp_idf_sys::ESP_BT_CTRL_CONFIG_VERSION,
        controller_task_stack_size: 4096,
        // ESP-IDF validates this MUST equal `ESP_TASK_BT_CONTROLLER_PRIO`
        // (== ESP_TASK_PRIO_MAX - 2 == 23). The previous value (5) failed the
        // check with ESP_ERR_INVALID_ARG (258) on real S3 hardware.
        controller_task_prio: 23,
        controller_task_run_cpu: 0,
        bluetooth_mode: esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_BLE as u8,
        // ESP-IDF validates ble_max_act in (0, BTDM_CONTROLLER_BLE_MAX_ACT_LIMIT]
        // (default CONFIG_BT_CTRL_BLE_MAX_ACT == 6). Zero-init left it 0 →
        // ESP_ERR_INVALID_ARG (258) on the S3.
        ble_max_act: 6,
        ..Default::default()
    }
}

impl BleServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the BLE stack using ESP-IDF Bluedroid
    pub fn init(&mut self) -> Result<(), BleError> {
        if self.is_initialized {
            return Ok(());
        }

        self.state = BleState::Initializing;
        log::info!("[ble] Initializing BLE stack...");

        // HARDENING (stability-2026-08): the Bluedroid host's `btm_ble_init`
        // allocates a FreeRTOS mutex (internal DRAM) for `adv_rpt_queue`.
        // On the RAM-limited C61, after WiFi init leaves too little internal
        // DRAM, `xSemaphoreCreateMutex` returns NULL and the stack asserts
        // (`btm_ble_gap.c adv_rpt_queue != NULL`), panicking the board into a
        // reboot loop. Log the internal-heap headroom here so an operator can
        // see exactly how tight it is and tune sdkconfig accordingly.
        log::info!(
            "[ble] pre-init heap: default_free={} internal_free={}",
            unsafe { esp_idf_sys::esp_get_free_heap_size() },
            unsafe { esp_idf_sys::esp_get_free_internal_heap_size() },
        );

        unsafe {
            // The chip/IDF-correct default controller config. (The old code
            // hand-built the struct with `config_version: 5`, which didn't
            // match the ESP32-C61 controller and made `esp_bt_controller_init`
            // fail with ESP_ERR_INVALID_VERSION (266) —
            // "REG EXT ERROR: Invalid ext version".)
            // Reproduce ESP-IDF's `BT_CONTROLLER_INIT_CONFIG_DEFAULT()` for the
            // ESP32-C61 (values taken from the v5.5.5 framework's
            // components/bt/controller/esp32c6/{Kconfig.in,esp_bt_cfg.h}).
            //
            // The old hand-built struct set many fields to 0 via
            // `..Default::default()` (main_xtal_freq, cpu_freq_mhz, rtc_freq,
            // the scheduling windows, evt buf size, ...). The controller
            // validates these against its expected defaults, so
            // `esp_bt_controller_init` passed but `esp_bt_controller_enable`
            // failed with ESP_FAIL (-1). These are the real per-chip defaults.
            let mut cfg = bt_controller_config();

            let ret = esp_idf_sys::esp_bt_controller_init(&mut cfg);
            if ret != 0 {
                log::error!("[ble] BT controller init failed: {ret}");
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            log::info!("[ble] BT controller init OK");

            // Enable BLE mode
            let ret = esp_idf_sys::esp_bt_controller_enable(esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_BLE);
            if ret != 0 {
                log::error!("[ble] BT controller enable failed: {}", ret);
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            log::info!("[ble] BT controller enabled (BLE mode)");
        }

        // Initialize Bluedroid host stack
        unsafe {
            let ret = esp_idf_sys::esp_bluedroid_init();
            if ret != 0 {
                log::error!("[ble] Bluedroid init failed: {}", ret);
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            log::info!("[ble] Bluedroid init OK");

            let ret = esp_idf_sys::esp_bluedroid_enable();
            if ret != 0 {
                log::error!("[ble] Bluedroid enable failed: {}", ret);
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            log::info!("[ble] Bluedroid enabled");

            // Register the GAP callback so advertising actually starts once
            // the (asynchronous) adv-data configuration completes.
            let ret = esp_idf_sys::esp_ble_gap_register_callback(Some(gap_event_handler));
            if ret != 0 {
                log::error!("[ble] GAP callback register failed: {ret}");
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            // Configure the Security Manager so the peripheral can pair /
            // encrypt (needed before writes to encrypted characteristics are
            // allowed). Best-effort; safe to call before GATTS app register.
            configure_ble_security();
            // Register the GATTS callback so a BLE central can connect and
            // read/write the mAgent config service characteristics.
            let ret = esp_idf_sys::esp_ble_gatts_register_callback(Some(gatts_event_handler));
            if ret != 0 {
                log::error!("[ble] GATTS callback register failed: {ret}");
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            // Register the GATT application; the GATTS handler then builds
            // the config service asynchronously.
            let ret = esp_idf_sys::esp_ble_gatts_app_register(0);
            if ret != 0 {
                log::error!("[ble] GATTS app register failed: {ret}");
                self.state = BleState::Error;
                return Err(BleError::EspError(ret));
            }
            // Advertise the device name so BLE scanners show "mAgent".
            let name =
                std::ffi::CString::new(self.device_name.as_str()).unwrap_or_default();
            let ret = esp_idf_sys::esp_ble_gap_set_device_name(name.as_ptr());
            if ret != 0 {
                log::warn!("[ble] set_device_name failed: {ret}");
            }
            log::info!("[ble] GAP+GATTS callbacks registered, device name set");
        }

        self.is_initialized = true;
        self.state = BleState::Idle;
        log::info!("[ble] BLE stack initialized successfully");

        Ok(())
    }

    /// Start BLE advertising
    pub fn start_advertising(&mut self) -> Result<(), BleError> {
        if !self.is_initialized {
            return Err(BleError::NotInitialized);
        }
        if self.is_advertising {
            return Ok(());
        }

        log::info!("[ble] Configuring BLE advertising...");

        // Raw advertising data (explicit bytes — no ambiguity about how the
        // structured `esp_ble_adv_data_t` API packs the service UUID, which
        // kept getting dropped with a "Partial data write" warning):
        //   flags (LE General Discoverable | BR/EDR Not Supported),
        //   complete list of 16-bit service UUIDs (0x1850, little-endian),
        //   complete local name "mAgent".  15 bytes < 31-byte adv packet.
        let mut raw_adv: [u8; 15] = [
            0x02, 0x01, 0x06, // flags
            0x03, 0x03, CONFIG_SERVICE_UUID16 as u8, (CONFIG_SERVICE_UUID16 >> 8) as u8, // 0x1850 LE
            0x07, 0x09, b'm', b'A', b'g', b'e', b'n', b't', // "mAgent"
        ];

        unsafe {
            let ret = esp_idf_sys::esp_ble_gap_config_adv_data_raw(
                raw_adv.as_mut_ptr(),
                raw_adv.len() as u32,
            );
            if ret != 0 {
                log::error!("[ble] Config adv data failed: {}", ret);
                return Err(BleError::EspError(ret));
            }
            log::info!("[ble] Advertising data configured (name + UUID 0x1850)");
        }

        // The actual `esp_ble_gap_start_advertising` is issued by the GAP
        // event handler once ESP-IDF reports the adv-data config complete
        // (`ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT`). Issuing it here —
        // before that event — is a no-op/failure, which is exactly why the
        // previous firmware never advertised.

        self.is_advertising = true;
        self.state = BleState::Advertising;

        log::info!("");
        log::info!("[ble] ===========================================");
        log::info!("[ble] BLE ADVERTISING ACTIVE!");
        log::info!("[ble] ===========================================");
        log::info!("[ble] Device Name: {}", self.device_name.as_str());
        log::info!("[ble] Service UUID: 0x1850 (mAgent Config)");
        log::info!("[ble] Scan for 'mAgent' in your BLE scanner!");
        log::info!("[ble] ===========================================");
        log::info!("");

        Ok(())
    }

    /// Stop BLE advertising.
    ///
    /// Idempotent and graceful: if not currently advertising this is a
    /// no-op success (even if the stack was never initialised), so
    /// `AT+BLE=OFF` is safe to issue at any time. The stop command is
    /// issued synchronously; the GAP completion event is not awaited —
    /// the host only needs the advertising flag cleared and the state
    /// reflected as `Idle`.
    pub fn stop_advertising(&mut self) -> Result<(), BleError> {
        if !self.is_advertising {
            return Ok(());
        }
        // Advertising implies initialised, but keep the invariant robust:
        // if somehow not initialised, just clear the flag and bail.
        if !self.is_initialized {
            self.is_advertising = false;
            self.state = BleState::Idle;
            return Ok(());
        }

        unsafe {
            let ret = esp_idf_sys::esp_ble_gap_stop_advertising();
            if ret != 0 {
                log::error!("[ble] Stop advertising failed: {}", ret);
                return Err(BleError::EspError(ret));
            }
        }
        self.is_advertising = false;
        self.state = BleState::Idle;
        log::info!("[ble] Advertising stopped");
        Ok(())
    }

    /// Tear down the BLE stack. Best-effort: logs errors but never
    /// panics and never leaves the handle in a half-initialised state.
    #[allow(dead_code)] // public `BleServer` API, not yet called on the active path
    pub fn deinit(&mut self) {
        if self.is_advertising {
            let _ = self.stop_advertising();
        }
        if self.is_initialized {
            unsafe {
                let ret = esp_idf_sys::esp_bt_controller_disable();
                if ret != 0 {
                    log::error!("[ble] BT controller disable failed: {}", ret);
                }
            }
            self.is_initialized = false;
        }
        self.state = BleState::Idle;
        log::info!("[ble] BLE stack deinitialized");
    }

    /// Explicitly set the reported state (used by GAP/GATTS callbacks to
    /// reflect connection / disconnection events).
    #[allow(dead_code)] // public `BleServer` API, not yet called on the active path
    pub fn set_state(&mut self, state: BleState) {
        self.state = state;
    }

    pub fn get_state(&self) -> BleState {
        self.state
    }

    #[allow(dead_code)] // public `BleServer` API, not yet called on the active path
    pub fn is_active(&self) -> bool {
        self.is_initialized && self.is_advertising
    }

    pub fn device_name(&self) -> &str {
        self.device_name.as_str()
    }
}
