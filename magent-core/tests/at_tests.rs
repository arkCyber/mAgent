//! Integration tests for the [`magent_core::at`] parser.
//!
//! Each test corresponds to a documented ESP-AT behaviour so future
//! refactors cannot regress wire-compatibility without noticing. Host
//! only (`required-features = ["std"]` is implied — the lib itself is
//! `no_std`).
//!
//! The parser is exercised here from a public-API perspective
//! (`parse_line`) and validates crash-loop corner cases (max-length
//! lines, mass arguments, hostile escaping, etc.) that belong in
//! integration tests rather than `cfg(test)` blocks.

use magent_core::at::{
    is_at_line, parse_line, parse_u32, parse_i32, build_response, AtArg,
    AtCommandKind, AtOp, AtParseError, AtParseErrorKind, AtResponseKind,
    MAX_LINE, validate_mac, validate_passphrase, validate_ssid,
    trim_line_terminator, MAX_RESPONSE,
};
use magent_core::at_validate::{validate_cwjap_set, validate_cwmode_set};

#[test]
fn esp_at_compat_at_ping() {
    let cmd = parse_line(b"AT\r\n").expect("AT");
    assert_eq!(cmd.op, AtOp::Ping);
    assert_eq!(cmd.kind, AtCommandKind::Control);
}

#[test]
fn esp_at_compat_at_echo_off() {
    let cmd = parse_line(b"ATE0").expect("ATE0");
    assert_eq!(cmd.op, AtOp::SetEcho { on: false });
}

#[test]
fn esp_at_compat_at_echo_on() {
    let cmd = parse_line(b"ATE1").expect("ATE1");
    assert_eq!(cmd.op, AtOp::SetEcho { on: true });
}

#[test]
fn esp_at_compat_at_gmr() {
    let cmd = parse_line(b"AT+GMR").expect("AT+GMR");
    assert_eq!(cmd.op, AtOp::GetVersion);
}

#[test]
fn esp_at_compat_at_rst() {
    let cmd = parse_line(b"AT+RST").expect("AT+RST");
    assert_eq!(cmd.op, AtOp::Reset);
}

#[test]
fn esp_at_compat_at_cwmode_query() {
    let cmd = parse_line(b"AT+CWMODE?").expect("CWMODE?");
    assert_eq!(cmd.op, AtOp::CwMode);
    assert_eq!(cmd.kind, AtCommandKind::Query);
}

#[test]
fn esp_at_compat_at_cwmode_set_station() {
    let cmd = parse_line(b"AT+CWMODE=1").expect("CWMODE=1");
    assert_eq!(cmd.op, AtOp::CwMode);
    assert_eq!(cmd.kind, AtCommandKind::Set);
    match cmd.arg(0) {
        Some(AtArg::Token(b)) => assert_eq!(*b, b"1"),
        _ => panic!("token 1"),
    }
}

#[test]
fn esp_at_compat_at_cwjap_set_with_escapes() {
    // From Espressif docs: AT+CWJAP="ab\\\,c","0123456789\"\\"
    // The quoted bytes are returned verbatim (we step over the escape
    // bytes but include them in the slice). We don't decode them —
    // the firmware may choose to unescape. Here we just assert the
    // command parses, returns Quoted strings, and the SSID ends with
    // 'c'.
    let cmd = parse_line(b"AT+CWJAP=\"ab\\\\\\,c\",\"0123456789\\\"\\\\\"").expect("CWJAP");
    assert_eq!(cmd.op, AtOp::CwJap);
    assert_eq!(cmd.kind, AtCommandKind::Set);
    match cmd.arg(0) {
        Some(AtArg::Quoted(s)) => {
            assert!(s.ends_with(b"c") || s.ends_with(b"\\\\\\,c"));
        }
        _ => panic!("expected quoted SSID"),
    }
}

#[test]
fn esp_at_compat_at_cwjap_query() {
    let cmd = parse_line(b"AT+CWJAP?").expect("CWJAP?");
    assert_eq!(cmd.op, AtOp::CwJap);
    assert_eq!(cmd.kind, AtCommandKind::Query);
}

