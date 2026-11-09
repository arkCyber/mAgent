//! `AT+PING` — ICMP echo to an IPv4 or IPv6 host via ESP-IDF's `esp_ping`
//! (lwip).
//!
//! IPv6 literals are supported only when the build enables lwIP IPv6
//! (`CONFIG_LWIP_IPV6=y`); on an IPv6-less build they answer `+CMDER:9` rather
//! than being mis-parsed as IPv4. See the `ipv6_ping` cfg in `build.rs`.
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
/// Round-trip time of the last reply, in **milliseconds**.
///
/// ESP-IDF's `ESP_PING_PROF_TIMEGAP` writes `elapsed_time_ms`
/// (`ping_sock.c`), so the old `us` label reported 25 ms as "25us".
static PING_TIME_MS: AtomicU32 = AtomicU32::new(0);

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
        PING_TIME_MS.store(gap, Ordering::SeqCst);
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

    // Ask for IPv4 explicitly. A null `hints` means `AF_UNSPEC`, and lwIP then
    // resolves with `NETCONN_DNS_IPV4_IPV6` and returns an entry per address,
    // in resolver order — for an IPv6-only name the first (only) entry is
    // `AF_INET6`, pointing at a `sockaddr_in6`. Reading that as a
    // `sockaddr_in` lands on the wrong field: `sin_addr` sits at offset 4,
    // which in `sockaddr_in6` is `sin6_flowinfo` — a field lwIP's
    // `create_addrinfo` memsets to zero and never fills. The resolved address
    // would come back as 0.0.0.0, so `AT+PING` would silently target the wrong
    // host instead of reporting that it could not resolve one.
    // (The hint also makes the cast below unconditionally correct: lwIP only
    // ever returns `AF_INET` entries for `ai_family = AF_INET`.)
    let hints = esp_idf_sys::addrinfo {
        ai_family: esp_idf_sys::AF_INET as core::ffi::c_int,
        ..Default::default()
    };
    let mut res: *mut esp_idf_sys::addrinfo = core::ptr::null_mut();
    // SAFETY: `nodename` is valid for the call; `hints` is a zeroed struct with
    // only `ai_family` set; `res` is written by the callee and freed below.
    let rc = unsafe {
        esp_idf_sys::lwip_getaddrinfo(cstr.as_ptr(), core::ptr::null(), &hints, &mut res)
    };
    if rc != 0 || res.is_null() {
        return None;
    }
    let ai = unsafe { &*res };
    let addr = if ai.ai_addr.is_null() {
        None
    } else {
        // SAFETY: with the `AF_INET` hint above, `ai_addr` points at a
        // `sockaddr_in`.
        let sa = unsafe { &*(ai.ai_addr as *const esp_idf_sys::sockaddr_in) };
        Some(sa.sin_addr.s_addr)
    };
    // SAFETY: `res` was allocated by `lwip_getaddrinfo` above.
    unsafe { esp_idf_sys::lwip_freeaddrinfo(res) };
    // 0.0.0.0 is not a destination. lwIP hands one back when a name resolves to
    // no non-zero address (see the `ipaddr_cnt == 0` branch of
    // `lwip_getaddrinfo`), which is also what a DNS server answering with
    // 0.0.0.0 — the usual response from a blocklist — produces. Treat it as "no
    // IPv4 address" so the caller reports an unresolvable host rather than
    // pinging the wildcard address.
    match addr {
        Some(0) | None => None,
        other => other,
    }
}

