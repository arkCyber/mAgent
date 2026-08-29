//! `AT+PING` — ICMP echo to an IPv4 host via ESP-IDF's `esp_ping` (lwip).
//!
//! esp_ping is asynchronous: it runs on its own task and invokes callbacks on
//! the internal ping thread. We bridge that into the synchronous AT dispatch
//! path with a small set of `extern "C"` callbacks that write into `static`
//! atomics, then busy-wait (with a bounded timeout) for the "end" flag.
//!
//! The firmware targets run in safe mode right now (BLE/wifi memory trade-offs),
//! but ping itself needs no BLE and only needs the network up to reach the
//! target — so this works once Wi-Fi is associated.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use std::time::Duration;

/// Set true when the ping session ends (all probes done or stopped).
static PING_DONE: AtomicBool = AtomicBool::new(false);
/// Number of ICMP echo replies received.
static PING_REPLY: AtomicU32 = AtomicU32::new(0);
/// Round-trip time of the last reply, in microseconds.
static PING_TIME_US: AtomicU32 = AtomicU32::new(0);

/// Milliseconds since boot (for the bounded wait loop).
fn now_ms() -> u64 {
    unsafe { esp_idf_sys::esp_timer_get_time().max(0) as u64 / 1000 }
}

/// `on_ping_success` — a reply arrived; record it and its RTT.
unsafe extern "C" fn on_ping_success(hdl: esp_idf_sys::esp_ping_handle_t, _args: *mut core::ffi::c_void) {
    PING_REPLY.fetch_add(1, Ordering::SeqCst);
    let mut gap: u32 = 0;
    // SAFETY: gap is a valid u32 buffer for the call.
    let rc = unsafe {
        esp_idf_sys::esp_ping_get_profile(
            hdl,
            esp_idf_sys::esp_ping_profile_t_ESP_PING_PROF_TIMEGAP,
            &mut gap as *mut u32 as *mut core::ffi::c_void,
            4,
        )
    };
    if rc == esp_idf_sys::ESP_OK {
        PING_TIME_US.store(gap, Ordering::SeqCst);
    }
}

/// `on_ping_timeout` — a probe timed out (no reply this round).
unsafe extern "C" fn on_ping_timeout(
    _hdl: esp_idf_sys::esp_ping_handle_t,
    _args: *mut core::ffi::c_void,
) {
    // Nothing to do; the end callback still fires.
}

/// `on_ping_end` — session finished; release the AT wait loop.
unsafe extern "C" fn on_ping_end(
    _hdl: esp_idf_sys::esp_ping_handle_t,
    _args: *mut core::ffi::c_void,
) {
    PING_DONE.store(true, Ordering::SeqCst);
}

/// Resolve `host` to an IPv4 address (network byte order) via DNS
/// (`lwip_getaddrinfo`). Returns `None` on any failure (unknown host / no IPv4).
fn resolve_ipv4(host: &str) -> Option<u32> {
    let cstr = std::ffi::CString::new(host).ok()?;
    let mut res: *mut esp_idf_sys::addrinfo = core::ptr::null_mut();
    // SAFETY: `nodename` is valid for the call; hints is null (any); `res` is
    // written by the callee and freed below.
    let rc = unsafe {
        esp_idf_sys::lwip_getaddrinfo(
            cstr.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            &mut res,
        )
    };
    if rc != 0 || res.is_null() {
        return None;
    }
    let ai = unsafe { &*res };
    let addr = if ai.ai_addr.is_null() {
        None
    } else {
        // `ai_addr` is a generic `sockaddr`; for AF_INET it is actually
        // `sockaddr_in` (larger). Reading the IPv4 address is well-defined.
        // SAFETY: the resolved address is a `sockaddr_in` for IPv4.
        let sa = unsafe { &*(ai.ai_addr as *const esp_idf_sys::sockaddr_in) };
        Some(sa.sin_addr.s_addr)
    };
    // SAFETY: `res` was allocated by `lwip_getaddrinfo` above.
    unsafe { esp_idf_sys::lwip_freeaddrinfo(res) };
    addr
}

