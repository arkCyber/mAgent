//! Pure-logic decision helpers for AT command validation.
//!
//! These functions are I/O-free and deterministic: they take
//! already-parsed [`AtCommand`] inputs and either return a
//! `Validated*` payload or an `AtOutcome::error(N)` describing
//! why the input was rejected. The firmware-side dispatcher
//! (`firmware/esp32-app/src/at_dispatch.rs`) wraps each helper
//! with the actual NVS / Wi-Fi / sealing glue so the security-
//! sensitive validation rules (length caps, NUL rejection,
//! encoding checks, mode restrictions) can be exercised on the
//! host with hundreds of unit tests against malicious /
//! pathological inputs.
//!
//! # Why split out validation?
//!
//! The validation rules ARE the security boundary of the AT
//! interface — they decide which inputs are accepted, and getting
//! them wrong is how an attacker turns a configuration command
//! into a privilege escalation (e.g. by injecting a 1 KB SSID
//! that overflows a stack buffer, or by passing a NUL byte that
//! truncates a Wi-Fi password and silently turns the network
//! open).
//!
//! Keeping the rules in `magent-core` (host-testable) means we
//! can:
//!   1. Cover every rejection path with a unit test, including
//!      boundary conditions that are tedious to set up on hardware.
//!   2. Audit the rules independently of the firmware glue.
//!   3. Re-use the same rules across multiple firmware targets
//!      (ESP32 today, nRF52 tomorrow) without duplicating logic.

// Like `at.rs`, these validators deliberately return the large `AtOutcome`
// error type on the `Err` path (it carries a formatted message); suppress
// the size lint at the crate level so the code stays readable.
#![allow(clippy::result_large_err)]

use crate::at::{AtArg, AtCommand};
use crate::at_dispatch_outcome::AtOutcome;
use crate::time_sync::{TZ_MAX_MINUTES, TZ_MIN_MINUTES};

// ---------------------------------------------------------------------------
// Length / size caps
// ---------------------------------------------------------------------------

/// Maximum SSID length accepted by AT+CWJAP=.
pub const CWJAP_SSID_MAX: usize = 32;

/// Maximum password length accepted by AT+CWJAP=.
pub const CWJAP_PASS_MAX: usize = 64;

/// Maximum hostname length accepted by AT+CWHOSTNAME=.
pub const HOSTNAME_MAX: usize = 32;

/// Mode values accepted by AT+CWMODE=.
pub const CWMODE_VALID: &[u8] = &[1, 2, 3];

/// Maximum model name length accepted by AT+LLMCFG= (matches the
/// firmware-side cap in `llmcfg_dispatch`).
pub const LLM_MODEL_MAX: usize = 64;

/// Maximum API-key length accepted by AT+LLMCFG= (matches the firmware-side
/// cap in `llmcfg_dispatch`).
pub const LLM_API_KEY_MAX: usize = 128;

/// Maximum provider-name length accepted by `AT+FLIGHTCFG=`
/// (`travelpayouts` / `kiwi` today; the cap leaves room for future providers).
pub const FLIGHT_PROVIDER_MAX: usize = 16;

/// Maximum provider token length accepted by `AT+FLIGHTCFG=` (travelpayouts
/// tokens are ~32 hex chars; Kiwi Tequila keys are 32).
pub const FLIGHT_TOKEN_MAX: usize = 128;

/// Maximum URL length accepted by AT+HTTPGET=.
pub const HTTPGET_URL_MAX: usize = 512;

// ---------------------------------------------------------------------------
// Validated payloads
// ---------------------------------------------------------------------------

/// Result of a successful `AT+CWJAP=` validation. Both fields are
/// owned heapless strings so the payload can outlive the source
/// `AtCommand` (caller drops the original args buffer).
#[derive(Debug, Clone)]
pub struct CwjapValidated {
    /// The validated SSID (non-empty, ≤ `CWJAP_SSID_MAX` bytes).
    pub ssid: heapless::String<CWJAP_SSID_MAX>,
    /// The validated WPA password (≤ `CWJAP_PASS_MAX` bytes).
    pub password: heapless::String<CWJAP_PASS_MAX>,
}

/// Result of a successful `AT+CWHOSTNAME=` validation.
#[derive(Debug, Clone)]
pub struct HostnameValidated {
    /// The validated hostname (RFC-1123, ≤ `HOSTNAME_MAX` bytes).
    pub hostname: heapless::String<HOSTNAME_MAX>,
}

/// Result of a successful `AT+CWMODE=` validation. `mode` is
/// guaranteed to be one of `CWMODE_VALID`.
#[derive(Debug, Clone, Copy)]
pub struct CwmodeValidated {
    /// The validated Wi-Fi mode (one of `CWMODE_VALID`).
    pub mode: u8,
}

/// Result of a successful `AT+TIMEZONE=` validation. `offset_minutes`
/// is guaranteed to be in `TZ_MIN_MINUTES..=TZ_MAX_MINUTES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimezoneValidated {
    /// Offset in minutes east of UTC. Negative for west.
    pub offset_minutes: i16,
}

// ---------------------------------------------------------------------------
// Pure-logic decision helpers
// ---------------------------------------------------------------------------

/// Extract a byte slice from a Token/Quoted argument, rejecting
/// Named-key arguments (`key=val`) which are not legal for the
/// AT commands we validate here. Returns `None` if the argument
/// is Named — caller decides the error code.
/// Extract the payload bytes of an argument into `out`. Quoted
/// arguments are ESP-AT-unescaped (so `\,`→`,`, …); unquoted tokens
/// contain no escapes and are copied verbatim. The returned slice
/// always borrows `out`, so both cases share one return lifetime and
/// the caller never holds a borrow into the argument storage.
///
/// Returns `Err(())` if the argument is a `key=val` (never legal here)
/// or if the decoded/copied bytes overflow `out` (caller maps that to
/// the "too long" rejection).
fn arg_bytes_decoded<'a, const N: usize>(
    arg: &AtArg<'_>,
    out: &'a mut heapless::Vec<u8, N>,
) -> Result<&'a [u8], ()> {
    match arg {
        AtArg::Quoted(s) => {
            crate::at::unescape_quoted(s, out)?;
            Ok(out.as_slice())
        }
        AtArg::Token(s) => {
            out.clear();
            out.extend_from_slice(s).map_err(|_| ())?;
            Ok(out.as_slice())
        }
        AtArg::Named { .. } => Err(()),
    }
}

/// Validate the inputs of an `AT+CWJAP=` SET request.
///
/// # Validation rules
/// 1. SSID length in `1..=CWJAP_SSID_MAX` (after unescaping).
/// 2. Password length in `0..=CWJAP_PASS_MAX` (empty password is
///    valid — it indicates an OPEN network).
/// 3. No NUL bytes in either field.
/// 4. Both fields are valid UTF-8.
///
/// Quoted arguments are ESP-AT-unescaped before validation so a real
/// SSID/passphrase containing `,` / `"` / `\` round-trips correctly
/// (e.g. `AT+CWJAP="my\,ssid","p\"w"` → `my,ssid` / `p"w`).
pub fn validate_cwjap_set(cmd: &AtCommand<'_>) -> Result<CwjapValidated, AtOutcome> {
    // Two independent stack buffers so the SSID and password decoded
    // slices can coexist (each borrows its own buffer).
    let mut ssid_dec: heapless::Vec<u8, CWJAP_SSID_MAX> = heapless::Vec::new();
    let mut pass_dec: heapless::Vec<u8, CWJAP_PASS_MAX> = heapless::Vec::new();

    // SSID (required).
    let ssid_arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let ssid_src = arg_bytes_decoded(ssid_arg, &mut ssid_dec).map_err(|_| AtOutcome::error(8))?;
    if ssid_src.is_empty() || ssid_src.len() > CWJAP_SSID_MAX {
        return Err(AtOutcome::error(8));
    }
    if ssid_src.contains(&0) {
        return Err(AtOutcome::error(4));
    }
    let ssid_str = core::str::from_utf8(ssid_src).map_err(|_| AtOutcome::error(4))?;

    // Password: optional (empty => OPEN network).
    let pass_src_opt: Option<&[u8]> = match cmd.args.get(1) {
        Some(arg) => Some(arg_bytes_decoded(arg, &mut pass_dec).map_err(|_| AtOutcome::error(8))?),
        None => None,
    };
    if let Some(src) = pass_src_opt {
        if src.len() > CWJAP_PASS_MAX {
            return Err(AtOutcome::error(8));
        }
        if src.contains(&0) {
            return Err(AtOutcome::error(4));
        }
        if !src.is_empty() {
            core::str::from_utf8(src).map_err(|_| AtOutcome::error(4))?;
        }
    }

    // Copy into the owned payloads. Length / NUL / UTF-8 are already
    // verified above, so these `push_str` calls cannot fail.
    let mut ssid: heapless::String<CWJAP_SSID_MAX> = heapless::String::new();
    ssid.push_str(ssid_str).map_err(|_| AtOutcome::error(8))?;
    let mut password: heapless::String<CWJAP_PASS_MAX> = heapless::String::new();
    if let Some(src) = pass_src_opt {
        if !src.is_empty() {
            let s = core::str::from_utf8(src).map_err(|_| AtOutcome::error(4))?;
            password.push_str(s).map_err(|_| AtOutcome::error(8))?;
        }
    }

    Ok(CwjapValidated { ssid, password })
}

/// Validate the inputs of an `AT+CWHOSTNAME=` SET request.
///
/// Quoted hostnames are ESP-AT-unescaped before validation.
pub fn validate_cwhostname_set(cmd: &AtCommand<'_>) -> Result<HostnameValidated, AtOutcome> {
    let arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let mut decoded: heapless::Vec<u8, HOSTNAME_MAX> = heapless::Vec::new();
    let src = arg_bytes_decoded(arg, &mut decoded).map_err(|_| AtOutcome::error(8))?;
    if src.is_empty() {
        return Err(AtOutcome::error(4));
    }
    if src.len() > HOSTNAME_MAX {
        return Err(AtOutcome::error(8));
    }
    if src.contains(&0) {
        return Err(AtOutcome::error(4));
    }
    let hostname_str = core::str::from_utf8(src).map_err(|_| AtOutcome::error(4))?;
    let mut hostname: heapless::String<HOSTNAME_MAX> = heapless::String::new();
    hostname
        .push_str(hostname_str)
        .map_err(|_| AtOutcome::error(8))?;
    Ok(HostnameValidated { hostname })
}

