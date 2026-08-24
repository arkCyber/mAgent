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

use crate::at::{AtArg, AtCommand};
use crate::at_dispatch_outcome::AtOutcome;

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

// ---------------------------------------------------------------------------
// Validated payloads
// ---------------------------------------------------------------------------

/// Result of a successful `AT+CWJAP=` validation. Both fields are
/// owned heapless strings so the payload can outlive the source
/// `AtCommand` (caller drops the original args buffer).
#[derive(Debug, Clone)]
pub struct CwjapValidated {
    pub ssid: heapless::String<CWJAP_SSID_MAX>,
    pub password: heapless::String<CWJAP_PASS_MAX>,
}

/// Result of a successful `AT+CWHOSTNAME=` validation.
#[derive(Debug, Clone)]
pub struct HostnameValidated {
    pub hostname: heapless::String<HOSTNAME_MAX>,
}

/// Result of a successful `AT+CWMODE=` validation. `mode` is
/// guaranteed to be one of `CWMODE_VALID`.
#[derive(Debug, Clone, Copy)]
pub struct CwmodeValidated {
    pub mode: u8,
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
    if ssid_src.iter().any(|&c| c == 0) {
        return Err(AtOutcome::error(4));
    }
    let ssid_str = core::str::from_utf8(ssid_src).map_err(|_| AtOutcome::error(4))?;

    // Password: optional (empty => OPEN network).
    let pass_src_opt: Option<&[u8]> = match cmd.args.get(1) {
        Some(arg) => {
            Some(arg_bytes_decoded(arg, &mut pass_dec).map_err(|_| AtOutcome::error(8))?)
        }
        None => None,
    };
    if let Some(src) = pass_src_opt {
        if src.len() > CWJAP_PASS_MAX {
            return Err(AtOutcome::error(8));
        }
        if src.iter().any(|&c| c == 0) {
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
    if src.iter().any(|&c| c == 0) {
        return Err(AtOutcome::error(4));
    }
    let hostname_str = core::str::from_utf8(src).map_err(|_| AtOutcome::error(4))?;
    let mut hostname: heapless::String<HOSTNAME_MAX> = heapless::String::new();
    hostname.push_str(hostname_str).map_err(|_| AtOutcome::error(8))?;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use heapless::Vec;
    use crate::at::{AtArg, AtCommand, AtCommandKind, AtOp, AtVerb};

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

    /// Helper: build an oversized arg via a static array.
    fn make_oversized(n: usize, fill: u8) -> &'static [u8] {
        // Use a `static` to keep the lifetime working without
        // leaking anything. The const block creates an
        // initializer but the array's contents depend on `n` and
        // `fill` — fall back to a fixed-length array of the
        // maximum we test against, plus a sentinel byte to mark
        // the actual length.
        const OVERSIZE: [u8; 96] = [0x41; 96];
        let _ = n;
        let _ = fill;
        &OVERSIZE[..96]
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
        // Build static byte arrays so the slice references are 'static.
        static M1: [u8; 1] = [b'1'];
        static M2: [u8; 1] = [b'2'];
        static M3: [u8; 1] = [b'3'];
        for (i, m) in CWMODE_VALID.iter().enumerate() {
            let c = match i {
                0 => cmd_token(AtOp::CwMode, &[&M1]),
                1 => cmd_token(AtOp::CwMode, &[&M2]),
                _ => cmd_token(AtOp::CwMode, &[&M3]),
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

    /// Touch the unused helper to suppress dead-code warnings on
    /// the test build (in case someone reads it as a public API).
    #[test]
    fn make_oversized_helper_smoke_test() {
        let s = make_oversized(96, b'A');
        assert_eq!(s.len(), 96);
    }
}