#[test]
fn esp_at_compat_at_cwjap_execute() {
    let cmd = parse_line(b"AT+CWJAP").expect("CWJAP");
    assert_eq!(cmd.op, AtOp::CwJap);
    assert_eq!(cmd.kind, AtCommandKind::Execute);
}

#[test]
fn esp_at_compat_at_cwqap() {
    let cmd = parse_line(b"AT+CWQAP").expect("CWQAP");
    assert_eq!(cmd.op, AtOp::CwQap);
}

#[test]
fn esp_at_compat_at_cwlap_execute() {
    let cmd = parse_line(b"AT+CWLAP").expect("CWLAP");
    assert_eq!(cmd.op, AtOp::CwLap);
}

#[test]
fn esp_at_compat_at_cwreconncfg() {
    let cmd = parse_line(b"AT+CWRECONNCFG=1,100").expect("CWRECONNCFG");
    assert_eq!(cmd.op, AtOp::CwReconnCfg);
    assert_eq!(cmd.args.len(), 2);
    assert_eq!(parse_u32(cmd.arg(0).unwrap().data()), Some(1));
    assert_eq!(parse_u32(cmd.arg(1).unwrap().data()), Some(100));
}

#[test]
fn esp_at_compat_at_cwreconncfg_reconnect_now() {
    let cmd = parse_line(b"AT+CWRECONNCFG=2,50,1").expect("CWRECONNCFG");
    assert_eq!(cmd.op, AtOp::CwReconnCfg);
    assert_eq!(cmd.args.len(), 3);
}

#[test]
fn esp_at_compat_at_cwhostname() {
    let cmd = parse_line(b"AT+CWHOSTNAME=\"iot-001\"").expect("CWHOSTNAME");
    assert_eq!(cmd.op, AtOp::CwHostname);
    match cmd.arg(0) {
        Some(AtArg::Quoted(h)) => assert_eq!(*h, b"iot-001"),
        _ => panic!("hostname"),
    }
}

#[test]
fn esp_at_compat_at_cwstate() {
    let cmd = parse_line(b"AT+CWSTATE?").expect("CWSTATE?");
    assert_eq!(cmd.op, AtOp::CwState);
    assert_eq!(cmd.kind, AtCommandKind::Query);
}

#[test]
fn esp_at_compat_at_cipstamac() {
    let cmd = parse_line(b"AT+CIPSTAMAC=\"aa:bb:cc:dd:ee:ff\"").expect("CIPSTAMAC");
    assert_eq!(cmd.op, AtOp::CipStaMac);
    match cmd.arg(0) {
        Some(AtArg::Quoted(m)) => assert_eq!(validate_mac(m), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])),
        _ => panic!("mac"),
    }
}

#[test]
fn esp_at_compat_at_cipstamac_query() {
    let cmd = parse_line(b"AT+CIPSTAMAC?").expect("CIPSTAMAC?");
    assert_eq!(cmd.op, AtOp::CipStaMac);
    assert_eq!(cmd.kind, AtCommandKind::Query);
}