/// Resolve an IPv6 literal (e.g. `::1`, `fe80::1`) to an `ip6_addr` via
/// `ip6addr_aton`. Returns `None` on parse failure.
///
/// Only present when the build enables lwIP IPv6 (see `build.rs`'s
/// `ipv6_ping` cfg). With `CONFIG_LWIP_IPV6=n` bindgen emits neither
/// `ip6_addr` nor `ip6addr_aton`, so this function cannot exist at all.
#[cfg(ipv6_ping)]
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
    //
    // `target_addr` is `ip_addr_t` in both configurations, but what that typedef
    // *is* depends on lwIP IPv6: a tagged union (`ip_addr`) when it is on, and a
    // bare `ip4_addr` when it is off. The cfg keeps each shape sound.
    let target = if host.contains(':') {
        #[cfg(not(ipv6_ping))]
        {
            // No IPv6 in this build. Refuse rather than hand the literal to the
            // IPv4 parser, which would mis-read it as a malformed address.
            log::warn!("[ping] IPv6 literal '{host}' unsupported: CONFIG_LWIP_IPV6=n");
            return Err(9);
        }
        #[cfg(ipv6_ping)]
        {
            let ip6 = resolve_ip6(host).ok_or(4)?;
            esp_idf_sys::ip_addr {
                type_: esp_idf_sys::lwip_ip_addr_type_IPADDR_TYPE_V6 as u8,
                u_addr: esp_idf_sys::ip_addr__bindgen_ty_1 { ip6 },
            }
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
        #[cfg(ipv6_ping)]
        {
            esp_idf_sys::ip_addr {
                type_: esp_idf_sys::lwip_ip_addr_type_IPADDR_TYPE_V4 as u8,
                u_addr: esp_idf_sys::ip_addr__bindgen_ty_1 {
                    ip4: esp_idf_sys::ip4_addr { addr },
                },
            }
        }
        #[cfg(not(ipv6_ping))]
        {
            esp_idf_sys::ip4_addr { addr }
        }
    };

    let mut cfg = esp_idf_sys::esp_ping_config_t::default();
    // PATCHED (MicroAgent): the bindgen `Default` for `esp_ping_config_t` is a
    // **zeroed** struct, while the C ``ESP_PING_DEFAULT_CONFIG()`` macro also
    // fills in `data_size` / `ttl` / `task_stack_size` / `task_prio`. With
    // `task_stack_size == 0` `esp_ping_new_session` calls
    // `xTaskCreate(..., 0, ..., 0, ...)`, which always fails
    // (`E ping_sock: esp_ping_new_session(228): create ping task failed`) and
    // made *every* `AT+PING` answer `+CMDER:6` regardless of free heap
    // (reproduced with 125 KB internal DRAM free). Mirror the IDF v5.5 macro
    // (components/lwip/include/apps/ping/ping_sock.h) explicitly.
    cfg.count = 4;
    cfg.interval_ms = 1000;
    cfg.timeout_ms = 2000;
    cfg.data_size = 64;
    cfg.ttl = 64; // IP_DEFAULT_TTL
    // IDF's `ESP_TASK_PING_STACK` is `2048 + TASK_EXTRA_STACK_SIZE`; round up so
    // a future `TASK_EXTRA_STACK_SIZE` bump cannot under-size the task.
    cfg.task_stack_size = 4096;
    cfg.task_prio = 2; // ESP_PING_DEFAULT_CONFIG()->task_prio
    cfg.target_addr = target;

    let callbacks = esp_idf_sys::esp_ping_callbacks_t {
        cb_args: core::ptr::null_mut(),
        on_ping_success: Some(on_ping_success),
        on_ping_timeout: Some(on_ping_timeout),
        on_ping_end: Some(on_ping_end),
    };

    PING_DONE.store(false, Ordering::SeqCst);
    PING_REPLY.store(0, Ordering::SeqCst);
    PING_TIME_MS.store(0, Ordering::SeqCst);

    let mut handle: esp_idf_sys::esp_ping_handle_t = core::ptr::null_mut();
    // SAFETY: cfg/callbacks are valid; handle is written by the callee.
    let rc = unsafe { esp_idf_sys::esp_ping_new_session(&cfg, &callbacks, &mut handle) };
    if rc != esp_idf_sys::ESP_OK {
        // PATCHED (MicroAgent): surface the underlying reason. Previously every
        // failure collapsed into a bare `+CMDER:6`, which made "ping task could
        // not be created (bad config / no memory)" indistinguishable from "the
        // host did not answer" — a multi-hour debugging detour.
        log::warn!("[ping] esp_ping_new_session failed: rc={rc}");
        return Err(6);
    }
    // SAFETY: handle is live from new_session.
    let rc = unsafe { esp_idf_sys::esp_ping_start(handle) };
    if rc != esp_idf_sys::ESP_OK {
        log::warn!("[ping] esp_ping_start failed: rc={rc}");
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
    let gap_ms = PING_TIME_MS.load(Ordering::SeqCst);
    log::info!(
        "[ping] {host}: reply={reply} rtt={gap_ms}ms done={}",
        PING_DONE.load(Ordering::SeqCst)
    );

    // SAFETY: handle is still live.
    let _ = unsafe { esp_idf_sys::esp_ping_stop(handle) };
    let _ = unsafe { esp_idf_sys::esp_ping_delete_session(handle) };

    if reply == 0 {
        return Err(6); // no reply (unreachable / timeout)
    }
    Ok(format!("+PING: reply={reply} rtt={gap_ms}ms"))
}