/// Validate the inputs of an `AT+CWMODE=` SET request.
pub fn validate_cwmode_set(cmd: &AtCommand<'_>) -> Result<CwmodeValidated, AtOutcome> {
    let arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let token = match arg {
        AtArg::Token(s) => s,
        _ => return Err(AtOutcome::error(4)),
    };
    if token.is_empty() {
        return Err(AtOutcome::error(7));
    }
    let mut acc: u32 = 0;
    for &c in token.iter() {
        if !c.is_ascii_digit() {
            return Err(AtOutcome::error(7));
        }
        acc = acc * 10 + (c - b'0') as u32;
        if acc > u8::MAX as u32 {
            return Err(AtOutcome::error(7));
        }
    }
    let n = acc as u8;
    if !CWMODE_VALID.contains(&n) {
        return Err(AtOutcome::error(7));
    }
    Ok(CwmodeValidated { mode: n })
}

/// Validate the inputs of an `AT+TIMEZONE=` SET request.
///
/// Accepts a signed decimal integer in `[TZ_MIN_MINUTES, TZ_MAX_MINUTES]`
/// (the same band the `TimeSync::set_tz_offset_minutes` accepts). The
/// payload is in minutes east of UTC; negative values are written as
/// `AT+TIMEZONE=-300` (5 hours west).
pub fn validate_timezone_set(cmd: &AtCommand<'_>) -> Result<TimezoneValidated, AtOutcome> {
    let arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let token = match arg {
        AtArg::Token(s) => s,
        _ => return Err(AtOutcome::error(4)),
    };
    if token.is_empty() {
        return Err(AtOutcome::error(7));
    }
    // Reject anything outside the i32 ASCII range; i64 is overkill for
    // a timezone offset.
    if token.len() > 11 {
        return Err(AtOutcome::error(7));
    }
    // Parse signed. Allow leading `-` only; reject `+` because AT
    // numeric args are unsigned by convention (the sign is the only
    // exception we make for timezone).
    let signed: i32;
    let rest: &[u8];
    if token.first() == Some(&b'-') {
        signed = -1;
        rest = &token[1..];
    } else {
        signed = 1;
        rest = token;
    }
    if rest.is_empty() {
        return Err(AtOutcome::error(7));
    }
    let mut acc: i64 = 0;
    for &c in rest {
        if !c.is_ascii_digit() {
            return Err(AtOutcome::error(7));
        }
        acc = acc * 10 + (c - b'0') as i64;
        if acc > i32::MAX as i64 {
            return Err(AtOutcome::error(7));
        }
    }
    let magnitude = acc as i32;
    let offset = magnitude * signed;
    if !(TZ_MIN_MINUTES as i32..=TZ_MAX_MINUTES as i32).contains(&offset) {
        return Err(AtOutcome::error(7));
    }
    Ok(TimezoneValidated {
        offset_minutes: offset as i16,
    })
}

// ---------------------------------------------------------------------------
// BLE peripheral control (AT+BLE=<ON|OFF|STATE>)
// ---------------------------------------------------------------------------

/// Valid actions accepted by `AT+BLE=<...>` (case-insensitive on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleAction {
    /// `AT+BLE=ON` — initialise (if needed) and start advertising.
    Start,
    /// `AT+BLE=OFF` — stop advertising (idempotent).
    Stop,
    /// `AT+BLE=STATE` — report the current advertising / connection state.
    State,
}

/// Result of a successful `AT+BLE=` validation. The action is guaranteed
/// to be one of the three supported verbs above; the firmware-side
/// dispatcher still owns the actual `BleServer` handle and performs the
/// operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BleValidated {
    /// The validated BLE control verb from `AT+BLE=<...>`.
    pub action: BleAction,
}

/// Validate the argument of an `AT+BLE=<ON|OFF|STATE>` SET request.
///
/// # Validation rules
/// 1. Exactly one positional argument is required (`AT+BLE=` alone is
///    malformed → error 4).
/// 2. The argument must be an unquoted `Token`. Quoted (`"ON"`) and
///    `key=val` forms are rejected (error 4) so an attacker cannot slip
///    quotes or extra syntax past the validator — mirrors the strictness
///    of `validate_timezone_set`.
/// 3. The token must be `ON`, `OFF`, or `STATE` case-insensitively
///    (ASCII). Anything else (numeric, empty, unknown verb, oversized)
///    is rejected as an invalid value (error 7).
///
/// Extra arguments beyond the first are silently ignored, matching the
/// other validators (the parser caps the arg count anyway).
///
/// This is the pure decision helper for [`crate::at::AtOp::Ble`]; the
/// firmware-side dispatcher (`firmware/esp32-app/src/at_dispatch.rs`)
/// calls it before touching the `BleServer` handle.
pub fn validate_ble_set(cmd: &AtCommand<'_>) -> Result<BleValidated, AtOutcome> {
    let arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let token = match arg {
        AtArg::Token(s) => s,
        _ => return Err(AtOutcome::error(4)),
    };

    /// Case-insensitive (ASCII) byte-slice equality.
    fn eq_ascii(s: &[u8], pat: &[u8]) -> bool {
        s.len() == pat.len() && s.iter().zip(pat).all(|(a, b)| a.eq_ignore_ascii_case(b))
    }

    const ON: &[u8] = b"ON";
    const OFF: &[u8] = b"OFF";
    const STATE: &[u8] = b"STATE";

    let action = if eq_ascii(token, ON) {
        BleAction::Start
    } else if eq_ascii(token, OFF) {
        BleAction::Stop
    } else if eq_ascii(token, STATE) {
        BleAction::State
    } else {
        return Err(AtOutcome::error(7));
    };

    Ok(BleValidated { action })
}

// ---------------------------------------------------------------------------
// LLM backend configuration (AT+LLMCFG=<model>,<api_key>)
// ---------------------------------------------------------------------------

/// Result of a successful `AT+LLMCFG=` validation. Both fields are owned
/// heapless strings so the payload can outlive the source `AtCommand`.
#[derive(Debug, Clone)]
pub struct LlmCfgValidated {
    /// The validated LLM model name (1..=64 bytes).
    pub model: heapless::String<LLM_MODEL_MAX>,
    /// The validated API key (1..=128 bytes, no whitespace / control bytes).
    pub api_key: heapless::String<LLM_API_KEY_MAX>,
}

/// True if `b` is a control character (C0 `<0x20` or DEL `0x7f`).
///
/// A control byte in a model name or API key would be echoed back into
/// NVS / logs unescaped, breaking framing and enabling log injection —
/// so we reject them up front.
pub fn is_control_byte(b: u8) -> bool {
    b < 0x20 || b == 0x7f
}

/// True if `b` is ASCII whitespace (` `, `\t`, `\r`, `\n`, `\x0c`, `\x0b`).
fn is_ascii_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0x0b)
}

/// Why an agent-facing web fetch must refuse `host`, or `None` when it is a
/// legitimate public target.
///
/// This is the **single** SSRF classifier for the web tools. Both the host
/// `magent_core::web` module and the firmware's on-device tools (`fetch_web`,
/// `get_weather`, `get_news`, `get_flight_price`, via
/// `local_tools::http_get_once`) route through it, so the two cannot drift into
/// disagreeing about what is reachable — which is how the device ended up with
/// no guard at all while the host had one.
///
/// The URL an agent fetches is model-generated, and once a fetched page's text
/// re-enters the conversation its instructions are attacker-controlled. Without
/// this check the agent can be steered into reading the device's own HTTP
/// server, a LAN service, or the cloud-metadata endpoint (`169.254.169.254`)
/// and echo the body back out — where the next turn may ship it to the cloud
/// LLM.
///
/// `allow_private` permits RFC1918 / ULA ranges (the host tools' explicit
/// opt-in); loopback and link-local are refused regardless, since neither is
/// ever a legitimate fetch target.
///
/// **Documented limitation:** only literal addresses and the `localhost` /
/// `.local` names are classified. A hostname that merely *resolves* to a
/// private address is not caught, because this function never performs DNS;
/// closing that needs the connection pinned to the vetted address, which the
/// HTTP client does not expose.
/// Split an `http://` / `https://` URL into `(host, port)`, with `host`
/// borrowed from `url`.
///
/// The one authority parser for both the host web tools and the firmware's
/// on-device tools, so neither can drift into disagreeing about what the host
/// actually is. The port defaults to 443 for `https://` and 80 otherwise, and
/// any `user:pass@` userinfo is discarded.
///
/// The unbracketed-IPv6 case is why this is a shared function rather than a
/// line of inline splitting. A naive split on the *last* colon turns
/// `http://::1/` into host `":"`, port `1`; the socket layer then re-glues
/// those into `::1` and connects to loopback, while
/// [`public_fetch_target_refusal`] is handed `":"`, which parses as neither
/// v4 nor v6 and is therefore waved through — the SSRF guard silently
/// skipped. A lone colon cannot be caught by the classifier, so the fix has
/// to live here: an authority carrying several colons without brackets is
/// kept whole and handed to the classifier, which does recognise it.
///
/// Returns `None` for a non-http(s) scheme or an empty authority.
pub fn parse_http_authority(url: &str) -> Option<(&str, u16)> {
    let (rest, default_port) = match url.strip_prefix("https://") {
        Some(r) => (r, 443),
        // `?` covers the non-http(s) scheme: no prefix, no authority.
        None => (url.strip_prefix("http://")?, 80),
    };
    // WHATWG treats `\` like `/` in special-scheme URLs. Without it,
    // `http://127.0.0.1\@example.com/` parsed the authority as
    // `127.0.0.1\@example.com` → userinfo-stripped host `example.com`, while
    // reqwest dialled 127.0.0.1.
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if authority.is_empty() {
        return None;
    }
    // RFC 3986 requires brackets around an IPv6 literal, but a lax caller can
    // still produce one without; its final hextet must not be read as a port.
    let unbracketed_v6 = !authority.starts_with('[') && authority.matches(':').count() > 1;
    if !unbracketed_v6 {
        if let Some(idx) = authority.rfind(':') {
            let (host, port_str) = (&authority[..idx], &authority[idx + 1..]);
            if !host.is_empty() {
                // An empty port (`http://host:/`) is legal and means "the
                // scheme default". Falling through with the authority verbatim
                // would hand the classifier `host:`, which parses as neither
                // v4 nor v6 nor a known name — so `http://127.0.0.1:/`,
                // `http://169.254.169.254:/` and `http://localhost:/` would be
                // waved through while the HTTP client dials the default port.
                if port_str.is_empty() {
                    return Some((host, default_port));
                }
                if let Ok(port) = port_str.parse::<u16>() {
                    return Some((host, port));
                }
            }
        }
    }
    Some((authority, default_port))
}