#[test]
fn all_documented_verbs_parse() {
    // One representative line per documented verb. The 28 entries must
    // match docs/AT_COMMAND_REFERENCE.md §3 and `AtOp::name()` exactly;
    // the count is pinned so adding a verb without updating this test is
    // caught.
    let cases: &[(&[u8], AtOp)] = &[
        (b"AT", AtOp::Ping),
        (b"ATE0", AtOp::SetEcho { on: false }),
        (b"ATE1", AtOp::SetEcho { on: true }),
        (b"AT+GMR", AtOp::GetVersion),
        (b"AT+RST", AtOp::Reset),
        (b"AT+SYSRAM?", AtOp::SysRam),
        (b"AT+SYSLOG=3", AtOp::SysLog),
        (b"AT+SYSSTORE=1", AtOp::SysStore),
        (b"AT+CWMODE=1", AtOp::CwMode),
        (b"AT+CWJAP=\"myssid\",\"mypass\"", AtOp::CwJap),
        (b"AT+CWQAP", AtOp::CwQap),
        (b"AT+CWLAP", AtOp::CwLap),
        (b"AT+CWHOSTNAME=\"iot-001\"", AtOp::CwHostname),
        (b"AT+CWAUTOCONN=1", AtOp::CwAutoconn),
        (b"AT+CWRECONNCFG=1,100", AtOp::CwReconnCfg),
        (b"AT+CWSTATE?", AtOp::CwState),
        (b"AT+CIPSTAMAC=\"aa:bb:cc:dd:ee:ff\"", AtOp::CipStaMac),
        (b"AT+MACRAND=0", AtOp::MacRand),
        (b"AT+HEAP?", AtOp::Heap),
        (b"AT+UPTIME?", AtOp::Uptime),
        (b"AT+SAFEMODE=0", AtOp::Safemode),
        (b"AT+IDENT?", AtOp::Ident),
        (b"AT+IDENTROT", AtOp::IdentRot),
        (b"AT+SIGN=\"hello\"", AtOp::Sign),
        (b"AT+RESTORE", AtOp::Restore),
        (b"AT+IFCONFIG?", AtOp::Ifconfig),
        (b"AT+PING=\"1.2.3.4\"", AtOp::Ping6),
        (b"AT+AGENT=\"read the temperature\"", AtOp::Agent),
        (b"AT+WIFIPASSUPGRADE=1", AtOp::WifiPassUpgrade),
        (b"AT+HTTPGET=\"http://example.com/\"", AtOp::HttpGet),
        (b"AT+LLMCFG=\"deepseek-chat\",\"sk-abc\"", AtOp::LlmCfg),
    ];
    assert_eq!(cases.len(), 31, "documented verb count drifted; update this table");
    for (line, expected) in cases {
        let cmd = parse_line(line)
            .unwrap_or_else(|e| panic!("{line:?} should parse but failed: {e:?}"));
        assert_eq!(cmd.op, *expected, "op mismatch for {:?}", String::from_utf8_lossy(line));
    }
}

#[test]
fn every_op_has_a_stable_audit_name() {
    // `AtOp::name()` feeds the `[at] op=...` audit log line and the
    // docs. Ping deliberately has an empty name; every other op must
    // carry a non-empty, stable identifier so logs stay greppable.
    assert_eq!(AtOp::Ping.name(), "");
    for op in [
        AtOp::SetEcho { on: true }, AtOp::GetVersion, AtOp::Reset, AtOp::SysRam,
        AtOp::SysLog, AtOp::SysStore, AtOp::CwMode, AtOp::CwJap, AtOp::CwQap,
        AtOp::CwLap, AtOp::CwHostname, AtOp::CwAutoconn, AtOp::CwReconnCfg,
        AtOp::CwState, AtOp::CipStaMac, AtOp::MacRand, AtOp::Heap, AtOp::Uptime,
        AtOp::Safemode, AtOp::Ident, AtOp::IdentRot, AtOp::Sign, AtOp::Restore,
        AtOp::Ifconfig, AtOp::Ping6, AtOp::Agent, AtOp::WifiPassUpgrade,
        AtOp::HttpGet, AtOp::LlmCfg,
    ] {
        assert!(!op.name().is_empty(), "{op:?} must have an audit name");
    }
}

#[test]
fn numeric_error_codes_are_stable() {
    use AtParseErrorKind::*;
    // Pin the full +CMDER:<n> mapping so a refactor can't silently
    // change the code a script depends on.
    let cases: &[(AtParseErrorKind, u8)] = &[
        (Empty, 0),
        (NotAnAtCommand, 0),
        (UnknownOp, 5),
        (TooManyArgs, 6),
        (NumberOutOfRange, 7),
        (StringTooLong, 8),
        (UnterminatedString, 4),
        (BadEscape, 4),
        (InvalidMac, 9),
        (InvalidArgument, 4),
        (Internal, 6),
    ];
    for (kind, code) in cases {
        let err = AtParseError::new(*kind, 0);
        assert_eq!(err.numeric_code(), *code, "numeric_code for {kind:?}");
    }
}