/// Resolve an IPv6 literal (e.g. `::1`, `fe80::1`) to an `ip6_addr` via
/// `ip6addr_aton`. Returns `None` on parse failure.
fn resolve_ip6(host: &str) -> Option<esp_idf_sys::ip6_addr> {
    let cstr = std::ffi::CString::new(host).ok()?;
    let mut addr: esp_idf_sys::ip6_addr = unsafe { std::mem::zeroed() };
    // SAFETY: `cstr` is valid; `addr` is a valid `ip6_addr` buffer for the call.
    let rc = unsafe { esp_idf_sys::ip6addr_aton(cstr.as_ptr(), &mut addr) };
    if rc == 0 {
        return None;
    }
    Some(addr)
}

/// Ping `host` — an IPv4 or IPv6 literal, or a hostname (DNS → IPv4) — and
/// return a `+PING:` line, or `Err(code)` where `code` is a `+CMDER` numeric.
pub fn ping_ipv4(host: &str, now_ms_now: u64) -> Result<String, u8> {
    let Ok(cstr) = std::ffi::CString::new(host) else {
        return Err(4);
    };
    // An IPv6 literal contains ':' (e.g. `::1`); otherwise resolve as IPv4.
    let target: esp_idf_sys::ip_addr = if host.contains(':') {
        let ip6 = resolve_ip6(host).ok_or(4)?;
        esp_idf_sys::ip_addr {
            type_: esp_idf_sys::lwip_ip_addr_type_IPADDR_TYPE_V6 as u8,
            u_addr: esp_idf_sys::ip_addr__bindgen_ty_1 { ip6 },
        }
    } else {
        // IPv4: try a literal first, then DNS.
        let mut addr = unsafe { esp_idf_sys::ipaddr_addr(cstr.as_ptr()) };
        if addr == 0 {
            match resolve_ipv4(host) {
                Some(a) => addr = a,
                None => return Err(4),
            }
        }
        esp_idf_sys::ip_addr {
            type_: esp_idf_sys::lwip_ip_addr_type_IPADDR_TYPE_V4 as u8,
            u_addr: esp_idf_sys::ip_addr__bindgen_ty_1 {
                ip4: esp_idf_sys::ip4_addr { addr },
            },
        }
    };

    let mut cfg = esp_idf_sys::esp_ping_config_t::default();
    cfg.count = 4;
    cfg.interval_ms = 1000;
    cfg.timeout_ms = 2000;
    cfg.target_addr = target;

    let callbacks = esp_idf_sys::esp_ping_callbacks_t {
        cb_args: core::ptr::null_mut(),
        on_ping_success: Some(on_ping_success),
        on_ping_timeout: Some(on_ping_timeout),
        on_ping_end: Some(on_ping_end),
    };

    PING_DONE.store(false, Ordering::SeqCst);
    PING_REPLY.store(0, Ordering::SeqCst);
    PING_TIME_US.store(0, Ordering::SeqCst);

    let mut handle: esp_idf_sys::esp_ping_handle_t = core::ptr::null_mut();
    // SAFETY: cfg/callbacks are valid; handle is written by the callee.
    let rc = unsafe { esp_idf_sys::esp_ping_new_session(&cfg, &callbacks, &mut handle) };
    if rc != esp_idf_sys::ESP_OK {
        return Err(6);
    }
    // SAFETY: handle is live from new_session.
    let rc = unsafe { esp_idf_sys::esp_ping_start(handle) };
    if rc != esp_idf_sys::ESP_OK {
        // SAFETY: abort a live session.
        let _ = unsafe { esp_idf_sys::esp_ping_delete_session(handle) };
        return Err(6);
    }

    // Bounded wait for the session end (4 probes * ~3 s max).
    let deadline = now_ms_now + 10_000;
    while !PING_DONE.load(Ordering::SeqCst) && now_ms() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let reply = PING_REPLY.load(Ordering::SeqCst);
    let gap_us = PING_TIME_US.load(Ordering::SeqCst);

    // SAFETY: handle is still live.
    let _ = unsafe { esp_idf_sys::esp_ping_stop(handle) };
    let _ = unsafe { esp_idf_sys::esp_ping_delete_session(handle) };

    if reply == 0 {
        return Err(6); // no reply (unreachable / timeout)
    }
    Ok(format!("+PING: reply={reply} rtt={gap_us}us"))
}