/// Resolve a `Location:` header against the URL that produced it, for callers
/// that follow redirects **themselves**.
///
/// Both HTTP clients in this tree follow redirects by default, and neither lets
/// the caller vet a hop *before* it is dialled: the host's `reqwest` resolves and
/// follows internally but does return the final URL (so `magent_core::web` can
/// re-check it after the fact), while the firmware's esp-idf-svc wrapper follows
/// inside C and exposes no post-redirect URL at all. A caller that must keep
/// [`public_fetch_target_refusal`] authoritative therefore turns the built-in
/// following **off**, walks the chain one hop at a time, and uses this function
/// to compute each next URL — re-running the classifier on every hop before it
/// is dialled. That is how the host `magent_core::web` tools and the on-device
/// `local_tools` fetch agree on what is reachable.
///
/// Only the three forms that cannot be mis-joined are accepted: absolute
/// (`https://host/p`), protocol-relative (`//host/p`) and root-relative
/// (`/p`). Anything else — a relative path, an opaque/blank `Location`, a
/// non-http(s) scheme — returns `None`, meaning *do not follow*: the caller
/// keeps the 3xx response rather than guessing a URL whose host it cannot vet.
pub fn resolve_http_redirect(base: &str, location: &str) -> Option<alloc::string::String> {
    let loc = location.trim();
    if loc.starts_with("https://") || loc.starts_with("http://") {
        return Some(alloc::string::String::from(loc));
    }
    let (scheme, authority) = split_scheme_authority(base)?;
    if let Some(rest) = loc.strip_prefix("//") {
        return Some(alloc::format!("{scheme}://{rest}"));
    }
    if loc.starts_with('/') {
        return Some(alloc::format!("{scheme}://{authority}{loc}"));
    }
    None
}

/// `("https"|"http", "host[:port]")` for an absolute http(s) URL. The authority
/// is sliced verbatim so an IPv6 literal round-trips with its brackets.
fn split_scheme_authority(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = match url.strip_prefix("https://") {
        Some(r) => ("https", r),
        None => ("http", url.strip_prefix("http://")?),
    };
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority))
}

/// Why an agent-facing web fetch must refuse `host`, or `None` when it is a
/// legitimate public target.
///
/// This is the **single** SSRF classifier for the web tools. Both the host
/// `magent_core::web` module and the firmware's on-device tools (`fetch_web`,
/// `get_weather`, `get_news`, `get_flight_price`, via
/// `local_tools::http_get_once`) route through it, so the two cannot drift into
/// disagreeing about what is reachable — which is how the device ended up with
/// no guard at all while the host had one.
///
/// The URL an agent fetches is model-generated, and once a fetched page's text
/// re-enters the conversation its instructions are attacker-controlled. Without
/// this check the agent can be steered into reading the device's own HTTP
/// server, a LAN service, or the cloud-metadata endpoint (`169.254.169.254`)
/// and echo the body back out — where the next turn may ship it to the cloud
/// LLM.
///
/// `allow_private` permits RFC1918 / ULA ranges (the host tools' explicit
/// opt-in); loopback and link-local are refused regardless, since neither is
/// ever a legitimate fetch target.
///
/// **Documented limitation:** only literal addresses and the `localhost` /
/// `.local` names are classified. A hostname that merely *resolves* to a
/// private address is not caught, because this function never performs DNS;
/// closing that needs the connection pinned to the vetted address, which the
/// HTTP client does not expose.
///
/// **Redirects are the caller's duty.** This only ever sees the *initial* host,
/// so a public URL that 302s to an internal one would sail past it. Every
/// caller must therefore follow redirects one hop at a time with
/// [`resolve_http_redirect`] and re-run this classifier on each new host — the
/// same re-validation `magent_core::web` does host-side.
pub fn public_fetch_target_refusal(host: &str, allow_private: bool) -> Option<&'static str> {
    let lower = host.to_ascii_lowercase();
    // A trailing dot is the DNS "fully-qualified" spelling: `localhost.`
    // resolves exactly like `localhost` but parsed as neither a known name
    // nor an IP literal, so it sailed through this classifier.
    let lower = lower.trim_end_matches('.');
    if lower.is_empty() {
        return Some("refusing empty host");
    }
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return Some("refusing loopback/mDNS host");
    }
    // A bracketed IPv6 literal (`[::1]`) arrives with its brackets.
    let lit = lower.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = lit.parse::<core::net::Ipv4Addr>() {
        // `is_link_local` covers 169.254.0.0/16 — APIPA *and* the
        // 169.254.169.254 cloud-metadata endpoint. `0.0.0.0`/`0.0.0.0/8`
        // ("this network") and the broadcast address are not fetch targets
        // either, and `is_unspecified` alone misses 0.1.2.3.
        if ip.octets()[0] == 0
            || ip.is_loopback()
            || ip.is_link_local()
            || ip.is_broadcast()
            || (!allow_private && ip.is_private())
        {
            return Some("refusing non-public address");
        }
    }
    if let Ok(ip) = lit.parse::<core::net::Ipv6Addr>() {
        // `::ffff:127.0.0.1` is loopback wearing a v6 spelling; grade the
        // embedded v4 rather than the wrapper. `fc00::/7` (unique-local) is
        // the IPv6 equivalent of RFC1918 and is refused even when private
        // networks are allowed, matching the documented contract.
        let mapped_blocked = ip.to_ipv4_mapped().is_some_and(|v4| {
            v4.octets()[0] == 0
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || (!allow_private && v4.is_private())
        });
        if ip.is_unspecified()
            || ip.is_loopback()
            || ip.is_unicast_link_local()
            || ip.is_unique_local()
            || ip.is_multicast()
            || mapped_blocked
        {
            return Some("refusing non-public address");
        }
    }
    None
}

/// Validate the inputs of an `AT+LLMCFG=<model>,<api_key>` SET request.
///
/// # Validation rules
/// 1. Two positional arguments are required (model, api_key) → error 4
///    if missing or if either is a `key=val` form.
/// 2. Model: `1..=LLM_MODEL_MAX` bytes, valid UTF-8, no NUL, no control
///    characters.
/// 3. API key: `1..=LLM_API_KEY_MAX` bytes, valid UTF-8, no NUL, no
///    control characters, and no ASCII whitespace (a real API key never
///    contains a space; allowing one invites accidental key-splitting /
///    log-framing issues).
///
/// Quoted arguments are ESP-AT-unescaped before validation (via
/// `arg_bytes_decoded`), mirroring the other validators.
pub fn validate_llmcfg_set(cmd: &AtCommand<'_>) -> Result<LlmCfgValidated, AtOutcome> {
    let mut model_dec: heapless::Vec<u8, LLM_MODEL_MAX> = heapless::Vec::new();
    let mut key_dec: heapless::Vec<u8, LLM_API_KEY_MAX> = heapless::Vec::new();

    let model_src = cmd
        .args
        .first()
        .map(|a| arg_bytes_decoded(a, &mut model_dec))
        .transpose()
        .map_err(|_| AtOutcome::error(8))?
        .ok_or_else(|| AtOutcome::error(4))?;
    let key_src = cmd
        .args
        .get(1)
        .map(|a| arg_bytes_decoded(a, &mut key_dec))
        .transpose()
        .map_err(|_| AtOutcome::error(8))?
        .ok_or_else(|| AtOutcome::error(4))?;

    // Model.
    if model_src.is_empty() || model_src.len() > LLM_MODEL_MAX {
        return Err(AtOutcome::error(8));
    }
    if model_src.iter().any(|&c| c == 0 || is_control_byte(c)) {
        return Err(AtOutcome::error(4));
    }
    let model_str = core::str::from_utf8(model_src).map_err(|_| AtOutcome::error(4))?;

    // API key.
    if key_src.is_empty() || key_src.len() > LLM_API_KEY_MAX {
        return Err(AtOutcome::error(8));
    }
    if key_src
        .iter()
        .any(|&c| c == 0 || is_control_byte(c) || is_ascii_whitespace(c))
    {
        return Err(AtOutcome::error(4));
    }
    let key_str = core::str::from_utf8(key_src).map_err(|_| AtOutcome::error(4))?;

    let mut model: heapless::String<LLM_MODEL_MAX> = heapless::String::new();
    model.push_str(model_str).map_err(|_| AtOutcome::error(8))?;
    let mut api_key: heapless::String<LLM_API_KEY_MAX> = heapless::String::new();
    api_key.push_str(key_str).map_err(|_| AtOutcome::error(8))?;

    Ok(LlmCfgValidated { model, api_key })
}

/// Validated payload of an `AT+FLIGHTCFG=` set request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlightCfgValidated {
    /// Provider id (1..=FLIGHT_PROVIDER_MAX bytes, lower-cased).
    pub provider: heapless::String<FLIGHT_PROVIDER_MAX>,
    /// Provider token / API key (1..=FLIGHT_TOKEN_MAX bytes, no whitespace).
    pub token: heapless::String<FLIGHT_TOKEN_MAX>,
}

/// Supported flight-price providers. Kept as a closed set so a typo cannot
/// silently configure a provider the firmware has no client for.
pub const FLIGHT_PROVIDERS: &[&str] = &["travelpayouts", "kiwi"];