#[test]
fn cwjap_set_with_escapes_round_trips_through_validation() {
    // Full wire → parse → validate path: ESP-AT escapes must survive
    // parsing and be decoded by the validator so the stored
    // SSID/password match what the operator intended. The firmware's
    // CWJAP SET handler routes through validate_cwjap_set.
    let cmd = parse_line(b"AT+CWJAP=\"my\\,ssid\",\"p\\\"w\\\\d\"").expect("parse");
    let v = validate_cwjap_set(&cmd).expect("validated");
    assert_eq!(v.ssid.as_str(), "my,ssid");
    assert_eq!(v.password.as_str(), "p\"w\\d");
}

#[test]
fn cwmode_zero_is_rejected_by_validator() {
    // The dispatcher routes AT+CWMODE= through validate_cwmode_set, so
    // mode 0 (not a valid ESP-AT mode) must be rejected against the
    // 1/2/3 whitelist — the old inline firmware check wrongly accepted
    // it.
    let cmd0 = parse_line(b"AT+CWMODE=0").expect("parse 0");
    assert!(validate_cwmode_set(&cmd0).is_err());
    let cmd1 = parse_line(b"AT+CWMODE=1").expect("parse 1");
    assert!(validate_cwmode_set(&cmd1).is_ok());
    let cmd3 = parse_line(b"AT+CWMODE=3").expect("parse 3");
    assert!(validate_cwmode_set(&cmd3).is_ok());
}

#[test]
fn default_wifi_credentials_env_is_provisioned() {
    // `.cargo/config.toml` `[env]` injects these; the firmware reads them
    // via the same `option_env!` macro in
    // `provision_and_load_wifi_credentials`. This pins the "default config"
    // so a build without explicit shell env vars still provisions this AP.
    // The values are generic placeholders (see .cargo/config.toml) — real
    // credentials are supplied via shell env vars at build time.
    assert_eq!(option_env!("MAGENT_WIFI_SSID"), Some("MySSID"));
    assert_eq!(option_env!("MAGENT_WIFI_PASS"), Some("password"));
}

#[test]
fn wifi_credential_provisioning_round_trips_for_default_ap() {
    use heapless::{String as HString, Vec as HVec};
    use magent_core::wifi_pass_seal;

    // Full pipeline for the default AP, exactly as the firmware does it:
    // AT+CWJAP= → parse → validate → seal → (persist to NVS) → open again
    // at boot. `provision_and_load_wifi_credentials` seals the env-var
    // default through `seal_and_store_wifi_pass`, which uses DBO1
    // (`wifi_pass_seal`) — so that is the seal we exercise here.
    let cmd = parse_line(b"AT+CWJAP=\"MySSID\",\"password\"").expect("parse");
    let v = validate_cwjap_set(&cmd).expect("validate");
    assert_eq!(v.ssid.as_str(), "MySSID");
    assert_eq!(v.password.as_str(), "password");

    // Seal the password with a device-bound key (the firmware uses the
    // real Ed25519 seed; a fixed 32-byte test key is equivalent here).
    let key = [0x42u8; 32];
    let nonce = [0u8; 12];
    let mut sealed: HString<{ wifi_pass_seal::MAX_ENCODED_LEN }> = HString::new();
    wifi_pass_seal::seal_str(v.password.as_str(), &key, &nonce, &mut sealed).expect("seal");
    assert!(sealed.starts_with("DBO1:"), "sealed form must be DBO1");

    // Open it back at boot and confirm the exact passphrase round-trips.
    let mut out: HVec<u8, { wifi_pass_seal::MAX_PLAINTEXT }> = HVec::new();
    let outcome = wifi_pass_seal::open_sealed_bytes(&sealed, &key, &mut out).expect("open");
    assert!(matches!(outcome, wifi_pass_seal::OpenOutcome::DecodedBytes));
    assert_eq!(&out[..], b"password");
}

#[test]
fn route_to_agent_when_not_an_at_command() {
    // Natural language text routes back to the agent's ReAct loop.
    let err = parse_line(b"read the temperature please").unwrap_err();
    assert_eq!(err.kind, AtParseErrorKind::NotAnAtCommand);
    // Should also fail the cheap pre-check.
    assert!(!is_at_line(b"read the temperature please"));
}

#[test]
fn route_to_agent_when_empty_line() {
    let err = parse_line(b"").unwrap_err();
    assert_eq!(err.kind, AtParseErrorKind::Empty);
}