/// Validate `AT+FLIGHTCFG=<provider>,<token>`.
///
/// Rules (mirroring [`validate_llmcfg_set`]):
/// 1. Provider: one of [`FLIGHT_PROVIDERS`] (case-insensitive), no control bytes.
/// 2. Token: `1..=FLIGHT_TOKEN_MAX` bytes, valid UTF-8, no NUL, no control
///    bytes and no ASCII whitespace (a real token never contains a space;
///    allowing one invites accidental key-splitting / log-framing issues).
///
/// `+CMDER:4` = missing/malformed argument, `+CMDER:7` = unknown provider,
/// `+CMDER:8` = length violation.
pub fn validate_flightcfg_set(cmd: &AtCommand<'_>) -> Result<FlightCfgValidated, AtOutcome> {
    let mut prov_dec: heapless::Vec<u8, FLIGHT_PROVIDER_MAX> = heapless::Vec::new();
    let mut tok_dec: heapless::Vec<u8, FLIGHT_TOKEN_MAX> = heapless::Vec::new();

    let prov_src = cmd
        .args
        .first()
        .map(|a| arg_bytes_decoded(a, &mut prov_dec))
        .transpose()
        .map_err(|_| AtOutcome::error(4))?
        .ok_or_else(|| AtOutcome::error(4))?;
    let tok_src = cmd
        .args
        .get(1)
        .map(|a| arg_bytes_decoded(a, &mut tok_dec))
        .transpose()
        .map_err(|_| AtOutcome::error(8))?
        .ok_or_else(|| AtOutcome::error(4))?;

    if prov_src.is_empty() || prov_src.len() > FLIGHT_PROVIDER_MAX {
        return Err(AtOutcome::error(8));
    }
    if prov_src.iter().any(|&c| c == 0 || is_control_byte(c)) {
        return Err(AtOutcome::error(4));
    }
    let prov_str = core::str::from_utf8(prov_src).map_err(|_| AtOutcome::error(4))?;

    if tok_src.is_empty() || tok_src.len() > FLIGHT_TOKEN_MAX {
        return Err(AtOutcome::error(8));
    }
    if tok_src
        .iter()
        .any(|&c| c == 0 || is_control_byte(c) || is_ascii_whitespace(c))
    {
        return Err(AtOutcome::error(4));
    }
    let tok_str = core::str::from_utf8(tok_src).map_err(|_| AtOutcome::error(4))?;

    // Normalise the provider to lower-case so `Travelpayouts` / `KIWI` work.
    let mut provider: heapless::String<FLIGHT_PROVIDER_MAX> = heapless::String::new();
    for c in prov_str.chars() {
        provider
            .push(c.to_ascii_lowercase())
            .map_err(|_| AtOutcome::error(8))?;
    }
    if !FLIGHT_PROVIDERS.contains(&provider.as_str()) {
        // Unknown provider: refuse rather than store something unusable.
        return Err(AtOutcome::error(7));
    }

    let mut token: heapless::String<FLIGHT_TOKEN_MAX> = heapless::String::new();
    token.push_str(tok_str).map_err(|_| AtOutcome::error(8))?;

    Ok(FlightCfgValidated { provider, token })
}

// ---------------------------------------------------------------------------
// Outbound HTTP reachability check (AT+HTTPGET=<url>)
// ---------------------------------------------------------------------------

/// Result of a successful `AT+HTTPGET=` validation.
#[derive(Debug, Clone)]
pub struct HttpgetValidated {
    /// The validated URL (scheme + rest), preserved verbatim.
    pub url: heapless::String<HTTPGET_URL_MAX>,
    /// Whether the scheme is `https://` (vs `http://`).
    pub scheme_https: bool,
}

/// Validate the URL argument of an `AT+HTTPGET=<url>` request.
///
/// # Validation rules
/// 1. Exactly one positional argument is required → error 4 if missing.
/// 2. Non-empty and `1..=HTTPGET_URL_MAX` bytes after unescaping.
/// 3. Valid UTF-8 with no NUL / control bytes.
/// 4. Must start with `http://` or `https://` (case-insensitive scheme).
///    Any other scheme (`ftp://`, `file://`, bare `host`, …) is rejected
///    so this command can never reach a non-HTTP service. A bare scheme
///    with no host (`http://`) is also rejected.
///
/// This is the pure decision helper for the SSRF-sensitive URL surface;
/// the firmware still performs the actual DNS/TCP/TLS preflight in
/// `http_get_worker`.
pub fn validate_httpget_set(cmd: &AtCommand<'_>) -> Result<HttpgetValidated, AtOutcome> {
    let arg = cmd.args.first().ok_or_else(|| AtOutcome::error(4))?;
    let mut decoded: heapless::Vec<u8, HTTPGET_URL_MAX> = heapless::Vec::new();
    let src = arg_bytes_decoded(arg, &mut decoded).map_err(|_| AtOutcome::error(8))?;

    if src.is_empty() || src.len() > HTTPGET_URL_MAX {
        return Err(AtOutcome::error(8));
    }
    if src.iter().any(|&c| c == 0 || is_control_byte(c)) {
        return Err(AtOutcome::error(4));
    }
    let s = core::str::from_utf8(src).map_err(|_| AtOutcome::error(4))?;

    // Case-insensitive scheme detection (RFC 3986: schemes are
    // case-insensitive; we only accept the two HTTP schemes).
    let scheme = {
        let lower = s.as_bytes().to_ascii_lowercase();
        if lower.starts_with(b"https://") {
            "https://"
        } else if lower.starts_with(b"http://") {
            "http://"
        } else {
            return Err(AtOutcome::error(4));
        }
    };
    // A URL must have something after the scheme (at least a host).
    if s.len() <= scheme.len() {
        return Err(AtOutcome::error(4));
    }

    let mut url: heapless::String<HTTPGET_URL_MAX> = heapless::String::new();
    url.push_str(s).map_err(|_| AtOutcome::error(8))?;
    Ok(HttpgetValidated {
        scheme_https: scheme == "https://",
        url,
    })
}

// ---------------------------------------------------------------------------
// `AT+AGENTCFG` — persisted core `AgentConfig`
// ---------------------------------------------------------------------------

/// Maximum agent-name length accepted by `AT+AGENTCFG=` (matches
/// `AgentConfig`'s own `MAX_FIELD_LENGTH`).
pub const AGENTCFG_NAME_MAX: usize = 64;

/// True when an `AT+AGENTCFG=` line is the `RESET` sentinel
/// (case-insensitive, exactly one argument).
pub fn agentcfg_is_reset(cmd: &AtCommand<'_>) -> bool {
    cmd.args.len() == 1
        && cmd
            .args
            .first()
            .and_then(crate::at::arg_token)
            .is_some_and(|v| v.eq_ignore_ascii_case(b"RESET"))
}