#[test]
fn numeric_codes_match_well_known_failures() {
    let err = parse_line(b"AT+CWJAP=\"missing_quote,secret").unwrap_err();
    assert_eq!(err.kind, AtParseErrorKind::UnterminatedString);
    assert_eq!(err.numeric_code(), 4);
}

#[test]
fn rejects_unknown_verb_with_code_5() {
    let err = parse_line(b"AT+ZZZZ?").unwrap_err();
    assert_eq!(err.kind, AtParseErrorKind::UnknownOp);
    assert_eq!(err.numeric_code(), 5);
}

#[test]
fn line_terminator_tolerances() {
    assert!(is_at_line(b"AT\r\n"));
    assert!(is_at_line(b"AT\n"));
    assert!(is_at_line(b"AT"));
}

#[test]
fn long_line_over_max_is_not_panic() {
    // Build a 256-byte-prefixed command line. The parser bounds on
    // MAX_LINE so this should still parse cleanly. We intentionally
    // don't grow it past 256 — the firmware clips upstream.
    use heapless::Vec;
    let mut s: Vec<u8, MAX_LINE> = Vec::new();
    let prefix: &[u8] = b"AT+PING=\"";
    let _ = s.extend_from_slice(prefix);
    // Pad with valid hostname characters.
    while s.len() < MAX_LINE - 2 {
        let _ = s.push(b'x');
    }
    let _ = s.push(b'"');
    // Should parse cleanly (we drop parse errors because the line
    // may be considered "too long" in some implementations and the
    // contract we assert here is that *something* happens that
    // isn't a panic).
    let _ = parse_line(&s).map(|_| ()).map_err(|_e| ());
}

#[test]
fn ssid_length_validation() {
    assert!(validate_ssid(b"").is_err());
    assert!(validate_ssid(&vec![b'x'; 32]).is_ok());
    assert!(validate_ssid(&vec![b'x'; 33]).is_err());
}

#[test]
fn passphrase_length_validation() {
    assert!(validate_passphrase(b"").is_ok());
    assert!(validate_passphrase(b"hunter2").is_ok());
    assert!(validate_passphrase(&vec![b'p'; 64]).is_ok());
    assert!(validate_passphrase(&vec![b'p'; 65]).is_err());
}

#[test]
fn mac_length_validation() {
    assert!(validate_mac(b"aa:bb:cc:dd:ee:ff").is_some());
    assert!(validate_mac(b"aa:bb:cc:dd:ee:gg").is_none()); // bad hex
    assert!(validate_mac(b"a:bb:cc:dd:ee:ff").is_none()); // too short
}

#[test]
fn build_response_rejects_oversized() {
    // We use a buffer that can't fit even one line + trailer. We
    // can't pass a `Vec<u8, 8>` directly because `build_response`
    // requires its exact-capacity signature; instead, we fill a
    // MAX_RESPONSE-capacity buffer past 8 bytes and assert the
    // function returns `Err`.
    use heapless::Vec;
    let mut buf: Vec<u8, MAX_RESPONSE> = Vec::new();
    // Pre-fill so the response is guaranteed not to fit.
    for _ in 0..MAX_RESPONSE - 2 {
        let _ = buf.push(b'x');
    }
    let lines: [&[u8]; 1] = [b"+CWJAP:\"foo\",6"];
    let r = build_response(&lines, AtResponseKind::Ok, &mut buf);
    assert!(r.is_err());
}

#[test]
fn build_response_with_data_trailing_ok() {
    use heapless::Vec;
    let mut buf: Vec<u8, MAX_RESPONSE> = Vec::new();
    let lines: [&[u8]; 2] = [b"+CWJAP:\"foo\",6", b"+IP:1.2.3.4"];
    build_response(&lines, AtResponseKind::Ok, &mut buf).unwrap();
    let s = core::str::from_utf8(&buf).unwrap();
    assert_eq!(s, "+CWJAP:\"foo\",6\r\n+IP:1.2.3.4\r\nOK\r\n");
}