/// Validate and build the core `AgentConfig` for
/// `AT+AGENTCFG=<name>,<max_iter>,<max_mem>,<watchdog_secs>,<ble_secs>,<skills>,<max_skills>,<debug>`.
///
/// All eight fields are positional and required — a partial form would make
/// "unset" ambiguous with "set to the default", and a config command should be
/// explicit about what it changes. Booleans are `0`/`1`. `AgentConfig::validate`
/// applies the range rules (iterations 1..=1000, memory ≤ the board ceiling,
/// watchdog 1..=60 s, BLE 1..=120 s, skills 1..=50), so this helper only
/// handles the byte/UTF-8/type boundary and then delegates.
///
/// Error codes follow the `+CMDER` table: `4` for a missing/malformed field,
/// `7` for a numeric value the schema rejects (including a `u16` overflow),
/// `8` for an over-long name.
///
/// `RESET` is handled by the caller via [`agentcfg_is_reset`] before this runs
/// (it is not a valid eight-field form).
#[cfg(any(
    feature = "nrf52",
    feature = "esp32",
    feature = "embedded",
    feature = "std"
))]
pub fn validate_agentcfg_set(cmd: &AtCommand<'_>) -> Result<crate::config::AgentConfig, AtOutcome> {
    if cmd.args.len() != 8 {
        return Err(AtOutcome::error(4));
    }

    // Field 0: name (quoted or token, ≤ 64 bytes, no NUL/control).
    let mut name_dec: heapless::Vec<u8, AGENTCFG_NAME_MAX> = heapless::Vec::new();
    let name_src = cmd
        .args
        .first()
        .map(|a| arg_bytes_decoded(a, &mut name_dec))
        .transpose()
        .map_err(|_| AtOutcome::error(8))?
        .ok_or_else(|| AtOutcome::error(4))?;
    if name_src.is_empty() || name_src.len() > AGENTCFG_NAME_MAX {
        return Err(AtOutcome::error(8));
    }
    if name_src.iter().any(|&c| c == 0 || is_control_byte(c)) {
        return Err(AtOutcome::error(4));
    }
    // The query reply is an unquoted comma-separated line; a comma in the
    // name would split it into more fields than the set form accepts, so the
    // value could never be read back and re-sent. Reject it at the boundary
    // rather than storing a name that breaks the command's own round-trip.
    if name_src.contains(&b',') {
        return Err(AtOutcome::error(4));
    }
    let name_str = core::str::from_utf8(name_src).map_err(|_| AtOutcome::error(4))?;
    let mut name: heapless::String<AGENTCFG_NAME_MAX> = heapless::String::new();
    name.push_str(name_str).map_err(|_| AtOutcome::error(8))?;

    let num = |idx: usize| -> Result<u32, AtOutcome> {
        let raw = cmd
            .args
            .get(idx)
            .and_then(crate::at::arg_token)
            .ok_or_else(|| AtOutcome::error(4))?;
        crate::at::parse_u32(raw).ok_or_else(|| AtOutcome::error(4))
    };
    let flag = |idx: usize| -> Result<bool, AtOutcome> {
        match num(idx)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(AtOutcome::error(4)),
        }
    };

    // Numeric range failures are `+CMDER:7` ("number out of range"), not
    // `:8` ("string too long") — the doc table in §4 of
    // `docs/AT_COMMAND_REFERENCE.md` is the contract.
    let u16_of = |v: u32| u16::try_from(v).map_err(|_| AtOutcome::error(7));

    let config = crate::config::AgentConfig {
        name,
        max_iterations: u16_of(num(1)?)?,
        max_memory: num(2)?,
        watchdog_timeout_secs: u16_of(num(3)?)?,
        ble_timeout_secs: u16_of(num(4)?)?,
        skills_enabled: flag(5)?,
        max_skills: u16_of(num(6)?)?,
        debug_enabled: flag(7)?,
    };
    config.validate().map_err(|_| AtOutcome::error(7))?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::at::{AtArg, AtCommand, AtCommandKind, AtOp, AtVerb};
    use heapless::Vec;

    /// Build a `AtCommand` from raw byte args (all Token). Uses
    /// the fact that `b"..."` literals are `'static` so we never
    /// need to leak anything.
    fn cmd_token(op: AtOp, args: &[&'static [u8]]) -> AtCommand<'static> {
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        for &a in args {
            let _ = v.push(AtArg::Token(a));
        }
        AtCommand {
            op,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        }
    }

    #[test]
    fn cwjap_set_happy_path() {
        let c = cmd_token(AtOp::CwJap, &[b"myssid", b"mypass"]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.ssid.as_str(), "myssid");
        assert_eq!(v.password.as_str(), "mypass");
    }

    #[test]
    fn cwjap_set_empty_password_is_open_network() {
        let c = cmd_token(AtOp::CwJap, &[b"open-net"]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.ssid.as_str(), "open-net");
        assert_eq!(v.password.as_str(), "");
    }

    /// Build a Set command whose args are all quoted (so escape
    /// decoding is exercised end-to-end).
    fn cmd_quoted(op: AtOp, args: &[&'static [u8]]) -> AtCommand<'static> {
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        for &a in args {
            let _ = v.push(AtArg::Quoted(a));
        }
        AtCommand {
            op,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        }
    }

    #[test]
    fn cwjap_set_unescapes_quoted_ssid_and_password() {
        // `AT+CWJAP="my\,ssid","pa\"ss\"wd"` — the escapes must be
        // decoded so the real SSID / passphrase round-trip correctly.
        let c = cmd_quoted(AtOp::CwJap, &[b"my\\,ssid", b"pa\"ss\\wd"]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.ssid.as_str(), "my,ssid");
        assert_eq!(v.password.as_str(), "pa\"ss\\wd");
    }

    #[test]
    fn cwjap_set_unescape_does_not_expand_over_max() {
        // A quoted SSID that decodes to just over the max (33 bytes)
        // must be rejected as too long — length is checked on the
        // *decoded* bytes, not the escaped wire bytes.
        static RAW: [u8; 34] = [b'x'; 34];
        let c = cmd_quoted(AtOp::CwJap, &[&RAW]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwhostname_set_unescapes_quoted() {
        let c = cmd_quoted(AtOp::CwHostname, &[b"iot\\-1\\,a"]);
        let v = validate_cwhostname_set(&c).expect("ok");
        // `\-` and `\,` are unknown/known escapes respectively;
        // `\,` → `,`, `\-` stays as-is (backslash preserved).
        assert_eq!(v.hostname.as_str(), "iot\\-1,a");
    }

    #[test]
    fn cwjap_set_rejects_empty_ssid() {
        let c = cmd_token(AtOp::CwJap, &[b"", b"x"]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwjap_set_rejects_oversized_ssid() {
        // 33 bytes (one over CWJAP_SSID_MAX=32) of 'A'.
        static BIG: [u8; 33] = [b'A'; 33];
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Token(&BIG));
        let _ = v.push(AtArg::Token(b"p"));
        let c = AtCommand {
            op: AtOp::CwJap,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwjap_set_accepts_max_ssid() {
        static EXACT: [u8; 32] = [b'A'; 32];
        let c = cmd_token(AtOp::CwJap, &[&EXACT, b""]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.ssid.len(), CWJAP_SSID_MAX);
    }

    #[test]
    fn cwjap_set_rejects_nul_in_ssid() {
        let c = cmd_token(AtOp::CwJap, &[b"foo\0bar", b"p"]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwjap_set_rejects_nul_in_password() {
        let c = cmd_token(AtOp::CwJap, &[b"ssid", b"pass\0word"]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwjap_set_rejects_invalid_utf8() {
        let c = cmd_token(AtOp::CwJap, &[b"\xff\xfe\xfd", b"p"]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwjap_set_rejects_oversized_password() {
        static BIG: [u8; 65] = [b'p'; 65];
        let c = cmd_token(AtOp::CwJap, &[b"ssid", &BIG]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwjap_set_accepts_max_password() {
        static EXACT: [u8; 64] = [b'p'; 64];
        let c = cmd_token(AtOp::CwJap, &[b"ssid", &EXACT]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.password.len(), CWJAP_PASS_MAX);
    }

    #[test]
    fn cwjap_set_accepts_unicode_password() {
        // WPA2 allows any 8-bit string; some routers use extended chars.
        let c = cmd_token(AtOp::CwJap, &[b"ssid", "密码-1234".as_bytes()]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.password.as_str(), "密码-1234");
    }

    #[test]
    fn hostname_set_happy_path() {
        let c = cmd_token(AtOp::CwHostname, &[b"my-device"]);
        let v = validate_cwhostname_set(&c).expect("ok");
        assert_eq!(v.hostname.as_str(), "my-device");
    }

    #[test]
    fn hostname_set_rejects_oversized() {
        static BIG: [u8; 33] = [b'h'; 33];
        let c = cmd_token(AtOp::CwHostname, &[&BIG]);
        let err = validate_cwhostname_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn hostname_set_rejects_nul() {
        let c = cmd_token(AtOp::CwHostname, &[b"foo\0bar"]);
        let err = validate_cwhostname_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwmode_set_accepts_known_modes() {
        // Build static byte slices so the references are 'static.
        static M1: &[u8] = b"1";
        static M2: &[u8] = b"2";
        static M3: &[u8] = b"3";
        for (i, m) in CWMODE_VALID.iter().enumerate() {
            let c = match i {
                0 => cmd_token(AtOp::CwMode, &[M1]),
                1 => cmd_token(AtOp::CwMode, &[M2]),
                _ => cmd_token(AtOp::CwMode, &[M3]),
            };
            let v = validate_cwmode_set(&c).expect("ok");
            assert_eq!(v.mode, *m);
        }
    }

    #[test]
    fn cwmode_set_rejects_invalid_mode() {
        let c = cmd_token(AtOp::CwMode, &[b"4"]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn cwmode_set_rejects_non_numeric() {
        let c = cmd_token(AtOp::CwMode, &[b"abc"]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn cwmode_set_rejects_overflow() {
        let c = cmd_token(AtOp::CwMode, &[b"999"]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    // -- AT+TIMEZONE validator tests -------------------------------------

    #[test]
    fn timezone_set_accepts_zero() {
        let c = cmd_token(AtOp::Timezone, &[b"0"]);
        let v = validate_timezone_set(&c).expect("ok");
        assert_eq!(v.offset_minutes, 0);
    }

    #[test]
    fn timezone_set_accepts_max_positive() {
        // +14h = Kiritimati, the highest real-world timezone offset.
        let c = cmd_token(AtOp::Timezone, &[b"840"]);
        let v = validate_timezone_set(&c).expect("ok");
        assert_eq!(v.offset_minutes, 840);
    }

    #[test]
    fn timezone_set_accepts_max_negative() {
        // -12h = Baker Island, the lowest real-world timezone offset.
        let c = cmd_token(AtOp::Timezone, &[b"-720"]);
        let v = validate_timezone_set(&c).expect("ok");
        assert_eq!(v.offset_minutes, -720);
    }

    #[test]
    fn timezone_set_rejects_above_max() {
        let c = cmd_token(AtOp::Timezone, &[b"841"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_below_min() {
        let c = cmd_token(AtOp::Timezone, &[b"-721"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_non_numeric() {
        let c = cmd_token(AtOp::Timezone, &[b"abc"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_empty() {
        let c = cmd_token(AtOp::Timezone, &[b""]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_bare_minus() {
        // `-` alone is not a valid integer.
        let c = cmd_token(AtOp::Timezone, &[b"-"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_overflow() {
        // 99999 is well outside i32's signed range after sign.
        let c = cmd_token(AtOp::Timezone, &[b"99999"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_quoted_arg() {
        // `AT+TIMEZONE="480"` would let an attacker slip quotes past
        // the validator. The validator only accepts Token args.
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Quoted(b"480"));
        let c = AtCommand {
            op: AtOp::Timezone,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn timezone_set_rejects_named_arg() {
        // `key=val` style isn't valid for TIMEZONE.
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Named {
            key: b"offset",
            val: b"480",
        });
        let c = AtCommand {
            op: AtOp::Timezone,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn timezone_set_rejects_no_args() {
        let c = cmd_token(AtOp::Timezone, &[]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    // ---------------------------------------------------------------------
    // AT+BLE= validator
    // ---------------------------------------------------------------------

    #[test]
    fn ble_set_accepts_on_upper() {
        let c = cmd_token(AtOp::Ble, &[b"ON"]);
        let v = validate_ble_set(&c).expect("ok");
        assert_eq!(v.action, BleAction::Start);
    }

    #[test]
    fn ble_set_accepts_on_lower() {
        let c = cmd_token(AtOp::Ble, &[b"on"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::Start);
    }

    #[test]
    fn ble_set_accepts_on_mixed_case() {
        let c = cmd_token(AtOp::Ble, &[b"oN"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::Start);
    }

    #[test]
    fn ble_set_accepts_off_upper() {
        let c = cmd_token(AtOp::Ble, &[b"OFF"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::Stop);
    }

    #[test]
    fn ble_set_accepts_off_lower() {
        let c = cmd_token(AtOp::Ble, &[b"off"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::Stop);
    }

    #[test]
    fn ble_set_accepts_state() {
        let c = cmd_token(AtOp::Ble, &[b"STATE"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::State);
    }

    #[test]
    fn ble_set_accepts_state_lower() {
        let c = cmd_token(AtOp::Ble, &[b"state"]);
        assert_eq!(validate_ble_set(&c).unwrap().action, BleAction::State);
    }

    #[test]
    fn ble_set_rejects_unknown_verb() {
        let c = cmd_token(AtOp::Ble, &[b"MAYBE"]);
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn ble_set_rejects_numeric() {
        let c = cmd_token(AtOp::Ble, &[b"1"]);
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn ble_set_rejects_empty_token() {
        let c = cmd_token(AtOp::Ble, &[b""]);
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn ble_set_rejects_no_args() {
        let c = cmd_token(AtOp::Ble, &[]);
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn ble_set_rejects_quoted_arg() {
        // `AT+BLE="ON"` — quotes must not slip through the validator.
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Quoted(b"ON"));
        let c = AtCommand {
            op: AtOp::Ble,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn ble_set_rejects_named_arg() {
        // `AT+BLE=action=ON` — key=val is not a legal form.
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Named {
            key: b"action",
            val: b"ON",
        });
        let c = AtCommand {
            op: AtOp::Ble,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn ble_set_rejects_superset_token() {
        // "STATEE" is a prefix-plus — must not match "STATE".
        let c = cmd_token(AtOp::Ble, &[b"STATEE"]);
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn ble_set_ignores_extra_args() {
        // `AT+BLE=ON,whatever` — trailing args are ignored, first wins.
        let c = cmd_token(AtOp::Ble, &[b"ON", b"whatever"]);
        let v = validate_ble_set(&c).expect("ok");
        assert_eq!(v.action, BleAction::Start);
    }

    #[test]
    fn ble_set_rejects_utf8_garbage() {
        // Non-ASCII bytes (0xE9 é) are not a legal verb → error 7.
        static GARBAGE: [u8; 3] = [0xE9, 0xE9, 0xE9];
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Token(&GARBAGE));
        let c = AtCommand {
            op: AtOp::Ble,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_ble_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    // ---------------------------------------------------------------------
    // AT+LLMCFG= validator
    // ---------------------------------------------------------------------

    #[test]
    fn llmcfg_set_happy_path() {
        let c = cmd_token(AtOp::LlmCfg, &[b"deepseek-chat", b"sk-abc123"]);
        let v = validate_llmcfg_set(&c).expect("ok");
        assert_eq!(v.model.as_str(), "deepseek-chat");
        assert_eq!(v.api_key.as_str(), "sk-abc123");
    }

    #[test]
    fn llmcfg_set_accepts_max_model() {
        static MODEL: [u8; 64] = [b'M'; 64];
        let c = cmd_token(AtOp::LlmCfg, &[&MODEL, b"sk-key"]);
        let v = validate_llmcfg_set(&c).expect("ok");
        assert_eq!(v.model.len(), 64);
    }

    #[test]
    fn llmcfg_set_rejects_oversized_model() {
        static MODEL: [u8; 65] = [b'M'; 65];
        let c = cmd_token(AtOp::LlmCfg, &[&MODEL, b"sk-key"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn llmcfg_set_rejects_empty_model() {
        let c = cmd_token(AtOp::LlmCfg, &[b"", b"sk-key"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn llmcfg_set_rejects_empty_key() {
        let c = cmd_token(AtOp::LlmCfg, &[b"model", b""]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn llmcfg_set_rejects_missing_key() {
        let c = cmd_token(AtOp::LlmCfg, &[b"model"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_no_args() {
        let c = cmd_token(AtOp::LlmCfg, &[]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_nul_in_model() {
        static MODEL: [u8; 2] = [b'a', 0];
        let c = cmd_token(AtOp::LlmCfg, &[&MODEL, b"sk-key"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_control_in_key() {
        static KEY: [u8; 2] = [b'a', 0x1b];
        let c = cmd_token(AtOp::LlmCfg, &[b"model", &KEY]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_space_in_key() {
        let c = cmd_token(AtOp::LlmCfg, &[b"model", b"sk abc"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_tab_in_key() {
        let c = cmd_token(AtOp::LlmCfg, &[b"model", b"sk\tabc"]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_rejects_invalid_utf8_key() {
        static KEY: [u8; 2] = [0xC3, 0x28];
        let c = cmd_token(AtOp::LlmCfg, &[b"model", &KEY]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn llmcfg_set_unescapes_quoted_args() {
        let c = cmd_quoted(AtOp::LlmCfg, &[b"my\\,model", b"sk\\\"key"]);
        let v = validate_llmcfg_set(&c).expect("ok");
        assert_eq!(v.model.as_str(), "my,model");
        assert_eq!(v.api_key.as_str(), "sk\"key");
    }

    #[test]
    fn llmcfg_set_rejects_named_arg() {
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Named {
            key: b"model",
            val: b"deepseek",
        });
        let c = AtCommand {
            op: AtOp::LlmCfg,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn is_control_byte_boundaries() {
        assert!(is_control_byte(0x00));
        assert!(is_control_byte(0x1f));
        assert!(is_control_byte(0x7f));
        assert!(!is_control_byte(0x20));
        assert!(!is_control_byte(0x41));
        assert!(!is_control_byte(0x80));
    }

    // ---------------------------------------------------------------------
    // AT+HTTPGET= validator
    // ---------------------------------------------------------------------

    #[test]
    fn httpget_set_accepts_http() {
        let c = cmd_token(AtOp::HttpGet, &[b"http://example.com"]);
        let v = validate_httpget_set(&c).expect("ok");
        assert!(!v.scheme_https);
        assert_eq!(v.url.as_str(), "http://example.com");
    }

    #[test]
    fn httpget_set_accepts_https() {
        let c = cmd_token(AtOp::HttpGet, &[b"https://api.deepseek.com/v1"]);
        let v = validate_httpget_set(&c).expect("ok");
        assert!(v.scheme_https);
        assert_eq!(v.url.as_str(), "https://api.deepseek.com/v1");
    }

    #[test]
    fn httpget_set_accepts_uppercase_scheme() {
        let c = cmd_token(AtOp::HttpGet, &[b"HTTPS://Example.COM/path"]);
        let v = validate_httpget_set(&c).expect("ok");
        assert!(v.scheme_https);
        // Scheme preserved verbatim; classification is case-insensitive.
        assert_eq!(v.url.as_str(), "HTTPS://Example.COM/path");
    }

    #[test]
    fn httpget_set_rejects_non_http_scheme() {
        let c = cmd_token(AtOp::HttpGet, &[b"ftp://host/file"]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_file_scheme() {
        let c = cmd_token(AtOp::HttpGet, &[b"file:///etc/passwd"]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_bare_host() {
        let c = cmd_token(AtOp::HttpGet, &[b"example.com"]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_bare_scheme() {
        let c = cmd_token(AtOp::HttpGet, &[b"http://"]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_empty() {
        let c = cmd_token(AtOp::HttpGet, &[b""]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn httpget_set_rejects_no_args() {
        let c = cmd_token(AtOp::HttpGet, &[]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_control_in_url() {
        // 0x0a (newline) — CRLF injection into HTTP request / logs.
        static URL: [u8; 12] = [
            b'h', b't', b't', b'p', b':', b'/', b'/', b'a', b'b', b'c', 0x0a, b'x',
        ];
        let c = cmd_token(AtOp::HttpGet, &[&URL]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_nul_in_url() {
        static URL: [u8; 8] = [b'h', b't', b't', b'p', b':', b'/', b'/', 0];
        let c = cmd_token(AtOp::HttpGet, &[&URL]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_rejects_invalid_utf8() {
        static URL: [u8; 9] = [b'h', b't', b't', b'p', b':', b'/', b'/', b'a', 0xC3];
        let c = cmd_token(AtOp::HttpGet, &[&URL]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    /// Full AT-pipeline robustness: parse untrusted bytes, then for every
    /// command that parses, dispatch it to the validator for its op. This
    /// exercises the entire validation layer (the firmware's security
    /// boundary) and asserts it never panics on any input.
    #[test]
    fn validators_never_panic_on_parsed_adversarial_commands() {
        // Deterministic pseudo-random byte generator (LCG).
        let mut acc: u32 = 0x13579BDF;
        let gen = |acc: &mut u32| {
            *acc = acc.wrapping_mul(1664525).wrapping_add(1013904223);
            (*acc >> 24) as u8
        };

        // Sweep structured inputs across a range of lengths and op prefixes.
        let prefixes: &[&[u8]] = &[
            b"AT",
            b"AT+CWJAP=",
            b"AT+CWHOSTNAME=",
            b"AT+CWMODE=",
            b"AT+TIMEZONE=",
            b"AT+BLE=",
            b"AT+LLMCFG=",
            b"AT+HTTPGET=",
        ];
        for pfx in prefixes {
            for len in 0..=64usize {
                // Token arg case.
                let mut input: alloc::vec::Vec<u8> = pfx.to_vec();
                for _ in 0..len {
                    input.push(gen(&mut acc));
                }
                let run_through_validators = |input: &[u8]| {
                    if let Ok(cmd) = crate::at::parse_line(input) {
                        match cmd.op {
                            AtOp::CwJap => {
                                let _ = validate_cwjap_set(&cmd);
                            }
                            AtOp::CwHostname => {
                                let _ = validate_cwhostname_set(&cmd);
                            }
                            AtOp::CwMode => {
                                let _ = validate_cwmode_set(&cmd);
                            }
                            AtOp::Timezone => {
                                let _ = validate_timezone_set(&cmd);
                            }
                            AtOp::Ble => {
                                let _ = validate_ble_set(&cmd);
                            }
                            AtOp::LlmCfg => {
                                let _ = validate_llmcfg_set(&cmd);
                            }
                            AtOp::HttpGet => {
                                let _ = validate_httpget_set(&cmd);
                            }
                            _ => {}
                        }
                    }
                };
                run_through_validators(&input);

                // Quoted-arg case.
                let mut quoted: alloc::vec::Vec<u8> = pfx.to_vec();
                quoted.push(b'\"');
                for _ in 0..len {
                    quoted.push(gen(&mut acc));
                }
                quoted.push(b'\"');
                run_through_validators(&quoted);

                // Embedded NUL / 0xFF / high bytes.
                let mut nasty: alloc::vec::Vec<u8> = pfx.to_vec();
                for _ in 0..len {
                    let b = gen(&mut acc);
                    nasty.push(match b % 4 {
                        0 => 0x00,
                        1 => 0xFF,
                        2 => 0x80 | (b & 0x3F),
                        _ => b,
                    });
                }
                run_through_validators(&nasty);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Additional boundary tests (audit-2026-08)
    // ---------------------------------------------------------------------

    /// Build a Set command whose first arg is a `key=val` (Named) form.
    fn cmd_named(op: AtOp, key: &'static [u8], val: &'static [u8]) -> AtCommand<'static> {
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Named { key, val });
        AtCommand {
            op,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        }
    }

    #[test]
    fn cwjap_set_rejects_named_arg() {
        // `AT+CWJAP=ssid=my,pass` — key=val is never a legal CWJAP form.
        let c = cmd_named(AtOp::CwJap, b"ssid", b"my");
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwjap_set_rejects_no_args() {
        let c = cmd_token(AtOp::CwJap, &[]);
        let err = validate_cwjap_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwjap_set_ignores_extra_args() {
        // `AT+CWJAP="ssid","pass","extra"` — extra args are ignored.
        let c = cmd_token(AtOp::CwJap, &[b"ssid", b"pass", b"extra"]);
        let v = validate_cwjap_set(&c).expect("ok");
        assert_eq!(v.ssid.as_str(), "ssid");
        assert_eq!(v.password.as_str(), "pass");
    }

    #[test]
    fn hostname_set_rejects_empty() {
        let c = cmd_token(AtOp::CwHostname, &[b""]);
        let err = validate_cwhostname_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn hostname_set_rejects_invalid_utf8() {
        let c = cmd_token(AtOp::CwHostname, &[b"\xff\xfe"]);
        let err = validate_cwhostname_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn hostname_set_rejects_named_arg() {
        let c = cmd_named(AtOp::CwHostname, b"hostname", b"x");
        let err = validate_cwhostname_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn cwmode_set_rejects_mode_zero() {
        // Mode 0 is outside the valid {1,2,3} set.
        let c = cmd_token(AtOp::CwMode, &[b"0"]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn cwmode_set_rejects_empty() {
        let c = cmd_token(AtOp::CwMode, &[b""]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn cwmode_set_rejects_quoted() {
        // Quoted args are not accepted for CWMODE (error 4 — not Token).
        let mut v: Vec<AtArg<'static>, { crate::at::MAX_ARGUMENTS }> = Vec::new();
        let _ = v.push(AtArg::Quoted(b"1"));
        let c = AtCommand {
            op: AtOp::CwMode,
            kind: AtCommandKind::Set,
            args: v,
            verb: AtVerb::Set,
        };
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn cwmode_set_rejects_no_args() {
        let c = cmd_token(AtOp::CwMode, &[]);
        let err = validate_cwmode_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn timezone_set_accepts_mid_negative() {
        // A mid-range negative offset (-5h = US Eastern) is valid.
        let c = cmd_token(AtOp::Timezone, &[b"-300"]);
        let v = validate_timezone_set(&c).expect("ok");
        assert_eq!(v.offset_minutes, -300);
    }

    #[test]
    fn timezone_set_rejects_leading_plus() {
        // `AT+TIMEZONE=+480` — AT numeric args are unsigned by convention;
        // only a leading `-` is accepted. `+` must be rejected.
        let c = cmd_token(AtOp::Timezone, &[b"+480"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_i32_overflow() {
        // 3_000_000_000 > i32::MAX (2_147_483_647): the magnitude parse must
        // bail out *before* the timezone-band check so an out-of-range
        // integer can't wrap into a valid offset.
        let c = cmd_token(AtOp::Timezone, &[b"3000000000"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn timezone_set_rejects_negative_i32_overflow() {
        let c = cmd_token(AtOp::Timezone, &[b"-3000000000"]);
        let err = validate_timezone_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 7 }));
    }

    #[test]
    fn llmcfg_set_accepts_max_key() {
        static KEY: [u8; 128] = [b'k'; 128];
        let c = cmd_token(AtOp::LlmCfg, &[b"model", &KEY]);
        let v = validate_llmcfg_set(&c).expect("ok");
        assert_eq!(v.api_key.len(), LLM_API_KEY_MAX);
    }

    #[test]
    fn llmcfg_set_rejects_oversized_key() {
        static KEY: [u8; 129] = [b'k'; 129];
        let c = cmd_token(AtOp::LlmCfg, &[b"model", &KEY]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn llmcfg_set_rejects_nul_in_key() {
        static KEY: [u8; 2] = [b'a', 0];
        let c = cmd_token(AtOp::LlmCfg, &[b"model", &KEY]);
        let err = validate_llmcfg_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 4 }));
    }

    #[test]
    fn httpget_set_accepts_max_url() {
        // 512-byte URL: 8 scheme bytes + 504 host/path bytes.
        static PATH: [u8; HTTPGET_URL_MAX] = {
            let mut b = [b'x'; HTTPGET_URL_MAX];
            let prefix = b"http://";
            let mut i = 0;
            while i < prefix.len() {
                b[i] = prefix[i];
                i += 1;
            }
            b
        };
        let c = cmd_token(AtOp::HttpGet, &[&PATH]);
        let v = validate_httpget_set(&c).expect("ok");
        assert_eq!(v.url.len(), HTTPGET_URL_MAX);
    }

    #[test]
    fn httpget_set_rejects_oversized_url() {
        static URL: [u8; 513] = [b'h'; 513];
        let c = cmd_token(AtOp::HttpGet, &[&URL]);
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn httpget_set_accepts_quoted_url() {
        // A quoted (not bare-Token) URL is accepted via the unescape path.
        let c = cmd_quoted(AtOp::HttpGet, &[b"http://example.com/pa,th"]);
        let v = validate_httpget_set(&c).expect("ok");
        assert!(!v.scheme_https);
        assert_eq!(v.url.as_str(), "http://example.com/pa,th");
    }

    #[test]
    fn httpget_set_rejects_named_arg() {
        let c = cmd_named(AtOp::HttpGet, b"url", b"x");
        let err = validate_httpget_set(&c).unwrap_err();
        assert!(matches!(err, AtOutcome::Error { code: 8 }));
    }

    #[test]
    fn parse_http_authority_basics() {
        assert_eq!(
            parse_http_authority("https://example.com/p?q=1"),
            Some(("example.com", 443))
        );
        assert_eq!(
            parse_http_authority("http://example.com:8080/x"),
            Some(("example.com", 8080))
        );
        // Userinfo is discarded, so the guard sees the real host.
        assert_eq!(
            parse_http_authority("http://user:pass@10.0.0.1/x"),
            Some(("10.0.0.1", 80))
        );
        assert_eq!(parse_http_authority("ftp://example.com/"), None);
        assert_eq!(parse_http_authority("http://"), None);
        assert_eq!(parse_http_authority("http:///path"), None);
    }

    #[test]
    fn parse_http_authority_keeps_bracketed_ipv6_intact() {
        assert_eq!(parse_http_authority("http://[::1]/"), Some(("[::1]", 80)));
        assert_eq!(
            parse_http_authority("http://[fe80::1]:8080/"),
            Some(("[fe80::1]", 8080))
        );
    }

    /// `http://::1/` used to parse as host `":"`, port `1` — a hostname the
    /// classifier cannot identify, while the socket layer re-glued the pair
    /// back into `::1` and dialled loopback. The literal must stay whole.
    #[test]
    fn parse_http_authority_keeps_unbracketed_ipv6_intact() {
        assert_eq!(parse_http_authority("http://::1/"), Some(("::1", 80)));
        assert_eq!(
            parse_http_authority("http://::ffff:127.0.0.1/"),
            Some(("::ffff:127.0.0.1", 80))
        );
        assert_eq!(
            parse_http_authority("http://2001:db8::1/x"),
            Some(("2001:db8::1", 80))
        );
    }

    /// End-to-end: the pairing that matters is `parse_http_authority` feeding
    /// `public_fetch_target_refusal` — an unbracketed loopback authority must
    /// reach the classifier as a literal it recognises.
    #[test]
    fn unbracketed_ipv6_loopback_survives_the_full_guard() {
        for url in ["http://::1/", "http://[::1]/", "http://::ffff:127.0.0.1/"] {
            let (host, _) = parse_http_authority(url).expect("parses");
            assert!(
                public_fetch_target_refusal(host, true).is_some(),
                "{url} parsed to {host:?}, which the guard must refuse"
            );
        }
    }

    /// An empty port is legal (`http://host:/`) and the URL client dials the
    /// scheme default. The parser must drop the dangling colon so the
    /// classifier sees the bare host; returning `"host:"` verbatim made the
    /// SSRF guard wave the target through.
    #[test]
    fn parse_http_authority_drops_an_empty_port() {
        assert_eq!(
            parse_http_authority("http://127.0.0.1:/admin"),
            Some(("127.0.0.1", 80))
        );
        assert_eq!(
            parse_http_authority("https://example.com:/p"),
            Some(("example.com", 443))
        );
        assert_eq!(
            parse_http_authority("http://[::1]:/"),
            Some(("[::1]", 80))
        );
        // Userinfo and an empty port together.
        assert_eq!(
            parse_http_authority("http://user:pass@10.0.0.1:/x"),
            Some(("10.0.0.1", 80))
        );
    }

    #[test]
    fn resolve_redirect_accepts_the_three_safe_forms() {
        // Absolute: taken verbatim, scheme changes included.
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "https://b.example/y").as_deref(),
            Some("https://b.example/y")
        );
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "http://b.example/y").as_deref(),
            Some("http://b.example/y")
        );
        // Protocol-relative inherits the base scheme.
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "//b.example/y").as_deref(),
            Some("https://b.example/y")
        );
        // Root-relative keeps scheme + authority (port included).
        assert_eq!(
            resolve_http_redirect("http://a.example:8080/x", "/y").as_deref(),
            Some("http://a.example:8080/y")
        );
        // Surrounding whitespace is stripped.
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "  https://b.example/y  ").as_deref(),
            Some("https://b.example/y")
        );
        // An IPv6 literal round-trips with its brackets.
        assert_eq!(
            resolve_http_redirect("https://[fe80::1]:8443/x", "/y").as_deref(),
            Some("https://[fe80::1]:8443/y")
        );
    }

    #[test]
    fn resolve_redirect_refuses_what_it_cannot_revet() {
        // Relative path: joining it needs the base path, which we do not keep.
        assert_eq!(resolve_http_redirect("https://a.example/x/y", "z"), None);
        assert_eq!(resolve_http_redirect("https://a.example/x/y", "../z"), None);
        // Blank / opaque Location, and a non-http(s) scheme.
        assert_eq!(resolve_http_redirect("https://a.example/x", ""), None);
        assert_eq!(resolve_http_redirect("https://a.example/x", "  "), None);
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "mailto:a@b"),
            None
        );
        assert_eq!(
            resolve_http_redirect("https://a.example/x", "ftp://a.example/"),
            None
        );
        // A base with no authority cannot host a root-relative hop.
        assert_eq!(resolve_http_redirect("https://", "/y"), None);
    }

    /// The pairing that closes the redirect hole: a public hop's `Location` is
    /// resolved and then fed straight back to the classifier, which must refuse
    /// the internal targets a hostile 302 would aim at.
    #[test]
    fn resolved_redirect_targets_are_classified() {
        for (base, loc) in [
            (
                "https://evil.example/",
                "http://169.254.169.254/latest/meta-data/",
            ),
            ("https://evil.example/", "http://127.0.0.1:80/admin"),
            ("https://evil.example/", "//192.168.1.1/router"),
            ("https://evil.example/", "//printer.local/status"),
        ] {
            let next = resolve_http_redirect(base, loc).expect("resolves");
            let (host, _) = parse_http_authority(&next).expect("parses");
            assert!(
                public_fetch_target_refusal(host, false).is_some(),
                "{next} should have been refused"
            );
        }
        // …while a public redirect target still passes.
        let next =
            resolve_http_redirect("https://evil.example/", "//cdn.example/a").expect("resolves");
        let (host, _) = parse_http_authority(&next).expect("parses");
        assert_eq!(public_fetch_target_refusal(host, false), None);
    }

    /// End-to-end for the empty-port form: these URLs all parse as the default
    /// port on a real URL client and are accepted by the socket layer, so the
    /// guard must refuse every one of them.
    #[test]
    fn empty_port_targets_survive_the_full_guard() {
        for url in [
            "http://127.0.0.1:/admin",
            "http://127.8.8.8:/x",
            "http://169.254.169.254:/latest/meta-data/",
            "http://localhost:/x",
            "http://printer.local:/x",
            "http://10.0.0.1:/x",
            "http://[::1]:/",
            "http://[fe80::1]:/",
        ] {
            let (host, _) = parse_http_authority(url).expect("parses");
            assert!(
                public_fetch_target_refusal(host, false).is_some(),
                "{url} parsed to {host:?}, which the guard must refuse"
            );
        }
    }

    #[test]
    fn public_fetch_refuses_loopback_names() {
        for h in ["localhost", "LOCALHOST", "foo.localhost", "printer.local"] {
            assert!(
                public_fetch_target_refusal(h, false).is_some(),
                "{h} must be refused"
            );
            assert!(
                public_fetch_target_refusal(h, true).is_some(),
                "{h} must be refused even with allow_private"
            );
        }
    }

    #[test]
    fn public_fetch_refuses_loopback_and_link_local_v4() {
        for h in ["127.0.0.1", "127.8.8.8", "169.254.169.254", "169.254.1.1"] {
            assert!(
                public_fetch_target_refusal(h, true).is_some(),
                "{h} must be refused even with allow_private"
            );
        }
    }

    /// A trailing dot is the FQDN spelling of the same name; `0.0.0.0/8`
    /// and the broadcast address are not fetch targets.
    #[test]
    fn public_fetch_refuses_trailing_dot_and_unspecified_v4() {
        for h in [
            "localhost.",
            "127.0.0.1.",
            "169.254.169.254.",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
        ] {
            assert!(
                public_fetch_target_refusal(h, false).is_some(),
                "{h} must be refused"
            );
        }
    }

    /// IPv6 unique-local (`fc00::/7`), unspecified (`::`) and multicast are
    /// never public targets, even with `allow_private`.
    #[test]
    fn public_fetch_refuses_ipv6_ula_unspecified_and_multicast() {
        for h in ["[fd12:3456::1]", "[fc00::1]", "[::]", "[ff02::1]"] {
            assert!(
                public_fetch_target_refusal(h, false).is_some(),
                "{h} must be refused"
            );
            assert!(
                public_fetch_target_refusal(h, true).is_some(),
                "{h} must be refused even with allow_private"
            );
        }
    }

    #[test]
    fn public_fetch_refuses_private_v4_unless_allowed() {
        for h in ["10.0.0.1", "172.16.0.1", "192.168.1.1"] {
            assert!(
                public_fetch_target_refusal(h, false).is_some(),
                "{h} must be refused by default"
            );
            assert!(
                public_fetch_target_refusal(h, true).is_none(),
                "{h} should be allowed when allow_private is set"
            );
        }
    }

    #[test]
    fn public_fetch_refuses_v6_loopback_link_local_and_mapped() {
        for h in ["[::1]", "::1", "[fe80::1]", "[::ffff:127.0.0.1]"] {
            assert!(
                public_fetch_target_refusal(h, true).is_some(),
                "{h} must be refused even with allow_private"
            );
        }
        // A v4-mapped private address follows the same allow_private rule as
        // its embedded v4.
        assert!(public_fetch_target_refusal("[::ffff:192.168.0.1]", false).is_some());
        assert!(public_fetch_target_refusal("[::ffff:192.168.0.1]", true).is_none());
    }

    #[test]
    fn public_fetch_allows_public_targets() {
        for h in ["example.com", "8.8.8.8", "1.1.1.1", "[2606:4700:4700::1111]"] {
            assert!(
                public_fetch_target_refusal(h, false).is_none(),
                "{h} should be allowed"
            );
        }
        // A name that merely ends in "local" as a substring is not mDNS —
        // only the `.local` suffix is.
        assert!(public_fetch_target_refusal("localhost.evil.com", false).is_none());
    }

    // -- AT+AGENTCFG -------------------------------------------------------

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    fn agentcfg_args() -> [&'static [u8]; 8] {
        [b"node-1", b"20", b"524288", b"10", b"30", b"1", b"10", b"0"]
    }

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    #[test]
    fn agentcfg_accepts_the_documented_eight_field_form() {
        let cfg = validate_agentcfg_set(&cmd_token(AtOp::AgentCfg, &agentcfg_args()))
            .expect("valid form must be accepted");
        assert_eq!(cfg.name.as_str(), "node-1");
        assert_eq!(cfg.max_iterations, 20);
        assert_eq!(cfg.max_memory, 524288);
        assert_eq!(cfg.watchdog_timeout_secs, 10);
        assert_eq!(cfg.ble_timeout_secs, 30);
        assert!(cfg.skills_enabled);
        assert_eq!(cfg.max_skills, 10);
        assert!(!cfg.debug_enabled);
    }

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    #[test]
    fn agentcfg_rejects_a_partial_form() {
        let args = agentcfg_args();
        for n in 0..8 {
            let err = validate_agentcfg_set(&cmd_token(AtOp::AgentCfg, &args[..n]))
                .expect_err("a partial form must be rejected");
            assert_eq!(err, AtOutcome::error(4), "{n} fields must be a usage error");
        }
    }

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    #[test]
    fn agentcfg_rejects_values_agent_config_would_refuse() {
        // Zero iterations / memory / watchdog / skills are numeric range
        // failures (`:7`); a boolean that is not 0/1 is a malformed argument
        // (`:4`). Neither may be stored.
        for (idx, bad, expected_code) in [
            (1, b"0".as_slice(), 7), // max_iterations
            (2, b"0".as_slice(), 7), // max_memory
            (3, b"0".as_slice(), 7), // watchdog
            (4, b"0".as_slice(), 7), // BLE timeout
            (6, b"0".as_slice(), 7), // max_skills
            (5, b"2".as_slice(), 4), // skills_enabled
            (7, b"2".as_slice(), 4), // debug_enabled
        ] {
            let mut args = agentcfg_args();
            args[idx] = bad;
            let err = validate_agentcfg_set(&cmd_token(AtOp::AgentCfg, &args))
                .expect_err("out-of-range value must be rejected");
            assert_eq!(err, AtOutcome::error(expected_code), "field {idx}");
        }
    }

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    #[test]
    fn agentcfg_rejects_a_comma_in_the_name() {
        // A quoted name may legally contain a comma at the AT syntax layer,
        // but the query reply is comma-separated and unquoted — storing one
        // would make `AT+AGENTCFG?` unparseable as an eight-field line.
        let mut args = agentcfg_args();
        args[0] = b"node,1";
        let err = validate_agentcfg_set(&cmd_token(AtOp::AgentCfg, &args))
            .expect_err("a comma in the name must be rejected");
        assert_eq!(err, AtOutcome::error(4));
    }

    #[cfg(any(
        feature = "nrf52",
        feature = "esp32",
        feature = "embedded",
        feature = "std"
    ))]
    #[test]
    fn agentcfg_rejects_a_non_numeric_field() {
        let mut args = agentcfg_args();
        args[1] = b"twenty";
        let err = validate_agentcfg_set(&cmd_token(AtOp::AgentCfg, &args))
            .expect_err("non-numeric iterations must be rejected");
        assert_eq!(err, AtOutcome::error(4));
    }

    #[test]
    fn agentcfg_reset_sentinel_is_exactly_one_reset_token() {
        assert!(agentcfg_is_reset(&cmd_token(AtOp::AgentCfg, &[b"RESET"])));
        assert!(agentcfg_is_reset(&cmd_token(AtOp::AgentCfg, &[b"reset"])));
        // A name literally spelled "RESET" plus the other fields is a *set*,
        // not a reset — the sentinel requires exactly one argument.
        assert!(!agentcfg_is_reset(&cmd_token(
            AtOp::AgentCfg,
            &[b"RESET", b"20"]
        )));
        assert!(!agentcfg_is_reset(&cmd_token(
            AtOp::AgentCfg,
            &[b"RESETME"]
        )));
    }
}