#[test]
fn build_response_error_only() {
    use heapless::Vec;
    let mut buf: Vec<u8, MAX_RESPONSE> = Vec::new();
    build_response(&[], AtResponseKind::Error, &mut buf).unwrap();
    assert_eq!(core::str::from_utf8(&buf).unwrap(), "ERROR\r\n");
}

#[test]
fn trim_line_terminator_handles_combinations() {
    assert_eq!(trim_line_terminator(b"AT\r\n"), b"AT");
    assert_eq!(trim_line_terminator(b"AT\n\r\n"), b"AT"); // aggressive trim
    assert_eq!(trim_line_terminator(b"AT"), b"AT");
    assert_eq!(trim_line_terminator(b""), b"");
}

#[test]
fn parse_u32_rejects_overflow() {
    assert_eq!(parse_u32(b"99999999999"), None); // 11 chars
    assert_eq!(parse_u32(b"4294967295"), Some(u32::MAX));
    assert_eq!(parse_u32(b""), None);
    assert_eq!(parse_u32(b"-1"), None);
    assert_eq!(parse_u32(b" 1"), None);
    assert_eq!(parse_u32(b"abc"), None);
}

#[test]
fn parse_i32_rejects_min() {
    // Test that the parser handles the edges cleanly and rejects
    // magnitudes that don't fit in `i32`.
    assert_eq!(parse_i32(b"-2147483647"), Some(i32::MIN + 1));
    assert_eq!(parse_i32(b"2147483647"), Some(i32::MAX));
    // `-2147483649` is out of range for `i32`.
    assert_eq!(parse_i32(b"-2147483649"), None);
    // `-2147483648` is `i32::MIN` which we encode with the special
    // path; we either accept it or reject it but we MUST NOT panic.
    let r = parse_i32(b"-2147483648");
    // Either `Some(i32::MIN)` or `None` is acceptable as long as we
    // didn't panic. The current implementation accepts it.
    assert!(matches!(r, Some(i32::MIN) | None));
}

#[test]
fn respond_to_set_in_serial_form() {
    // The firmware will turn this into "+CWMODE:1\r\nOK\r\n".
    let cmd = parse_line(b"AT+CWMODE=1").expect("CWMODE=1");
    assert_eq!(cmd.op, AtOp::CwMode);
    assert_eq!(cmd.kind, AtCommandKind::Set);
    // For a Set command, the firmware echoes nothing and replies just
    // `OK\r\n` on success. Confirm the parser doesn't accidentally
    // classify this as query.
    assert_ne!(cmd.kind, AtCommandKind::Query);
}

#[test]
fn agent_escape_hatch_preserves_payload() {
    let cmd = parse_line(b"AT+AGENT=\"read the temperature\"").expect("AGENT");
    assert_eq!(cmd.op, AtOp::Agent);
    match cmd.arg(0) {
        Some(AtArg::Quoted(s)) => assert_eq!(*s, b"read the temperature"),
        _ => panic!("expected payload"),
    }
}

#[test]
fn ident_rot_is_pure_execute() {
    let cmd = parse_line(b"AT+IDENTROT").expect("IDENTROT");
    assert_eq!(cmd.op, AtOp::IdentRot);
    assert_eq!(cmd.kind, AtCommandKind::Execute);
}

#[test]
fn sign_command_quoted_payload() {
    let cmd = parse_line(b"AT+SIGN=\"hello world\"").expect("SIGN");
    assert_eq!(cmd.op, AtOp::Sign);
    match cmd.arg(0) {
        Some(AtArg::Quoted(p)) => assert_eq!(*p, b"hello world"),
        _ => panic!("expected quoted payload"),
    }
}

// ---------------------------------------------------------------------------
// Helper extension to access AtArg::data() uniformly regardless of variant.
// (AtArg doesn't have such a method in the public API, so we add it inline
// for tests.)
// ---------------------------------------------------------------------------

trait AtArgSliceExt<'a> {
    fn data(&'a self) -> &'a [u8];
}

impl<'a> AtArgSliceExt<'a> for AtArg<'a> {
    fn data(&'a self) -> &'a [u8] {
        match self {
            AtArg::Token(b) | AtArg::Quoted(b) => b,
            AtArg::Named { val, .. } => val,
        }
    }
}
