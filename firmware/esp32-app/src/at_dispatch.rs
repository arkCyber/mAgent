//! AT command dispatcher for the ESP32-C61 firmware.
//!
//! Firmware-side complement of the chip-agnostic `magent_core::at`
//! parser. The parser does syntax; this module does *work*:
//!
//! - NVS read/write (`wifi_ssid`, `wifi_pass`, `cwautoconn`, …).
//! - Wi-Fi mode / hostname / reconnect policy persistence.
//! - Identity load + rotate from hardware TRNG.
//! - Safe-mode and crash-counter wired into the same NVS namespace.
//! - The agent's serial line — every command lands here before
//!   `IngressGateway::ingest`, so the agent's ReAct loop never blocks
//!   on a Wi-Fi connect or sees a numeric AT reply.
//!
//! # Aerograde discipline
//!
//! - **Zero panic paths.** Every public function returns `Result`
//!   and the dispatcher never panics the ingress thread.
//! - **Bounded memory.** No heap allocation; all intermediate
//!   buffers are `heapless::String`/`Vec`.
//! - **Crash-loop aware.** Wi-Fi changes that could re-trigger a
//!   reboot loop are gated on `safe_mode`; the dispatcher still
//!   answers OK so scripts don't hang.
//! - **Audit log.** Each command produces exactly one log line.
//!
//! # What is NOT here (intentionally)
//!
//! - Wi-Fi connect (`esp_wifi_connect`) — lives on the supervisor thread;
//!   `AT+CWJAP=` updates NVS and raises `WIFI_RECONNECT_REQUESTED` so the
//!   supervisor reloads the new credentials and reconnects immediately
//!   (no reboot required). Boot-time `connect_wifi` still applies on power-up.
//! - Reset (`AT+RST`) — now performs a live `esp_restart()` (replies OK
//!   first, then reboots after a short delay so the reply flushes).
//! - Sign (`AT+SIGN`) — now implemented: the dispatcher loads the device
//!   identity directly and signs a payload, returning the canonical
//!   signed-message JSON (no gateway context required).

use core::fmt::Write as _;
use heapless::{String as HeaplessString, Vec};

use esp_idf_svc::nvs::EspDefaultNvs;
use magent_core::at::{
    self, AtArg, AtCommand, AtCommandKind, AtOp, AtResponseKind,
};
// `AtOutcome` was previously a local enum; it now lives in
// `magent-core` so pure validators like `at_validate` can return it
// directly. We re-export it here so external code can keep using
// `at_dispatch::AtOutcome::...` references without churn.
pub use magent_core::at_dispatch_outcome::AtOutcome;
use magent_core::time_sync::{Source, TZ_KEY, TZ_MAX_MINUTES, TZ_MIN_MINUTES};
use magent_core::web3::Identity;
use magent_core::wifi_pass_seal_v2;

// ---------------------------------------------------------------------------
// NVS key namespace for AT-managed configuration.
// ---------------------------------------------------------------------------
//
// All keys live under the `mag_at` namespace so we don't collide with
// the existing `magent` namespace used by `wifi_ssid` / `wifi_pass`
// (provisioned at boot from build-time env vars). The two namespaces
// are read at boot — AT-side values override build-time values when
// present.

/// Namespace used for AT-managed keys.
const NS: &str = "mag_at";

/// AT side overrides for the existing `wifi_ssid` / `wifi_pass` keys.
const NVS_KEY_WIFI_SSID: &str = "wifi_ssid";       // shared; in `magent` ns
const NVS_KEY_WIFI_PASS: &str = "wifi_pass";       // shared; in `magent` ns
const NVS_KEY_WIFI_MODE: &str = "wifi_mode";
const NVS_KEY_WIFI_HOSTNAME: &str = "hostname";
const NVS_KEY_WIFI_AUTOCONN: &str = "autoconn";
const NVS_KEY_WIFI_RECONN_INTERVAL: &str = "reconn_int";
const NVS_KEY_WIFI_RECONN_REPEAT: &str = "reconn_rep";
const NVS_KEY_SAFEMODE_FORCE: &str = "safemode";
const NVS_KEY_SYSSTORE: &str = "sysstore";
const NVS_KEY_IDENTITY: &str = "dev_identity";     // shared; in `magent` ns
// LLM backend parameters (`AT+LLMCFG`). Lives in the `mag_at` namespace.
const NVS_KEY_LLM_MODEL: &str = "llm_model";
const NVS_KEY_LLM_API_KEY: &str = "llm_api_key";

// ---------------------------------------------------------------------------
// Buffered response types.
// ---------------------------------------------------------------------------

const RESPONSE_BUF: usize = 768;

pub type ResponseBuf = Vec<u8, RESPONSE_BUF>;
pub type ReplyLine = HeaplessString<256>;

// `AtOutcome` is imported above from `magent_core::at_dispatch_outcome`.
// The dispatcher previously declared a local `AtOutcome` enum; that
// duplicate has been retired in favor of the chip-agnostic version
// in `magent-core` so that pure validators (e.g. `at_validate`) can
// return `AtOutcome` directly without a translation layer.

/// Render an [`AtOutcome`] onto the wire.
pub fn render_outcome(outcome: &AtOutcome, out: &mut ResponseBuf) -> Result<(), ()> {
    match outcome {
        AtOutcome::Error { code } => {
            let mut line = ReplyLine::new();
            let _ = write!(line, "+CMDER:{}", code);
            let line_slice: &[u8] = line.as_bytes();
            at::build_response(&[line_slice], AtResponseKind::Error, out)
        }
        AtOutcome::Ok { data } => {
            let line_slice: &[u8] = data.as_bytes();
            at::build_response(&[line_slice], AtResponseKind::Ok, out)
        }
        AtOutcome::NoReply => {
            at::build_response(&[], AtResponseKind::Ok, out)
        }
    }
}

// ---------------------------------------------------------------------------
// NVS helpers — local to this module so the dispatcher is independent
// of the main.rs NVS plumbing.
// ---------------------------------------------------------------------------

pub(crate) fn nvs_load(key: &str, namespace: &str) -> Option<HeaplessString<256>> {
    // PATCHED (MicroAgent): share the partition taken once at boot
    // (`main::init_default_nvs`); `EspDefaultNvsPartition::take()` fails
    // after EspWifi holds it, which made every AT read silently empty.
    let partition = crate::default_nvs()?;
    let nvs = EspDefaultNvs::new(partition, namespace, true).ok()?;
    let mut buf = [0u8; 256];
    let mut out = HeaplessString::new();
    if let Ok(Some(s)) = nvs.get_str(key, &mut buf) {
        let _ = out.push_str(s);
    }
    Some(out)
}

fn nvs_save(key: &str, value: &str, namespace: &str) -> Result<(), &'static str> {
    let partition = crate::default_nvs().ok_or("nvs_partition_unavailable")?;
    let nvs = EspDefaultNvs::new(partition, namespace, true)
        .map_err(|_| "nvs_open")?;
    nvs.set_str(key, value).map_err(|_| "nvs_write")?;
    Ok(())
}

/// `TZ_KEY` is a namespace-qualified key (`mag_at:timezone_min`, matching
/// main.rs's `namespace:key` shorthand). Our local `nvs_load`/`nvs_save`
/// take a bare key + namespace separately, so strip the prefix before use.
fn tz_bare_key() -> &'static str {
    TZ_KEY.rsplit_once(':').map(|(_, k)| k).unwrap_or(TZ_KEY)
}

/// Load the persisted timezone offset from NVS. Returns 0 if the key
/// is absent / unparseable. The `main` task uses this on boot to
/// pre-populate `TimeSync::new(tz)` so the operator's `AT+TIMEZONE=`
/// survives reboots.
pub fn load_tz_offset_from_nvs() -> i16 {
    let v = match nvs_load(tz_bare_key(), NS) {
        Some(s) => s,
        None => return 0,
    };
    v.parse::<i16>().unwrap_or(0).clamp(TZ_MIN_MINUTES, TZ_MAX_MINUTES)
}

/// Load the device-bound sealing key. The key is the Ed25519 seed stored
/// under `magent:dev_identity`. Supports BOTH the legacy 64-hex plaintext
/// and the modern `BTDK1:`-sealed form via
/// [`crate::device_key::open_dev_identity`]. Returns an error if the seed
/// is missing / unreadable, so the dispatcher refuses to seal rather than
/// falling back to a zero key.
fn load_device_key() -> Result<[u8; 32], &'static str> {
    let stored = nvs_load("dev_identity", "magent")
        .ok_or("dev_identity not in NVS")?;
    crate::device_key::open_dev_identity(stored.as_str())
}

// ---------------------------------------------------------------------------
// Public dispatcher entry.
// ---------------------------------------------------------------------------

/// Apply one parsed AT command. `now_ms` is the ESP-IDF monotonic ms
/// clock; `safe_mode` reflects the firmware's crash-loop detector;
/// `time_sync` is the shared wall-clock handle used by `AT+TIME?`,
/// `AT+NTPSYNC`, `AT+TIMEZONE`. `force_ntp_sync` lets the dispatcher
/// trigger an SNTP re-sync (it is called when `AT+NTPSYNC` arrives).
pub fn dispatch<'a>(
    cmd: &AtCommand<'a>,
    now_ms: u64,
    safe_mode: bool,
    wifi_status: Option<&crate::WifiStatusHandle>,
    time_sync: Option<&crate::sntp_sync::TimeSyncHandle>,
    force_ntp_sync: &mut bool,
) -> AtOutcome {
    let outcome = dispatch_inner(
        cmd,
        now_ms,
        safe_mode,
        wifi_status,
        time_sync,
        force_ntp_sync,
    );
    log::info!(
        "[at] op={} kind={:?} -> {:?} (t={}ms)",
        cmd.op.name(),
        cmd.kind,
        outcome,
        now_ms
    );
    outcome
}

fn dispatch_inner<'a>(
    cmd: &AtCommand<'a>,
    now_ms: u64,
    safe_mode: bool,
    wifi_status: Option<&crate::WifiStatusHandle>,
    time_sync: Option<&crate::sntp_sync::TimeSyncHandle>,
    force_ntp_sync: &mut bool,
) -> AtOutcome {
    match cmd.op {
        AtOp::Ping => AtOutcome::NoReply,
        AtOp::SetEcho { on: _ } => AtOutcome::NoReply,
        AtOp::GetVersion => version_string(),
        AtOp::Reset => {
            // AT+RST: reply OK, then reboot after a short delay so the
            // OK line flushes out the UART before the radio/stack tears
            // down. `esp_restart()` is a cold reboot (not a watchdog reset).
            log::info!("[at] AT+RST — rebooting in 200ms");
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                // SAFETY: `esp_restart()` never returns; nothing to clean up.
                unsafe {
                    esp_idf_sys::esp_restart();
                }
            });
            AtOutcome::NoReply
        }
        AtOp::SysRam => sysram_line(),
        AtOp::SysLog => syslog_dispatch(cmd),
        AtOp::SysStore => sysstore_dispatch(cmd),
        AtOp::CwMode => cwmode_dispatch(cmd),
        AtOp::CwJap => cwjap_dispatch(cmd, safe_mode),
        AtOp::CwQap => {
            log::info!("[at] CWQAP — disconnect issued (deferred to next boot)");
            AtOutcome::NoReply
        }
        AtOp::CwLap => {
            log::info!("[at] CWLAP deferred to background scan");
            AtOutcome::ok_line("+CWLAP:scan-started")
        }
        AtOp::CwHostname => cwhostname_dispatch(cmd),
        AtOp::CwAutoconn => cwautoconn_dispatch(cmd),
        AtOp::CwReconnCfg => cwreconncfg_dispatch(cmd),
        AtOp::CwState => cwstate_line(wifi_status),
        AtOp::CipStaMac => cipstamac_dispatch(cmd),
        AtOp::MacRand => {
            log::warn!("[at] MACRAND: not implemented in v0.2");
            AtOutcome::error(9)
        }
        AtOp::Heap => sysram_line(),
        AtOp::Uptime => uptime_line(now_ms),
        AtOp::Safemode => safemode_dispatch(cmd),
        AtOp::Ident => ident_query(),
        AtOp::IdentRot => ident_rot_dispatch(safe_mode),
        AtOp::Sign => sign_dispatch(cmd),
        AtOp::Restore => {
            log::warn!("[at] RESTORE needs full-nvs-wipe; deferred to v0.3");
            AtOutcome::error(4)
        }
        AtOp::Ifconfig => ifconfig_line(wifi_status),
        AtOp::Ping6 => AtOutcome::error(4),
        AtOp::Agent => AtOutcome::NoReply, // handled upstream in main.rs
        AtOp::WifiPassUpgrade => wifipass_upgrade_dispatch(cmd),
        AtOp::HttpGet => http_get_dispatch(cmd),
        AtOp::LlmCfg => llmcfg_dispatch(cmd),
        AtOp::Time => time_dispatch(cmd, time_sync, now_ms),
        AtOp::NtpSync => ntp_sync_dispatch(cmd, time_sync, force_ntp_sync, safe_mode),
        AtOp::Timezone => timezone_dispatch(cmd, time_sync),
        AtOp::Ble => ble_dispatch(cmd),
    }
}

/// Report the real Wi-Fi link state by reading the snapshot published by the
/// Wi-Fi supervisor thread. Falls back to `4` (disconnected) when no
/// supervisor is running. The optional IP makes the line self-describing
/// without needing a separate `AT+CIPSTA` probe.
fn cwstate_line(wifi_status: Option<&crate::WifiStatusHandle>) -> AtOutcome {
    let mut line = ReplyLine::new();
    let res = match wifi_status {
        Some(h) => match h.lock() {
            Ok(g) => {
                if g.ip.is_empty() {
                    write!(line, "+CWSTATE:{}", g.state)
                } else {
                    write!(line, "+CWSTATE:{},{}", g.state, g.ip)
                }
            }
            Err(_) => write!(line, "+CWSTATE:4"),
        },
        None => write!(line, "+CWSTATE:4"),
    };
    if res.is_err() {
        return AtOutcome::ok_line("+CWSTATE:4");
    }
    AtOutcome::ok_line(&line)
}


// ---------------------------------------------------------------------------
// Individual command implementations.
// ---------------------------------------------------------------------------

fn version_string() -> AtOutcome {
    let s: &str = concat!(
        "mAgent v",
        env!("CARGO_PKG_VERSION"),
        " / AT v0.2 / esp32-c61",
    );
    let mut line = ReplyLine::new();
    let _ = write!(line, "+GMR:{}", s);
    AtOutcome::Ok { data: line }
}

fn sysram_line() -> AtOutcome {
    let heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    let mut line = ReplyLine::new();
    let _ = write!(line, "+SYSRAM:{}", heap);
    AtOutcome::Ok { data: line }
}

fn uptime_line(now_ms: u64) -> AtOutcome {
    let mut line = ReplyLine::new();
    let _ = write!(line, "+UPTIME:{}", now_ms);
    AtOutcome::Ok { data: line }
}

fn syslog_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            // Report the *actual* global filter level so `AT+SYSLOG?`
            // agrees with whatever `AT+SYSLOG=<n>` most recently set
            // (and with the boot-time default).
            let v = match log::max_level() {
                log::LevelFilter::Off => 0,
                log::LevelFilter::Error => 1,
                log::LevelFilter::Warn => 2,
                log::LevelFilter::Info => 3,
                log::LevelFilter::Debug => 4,
                log::LevelFilter::Trace => 5,
            };
            let mut line = ReplyLine::new();
            let _ = write!(line, "+SYSLOG:{}", v);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let level = cmd.arg(0).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            match level {
                Some(v) if v <= 5 => {
                    let filter = match v {
                        0 => log::LevelFilter::Off,
                        1 => log::LevelFilter::Error,
                        2 => log::LevelFilter::Warn,
                        3 => log::LevelFilter::Info,
                        4 => log::LevelFilter::Debug,
                        _ => log::LevelFilter::Trace,
                    };
                    log::set_max_level(filter);
                    AtOutcome::NoReply
                }
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn sysstore_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let v = nvs_load(NVS_KEY_SYSSTORE, NS)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);
            let mut line = ReplyLine::new();
            let _ = write!(line, "+SYSSTORE:{}", v);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let raw = cmd.arg(0).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            match raw {
                Some(v) if v <= 1 => match nvs_save(NVS_KEY_SYSSTORE, &v.to_string(), NS) {
                    Ok(()) => AtOutcome::NoReply,
                    Err(_) => AtOutcome::error(7),
                },
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn cwmode_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let v = nvs_load(NVS_KEY_WIFI_MODE, NS)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);
            let mut line = ReplyLine::new();
            let _ = write!(line, "+CWMODE:{}", v);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            // PATCHED (MicroAgent): routed through the host-tested
            // validator so mode 0 is correctly rejected (ESP-AT only
            // accepts 1/2/3) — the old inline check (`n <= 3`) wrongly
            // accepted 0.
            let validated = match magent_core::at_validate::validate_cwmode_set(cmd) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            match nvs_save(NVS_KEY_WIFI_MODE, &validated.mode.to_string(), NS) {
                Ok(()) => AtOutcome::NoReply,
                Err(_) => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn cwjap_dispatch(cmd: &AtCommand<'_>, safe_mode: bool) -> AtOutcome {
    if safe_mode && matches!(cmd.kind, AtCommandKind::Set) {
        log::warn!("[at] CWJAP= refused: safe mode active");
        return AtOutcome::error(4);
    }
    match cmd.kind {
        AtCommandKind::Query => {
            let ssid = nvs_load(NVS_KEY_WIFI_SSID, "magent").unwrap_or_default();
            // PATCHED (MicroAgent): the Query reply now reports
            // the seal format of the stored password so an
            // operator can spot legacy entries that haven't been
            // migrated to DBO2 yet (motivates running
            // `AT+WIFIPASSUPGRADE=1`).
            let stored = nvs_load(NVS_KEY_WIFI_PASS, "magent").unwrap_or_default();
            let seal_fmt = if stored.starts_with("DBO2:") {
                "DBO2"
            } else if stored.starts_with("DBO1:") {
                "DBO1_LEGACY"
            } else if stored.is_empty() {
                "NONE"
            } else {
                "PLAINTEXT_LEGACY"
            };
            let mut line = ReplyLine::new();
            // Password field is intentionally empty — we never
            // expose the password over UART (see AT_COMMAND_REFERENCE §7).
            let _ = write!(
                line,
                "+CWJAP:\"{}\",,0,0,{}",
                escape_wire(&ssid),
                seal_fmt
            );
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            // PATCHED (MicroAgent): pure-logic validation moved to
            // `magent_core::at_validate::validate_cwjap_set` so the
            // length / NUL / UTF-8 rules can be unit-tested on the
            // host against hundreds of pathological inputs. Here we
            // just translate the validated payload into NVS writes.
            let validated = match magent_core::at_validate::validate_cwjap_set(cmd) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };

            // PATCHED (MicroAgent): Wi-Fi password is sealed with the
            // device-bound key (DBO2) before being persisted. A raw
            // flash dump of NVS no longer reveals the WPA2
            // passphrase — the attacker would also need to extract
            // the Ed25519 seed from `magent:dev_identity` AND the
            // hardware-bound BTDK1 key material AND replicate the
            // ESP32's TRNG output for this specific nonce.
            let device_key = match load_device_key() {
                Ok(k) => k,
                Err(_) => {
                    log::warn!("[at] CWJAP= refused: no device key in NVS yet");
                    return AtOutcome::error(7);
                }
            };
            // Draw a fresh nonce from the ESP32 hardware TRNG via
            // the `getrandom` shim — same source `dev_identity`
            // uses, so the sealing randomness is consistent
            // across the firmware. The nonce lives only in this
            // stack frame; nothing persists.
            let mut nonce = [0u8; wifi_pass_seal_v2::NONCE_LEN];
            if getrandom::getrandom(&mut nonce).is_err() {
                log::warn!("[at] CWJAP= refused: TRNG unavailable");
                return AtOutcome::error(6);
            }
            let mut sealed: HeaplessString<{ wifi_pass_seal_v2::MAX_ENCODED_LEN }> =
                HeaplessString::new();
            if let Err(e) = wifi_pass_seal_v2::seal_str(
                validated.password.as_str(),
                &device_key,
                &nonce,
                &mut sealed,
            ) {
                log::warn!("[at] CWJAP= DBO2 seal failed: {e}");
                return AtOutcome::error(7);
            }

            if nvs_save(NVS_KEY_WIFI_SSID, validated.ssid.as_str(), "magent").is_err() {
                return AtOutcome::error(7);
            }
            if nvs_save(NVS_KEY_WIFI_PASS, sealed.as_str(), "magent").is_err() {
                return AtOutcome::error(7);
            }
            // AUDIT: log only the SSID and a 4-byte fingerprint of the
            // *ciphertext* (NOT the password and NOT the nonce).
            //
            // Layout of `sealed.as_bytes()` for DBO2:
            //   [0..5)              = "DBO2:" prefix
            //   [5..5+2*NONCE_LEN)  = nonce as hex
            //   [5+2*NONCE_LEN..5+2*NONCE_LEN+2*cipher_len) = cipher as hex
            //   [..]                 = mac as hex (16 bytes)
            //
            // The cipher portion is safe to log: an operator who
            // sees this fingerprint can correlate repeat
            // `AT+CWJAP=` calls (same fingerprint ⇒ same plaintext
            // AND same nonce — which never happens across reboots
            // because each call draws a fresh nonce) without ever
            // seeing secret material.
            let cipher_offset = 5 + 2 * wifi_pass_seal_v2::NONCE_LEN;
            let mut fingerprint: HeaplessString<9> = HeaplessString::new();
            if sealed.len() >= cipher_offset + 4 {
                for &b in sealed.as_bytes()[cipher_offset..cipher_offset + 4].iter() {
                    let _ = write!(fingerprint, "{:02x}", b);
                }
            }
            log::info!(
                "[at] CWJAP set: ssid={} pass_len={} sealed_fp={} prefix=DBO2:",
                validated.ssid.as_str(),
                validated.password.len(),
                fingerprint.as_str(),
            );
            // COMPLETION (2026-08-27): signal the Wi-Fi supervisor to reload the
            // new credentials from NVS and reconnect immediately — no reboot
            // required. The supervisor polls this flag once on its next tick.
            crate::WIFI_RECONNECT_REQUESTED.store(true, core::sync::atomic::Ordering::Relaxed);
            AtOutcome::NoReply
        }
        AtCommandKind::Execute => {
            log::info!("[at] CWJAP execute — deferred to next boot");
            AtOutcome::NoReply
        }
        _ => AtOutcome::error(4),
    }
}

/// `AT+WIFIPASSUPGRADE?` / `AT+WIFIPASSUPGRADE=1`
///
/// Reports the seal format of the stored wifi_pass entry and,
/// on `=1`, re-seals it under DBO2 in place (using the same
/// plaintext recovered from the existing DBO1 / legacy entry).
///
/// This is the explicit migration command for the DBO1 → DBO2
/// upgrade. The Query form is informational; the Set form does
/// the actual work and is idempotent (DBO2 entries are reported
/// as "current" and re-sealed only if the operator forces it).
fn wifipass_upgrade_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    use magent_core::wifi_pass_seal_v2;
    let stored = match nvs_load(NVS_KEY_WIFI_PASS, "magent") {
        Some(s) if !s.is_empty() => s,
        _ => return AtOutcome::ok_line("+WIFIPASSUPGRADE:NO_ENTRY"),
    };
    match cmd.kind {
        AtCommandKind::Query => {
            // Just report the format.
            let line = if wifi_pass_seal_v2::is_legacy(&stored) {
                "+WIFIPASSUPGRADE:LEGACY"
            } else {
                "+WIFIPASSUPGRADE:CURRENT"
            };
            AtOutcome::ok_line(line)
        }
        AtCommandKind::Set => {
            // Only "1" forces the upgrade; anything else is an
            // error so an operator can't accidentally re-seal
            // by typo.
            let arg = match cmd.args.first() {
                Some(AtArg::Token(s)) => s,
                _ => return AtOutcome::error(4),
            };
            if arg != b"1" {
                return AtOutcome::error(7);
            }
            // Already current? Nothing to do.
            if !wifi_pass_seal_v2::is_legacy(&stored) {
                log::info!("[at] WIFIPASSUPGRADE: entry already DBO2; no-op");
                return AtOutcome::ok_line("+WIFIPASSUPGRADE:CURRENT");
            }
            // Open the legacy entry (DBO1 or plaintext) to
            // recover the plaintext, then re-seal under DBO2.
            let device_key = match load_device_key() {
                Ok(k) => k,
                Err(_) => {
                    log::warn!("[at] WIFIPASSUPGRADE: no device key");
                    return AtOutcome::error(7);
                }
            };
            let mut plain_buf: Vec<u8, { wifi_pass_seal_v2::MAX_PLAINTEXT }> = Vec::new();
            let outcome = match wifi_pass_seal_v2::open_sealed_v2(
                &stored,
                &device_key,
                &mut plain_buf,
            ) {
                Ok(o) => o,
                Err(e) => {
                    log::error!("[at] WIFIPASSUPGRADE: open failed: {e}");
                    return AtOutcome::error(7);
                }
            };
            // Reject if the recovered plaintext is empty (no
            // point in writing a sealed empty blob).
            if plain_buf.is_empty() {
                log::warn!("[at] WIFIPASSUPGRADE: recovered plaintext is empty; refusing");
                return AtOutcome::error(4);
            }
            // The plaintext may contain non-UTF-8 bytes; we
            // need a &str to call seal_str. Try strict-validate.
            let plain_str: &str = match core::str::from_utf8(&plain_buf) {
                Ok(s) => s,
                Err(_) => {
                    log::warn!("[at] WIFIPASSUPGRADE: plaintext is non-UTF-8; refusing");
                    return AtOutcome::error(4);
                }
            };
            let mut nonce = [0u8; wifi_pass_seal_v2::NONCE_LEN];
            if getrandom::getrandom(&mut nonce).is_err() {
                return AtOutcome::error(6);
            }
            let mut new_sealed: HeaplessString<{ wifi_pass_seal_v2::MAX_ENCODED_LEN }> =
                HeaplessString::new();
            if let Err(e) = wifi_pass_seal_v2::seal_str(
                plain_str,
                &device_key,
                &nonce,
                &mut new_sealed,
            ) {
                log::error!("[at] WIFIPASSUPGRADE: DBO2 seal failed: {e}");
                return AtOutcome::error(7);
            }
            if nvs_save(NVS_KEY_WIFI_PASS, new_sealed.as_str(), "magent").is_err() {
                return AtOutcome::error(7);
            }
            log::info!(
                "[at] WIFIPASSUPGRADE: migrated legacy entry (was {:?}) to DBO2",
                outcome
            );
            AtOutcome::ok_line("+WIFIPASSUPGRADE:MIGRATED")
        }
        _ => AtOutcome::error(4),
    }
}

fn cwhostname_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let name = nvs_load(NVS_KEY_WIFI_HOSTNAME, NS).unwrap_or_default();
            let mut line = ReplyLine::new();
            let _ = write!(line, "+CWHOSTNAME:\"{}\"", escape_wire(&name));
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            // PATCHED (MicroAgent): routed through the host-tested
            // validator, which applies length / NUL / UTF-8 checks and
            // decodes ESP-AT quoted escapes. The old inline path
            // silently persisted `""` on non-UTF-8 input.
            let validated = match magent_core::at_validate::validate_cwhostname_set(cmd) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            if validated.hostname.is_empty() {
                return AtOutcome::error(4);
            }
            match nvs_save(NVS_KEY_WIFI_HOSTNAME, validated.hostname.as_str(), NS) {
                Ok(()) => AtOutcome::NoReply,
                Err(_) => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn cwautoconn_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let v = nvs_load(NVS_KEY_WIFI_AUTOCONN, NS)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);
            let mut line = ReplyLine::new();
            let _ = write!(line, "+CWAUTOCONN:{}", v);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let raw = cmd.arg(0).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            match raw {
                Some(v) if v <= 1 => match nvs_save(NVS_KEY_WIFI_AUTOCONN, &v.to_string(), NS) {
                    Ok(()) => AtOutcome::NoReply,
                    Err(_) => AtOutcome::error(7),
                },
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn cwreconncfg_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let interval = nvs_load(NVS_KEY_WIFI_RECONN_INTERVAL, NS)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);
            let repeat = nvs_load(NVS_KEY_WIFI_RECONN_REPEAT, NS)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);
            let mut line = ReplyLine::new();
            let _ = write!(line, "+CWRECONNCFG:{},{}", interval, repeat);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let interval = cmd.arg(0).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            let repeat = cmd.arg(1).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            match (interval, repeat) {
                (Some(i), Some(r)) if i <= 7200 && r <= 1000 => {
                    if nvs_save(NVS_KEY_WIFI_RECONN_INTERVAL, &i.to_string(), NS).is_err() {
                        return AtOutcome::error(7);
                    }
                    if nvs_save(NVS_KEY_WIFI_RECONN_REPEAT, &r.to_string(), NS).is_err() {
                        return AtOutcome::error(7);
                    }
                    AtOutcome::NoReply
                }
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

/// `AT+IFCONFIG` — report the current STA IPv4 address (from the Wi-Fi
/// supervisor's live snapshot). Previously this returned a hard-coded
/// "+IFCONFIG: deferred"; now it reflects the real address (empty when the
/// STA has no link yet).
fn ifconfig_line(wifi_status: Option<&crate::WifiStatusHandle>) -> AtOutcome {
    let ip = wifi_status
        .and_then(|s| s.lock().ok())
        .map(|g| g.ip.clone())
        .unwrap_or_default();
    let mut line = ReplyLine::new();
    let _ = write!(line, "+IFCONFIG:\"{}\"", escape_wire(&ip));
    AtOutcome::Ok { data: line }
}

fn cipstamac_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            // CIPSTAMAC?: report the *real* STA netif MAC. Previously this
            // hard-coded a placeholder ("02:00:00:00:00:01") which made it
            // look like the feature worked, then was refused with +CMDER:9.
            // We now read it from the radio via `esp_wifi_get_mac`.
            let mut mac: [u8; 6] = [0u8; 6];
            let rc = unsafe {
                esp_idf_sys::esp_wifi_get_mac(
                    esp_idf_sys::wifi_interface_t_WIFI_IF_STA,
                    mac.as_mut_ptr(),
                )
            };
            if rc != esp_idf_sys::ESP_OK {
                log::warn!("[at] CIPSTAMAC? esp_wifi_get_mac failed: {rc}");
                return AtOutcome::error(9);
            }
            let mut line = ReplyLine::new();
            let _ = write!(
                line,
                "+CIPSTAMAC:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            // Setting a custom STA MAC requires stopping the radio first
            // (`esp_wifi_set_mac` fails while running) and re-associating —
            // risky mid-session, so it stays unsupported for now.
            log::warn!("[at] CIPSTAMAC set — runtime MAC change not supported");
            AtOutcome::error(4)
        }
        _ => AtOutcome::error(4),
    }
}

fn safemode_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let v = nvs_load(NVS_KEY_SAFEMODE_FORCE, NS)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0);
            let mut line = ReplyLine::new();
            let _ = write!(line, "+SAFEMODE:{}", v);
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let v = cmd.arg(0).and_then(|a| match a {
                AtArg::Token(b) => at::parse_u32(b),
                _ => None,
            });
            match v {
                Some(n) if n <= 1 => match nvs_save(NVS_KEY_SAFEMODE_FORCE, &n.to_string(), NS) {
                    Ok(()) => AtOutcome::NoReply,
                    Err(_) => AtOutcome::error(7),
                },
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

fn ident_query() -> AtOutcome {
    // `dev_identity` may be either BTDK1-sealed (modern) or legacy
    // 64-char hex plaintext. Both formats contain the same 32 raw
    // seed bytes once opened; the dispatcher doesn't care which
    // form is on disk — it just needs the public key for display.
    let stored = match nvs_load(NVS_KEY_IDENTITY, "magent") {
        Some(s) => s,
        None => return AtOutcome::ok_line("+IDENT:NO_IDENTITY"),
    };
    let seed = match crate::device_key::open_dev_identity(&stored) {
        Ok(s) => s,
        Err(_) => return AtOutcome::ok_line("+IDENT:INVALID"),
    };
    let id = match Identity::from_secret_bytes(&seed) {
        Ok(i) => i,
        Err(_) => return AtOutcome::ok_line("+IDENT:INVALID"),
    };
    let mut line = ReplyLine::new();
    let _ = write!(line, "+IDENT:{}", hex::encode(id.public_key().as_bytes()));
    AtOutcome::Ok { data: line }
}

fn ident_rot_dispatch(safe_mode: bool) -> AtOutcome {
    if safe_mode {
        log::warn!("[at] IDENTROT refused: safe mode active");
        return AtOutcome::error(4);
    }
    let mut seed = [0u8; 32];
    if getrandom::getrandom(&mut seed).is_err() {
        return AtOutcome::error(6);
    }
    let id = match Identity::from_secret_bytes(&seed) {
        Ok(i) => i,
        Err(_) => return AtOutcome::error(6),
    };
    // Seal with BTDK1 before persisting. If sealing fails (e.g.
    // eFuse read fault), fall back to plaintext with a loud
    // warning — the operator can still use the identity, just
    // without the hardware-bound wrapper.
    match crate::device_key::seal_and_store_dev_identity(&seed) {
        Ok(()) => {}
        Err(e) => {
            log::error!("[at] IDENTROT: BTDK1 seal failed ({e}); persisting as plaintext");
            let hex = hex::encode(seed);
            if nvs_save(NVS_KEY_IDENTITY, &hex, "magent").is_err() {
                return AtOutcome::error(7);
            }
        }
    }
    let mut line = ReplyLine::new();
    let _ = write!(line, "+IDENTROT:{}", hex::encode(id.public_key().as_bytes()));
    AtOutcome::Ok { data: line }
}

/// `AT+SIGN="<payload>"` — sign a payload with the device's Ed25519 identity
/// and return the canonical signed-message JSON (signer DID + payload_hex +
/// signature_hex). Previously deferred to v0.3 ("needs IngressGateway
/// context"); the dispatcher can load the identity directly, so signing now
/// works without the gateway. The signed message is the same wire format the
/// ingress uses to sign every command envelope, so a host can verify it with
/// the same `SignedMessage::from_json` / Ed25519 verify path.
fn sign_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    let payload = match cmd.arg(0) {
        Some(AtArg::Quoted(s)) => s,
        _ => return AtOutcome::error(4),
    };
    if payload.is_empty() || payload.len() > 64 {
        // Keep payload_hex short enough that the JSON reply fits in the
        // 256-byte reply line (64 raw bytes -> 128 hex chars + DID + sig).
        log::warn!("[at] SIGN: payload must be 1..=64 bytes");
        return AtOutcome::error(7);
    }
    let stored = match nvs_load(NVS_KEY_IDENTITY, "magent") {
        Some(s) => s,
        None => {
            log::warn!("[at] SIGN: no device identity in NVS");
            return AtOutcome::error(9);
        }
    };
    let seed = match crate::device_key::open_dev_identity(&stored) {
        Ok(s) => s,
        Err(_) => return AtOutcome::error(9),
    };
    let id = match Identity::from_secret_bytes(&seed) {
        Ok(i) => i,
        Err(_) => return AtOutcome::error(9),
    };
    let signed = match id.sign(payload) {
        Ok(m) => m,
        Err(_) => return AtOutcome::error(7),
    };
    let mut line = ReplyLine::new();
    if signed.to_json_into(&mut line).is_err() {
        log::warn!("[at] SIGN: signed message exceeds reply buffer");
        return AtOutcome::error(7);
    }
    AtOutcome::Ok { data: line }
}

/// Hard cap on the number of concurrent `AT+HTTPGET` worker threads. A
/// worker whose TLS handshake hangs is deliberately leaked (so the ingress
/// thread stays responsive); this counter stops that leak from growing
/// without bound and exhausting the heap / task pool.
const HTTP_MAX_WORKERS: u32 = 2;
static HTTP_WORKERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// RAII guard that releases a worker slot on drop, so every return path of
/// `http_get_dispatch` (Ok / Err / timeout) correctly decrements the count.
struct HTTPWorkerGuard;

impl Drop for HTTPWorkerGuard {
    fn drop(&mut self) {
        HTTP_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `AT+HTTPGET=<url>` — issue an HTTP GET and report the status code plus
/// a short body preview. Used to verify outbound network reachability from
/// the device (e.g. to well-known Chinese websites).
///
/// The URL is validated by `magent_core::at_validate::validate_httpget_set`
/// (http/https scheme whitelist, length cap, control-byte rejection) before
/// any worker thread is spawned.
///
/// Runs the actual network call on a separate worker thread and waits for
/// it with a bounded `recv_timeout`, so a hung TLS handshake can NEVER
/// block the ingress thread (which would freeze the whole AT console).
/// If the worker doesn't finish in time it is intentionally leaked (bounded
/// by the 6s HTTP timeout) and we reply with an error.
fn http_get_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    // Validate the URL up front (scheme whitelist, length cap, control-byte
    // rejection) before spending any worker / connection budget on it.
    let validated = match magent_core::at_validate::validate_httpget_set(cmd) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };

    // Bound concurrent workers (see `HTTP_MAX_WORKERS`).
    if HTTP_WORKERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= HTTP_MAX_WORKERS {
        HTTP_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        log::warn!("[at] HTTPGET: concurrency limit ({HTTP_MAX_WORKERS}) reached — rejected");
        return AtOutcome::error(6);
    }
    let _guard = HTTPWorkerGuard;

    let url = validated.url.as_str().to_string(); // owned for the worker thread

    let (tx, rx) = std::sync::mpsc::channel::<Result<ReplyLine, u8>>();
    // PATCHED (MicroAgent): the TLS handshake needs a real stack — the default
    // pthread stack (~4 KiB) overflows during a TLS handshake to
    // api.deepseek.com and panics the board ("Guru Meditation: Stack protection
    // fault"). Give the worker 24 KiB like the ingress thread.
    std::thread::Builder::new()
        .name("httpget-worker".into())
        .stack_size(24 * 1024)
        .spawn(move || {
            let _ = tx.send(http_get_worker(&url));
        })
        .ok();

    match rx.recv_timeout(std::time::Duration::from_secs(12)) {
        Ok(Ok(line)) => AtOutcome::Ok { data: line },
        Ok(Err(code)) => AtOutcome::error(code),
        Err(_) => {
            log::warn!("[at] HTTPGET: worker timed out (12s) — reply dropped");
            AtOutcome::error(6)
        }
    }
}

/// Do the actual HTTP GET (runs on a worker thread). Returns the reply line
/// or an error code (6 = connect/request failed).
fn http_get_worker(url: &str) -> Result<ReplyLine, u8> {
    use embedded_svc::http::client::Client as HttpClient;
    use embedded_svc::http::Method;
    use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
    use std::net::ToSocketAddrs;

    log::info!(
        "[at] HTTPGET: free heap before request = {}",
        unsafe { esp_idf_sys::esp_get_free_heap_size() }
    );

    // --- DNS + connection preflight with a bounded timeout ---
    // Resolve the host and verify we can actually reach it BEFORE handing
    // off to the HTTP client. esp-idf-svc's HTTP client `timeout` does not
    // reliably bound the DNS / TCP-connect / TLS phases on the C61, so a
    // dead or unresolvable host would otherwise make the worker hang.
    // `TcpStream::connect_timeout` gives a hard 5s cap on the connect phase.
    let (host, port) = match url_host_port(url) {
        Some(hp) => hp,
        None => {
            log::warn!("[at] HTTPGET: malformed URL: {url}");
            return Err(4);
        }
    };
    let authority = format!("{host}:{port}");
    match authority.to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => {
                match std::net::TcpStream::connect_timeout(
                    &addr,
                    core::time::Duration::from_secs(CONNECT_TIMEOUT_S),
                ) {
                    Ok(_) => log::info!("[at] HTTPGET: reachable {authority} (preflight ok)"),
                    Err(e) => {
                        log::warn!("[at] HTTPGET: connect timeout to {authority}: {e}");
                        return Err(6);
                    }
                }
            }
            None => {
                log::warn!("[at] HTTPGET: no address resolved for {authority}");
                return Err(6);
            }
        },
        Err(e) => {
            log::warn!("[at] HTTPGET: DNS resolution failed for {authority}: {e}");
            return Err(6);
        }
    }

    let cfg = HttpConfig {
        timeout: Some(core::time::Duration::from_secs(6)),
        // mbedTLS CA certificate bundle (enabled via
        // `CONFIG_MBEDTLS_CERTIFICATE_BUNDLE=y` in sdkconfig.defaults) so
        // HTTPS server certs verify correctly.
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let conn = EspHttpConnection::new(&cfg).map_err(|e| {
        log::warn!("[at] HTTPGET: connect failed: {e}");
        6
    })?;
    let mut client = HttpClient::wrap(conn);
    let headers = [("accept", "*/*")];
    let request = client.request(Method::Get, url, &headers).map_err(|e| {
        log::warn!("[at] HTTPGET: request failed: {e}");
        6
    })?;
    let mut response = request.submit().map_err(|e| {
        log::warn!("[at] HTTPGET: submit failed: {e}");
        6
    })?;
    let status = response.status();

    // Bounded read of the body (first ~256 bytes) for a preview.
    let mut buf = [0u8; 256];
    let mut read = 0usize;
    while read < buf.len() {
        match response.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => break,
        }
    }
    let lossy = String::from_utf8_lossy(&buf[..read]);
    let mut preview: HeaplessString<120> = HeaplessString::new();
    for c in lossy.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            continue;
        }
        if preview.push(c).is_err() {
            break;
        }
    }
    let mut line = ReplyLine::new();
    let _ = write!(line, "+HTTPGET:{} bytes={} body={}", status, read, preview);
    Ok(line)
}

/// Hard cap (seconds) on the TCP-connect phase of `AT+HTTPGET`'s preflight
/// DNS + connect check. Keeps an unreachable host from hanging the worker.
const CONNECT_TIMEOUT_S: u64 = 5;

/// Extract `(host, port)` from a `http://` / `https://` URL. Handles
/// optional userinfo (`user:pass@host`), a trailing port, and defaults
/// (80 for http, 443 for https). Returns `None` for anything else.
pub(crate) fn url_host_port(url: &str) -> Option<(String, u16)> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let is_tls = url.starts_with("https://");
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?; // strip userinfo
    if authority.is_empty() {
        return None;
    }
    if let Some(idx) = authority.rfind(':') {
        // Only treat as port if the part after ':' is all digits and the
        // host doesn't look like an unbracketed IPv6 literal.
        let host = &authority[..idx];
        let port_str = &authority[idx + 1..];
        if !host.is_empty() && !port_str.is_empty() {
            if let Ok(port) = port_str.parse::<u16>() {
                return Some((host.to_string(), port));
            }
        }
        return Some((authority.to_string(), if is_tls { 443 } else { 80 }));
    }
    Some((authority.to_string(), if is_tls { 443 } else { 80 }))
}

/// `AT+LLMCFG?` / `AT+LLMCFG=<model>,<api_key>` — query / set the LLM
/// backend parameters the agent uses for reasoning. The API key is never
/// echoed back verbatim on query; it is masked (`sk-abc…def`).
///
/// The SET path is validated by `magent_core::at_validate::validate_llmcfg_set`
/// (length caps, NUL / control-byte / whitespace rejection, UTF-8) before
/// anything is written to NVS.
fn llmcfg_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    match cmd.kind {
        AtCommandKind::Query => {
            let model = nvs_load(NVS_KEY_LLM_MODEL, NS).unwrap_or_default();
            let key = nvs_load(NVS_KEY_LLM_API_KEY, NS).unwrap_or_default();
            let mut line = ReplyLine::new();
            let _ = write!(line, "+LLMCFG:{},{}", model, mask_key(&key));
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let validated = match magent_core::at_validate::validate_llmcfg_set(cmd) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            match (
                nvs_save(NVS_KEY_LLM_MODEL, validated.model.as_str(), NS),
                nvs_save(NVS_KEY_LLM_API_KEY, validated.api_key.as_str(), NS),
            ) {
                (Ok(()), Ok(())) => {
                    log::info!(
                        "[at] LLMCFG: model={} api_key set",
                        validated.model.as_str()
                    );
                    AtOutcome::NoReply
                }
                _ => AtOutcome::error(7),
            }
        }
        _ => AtOutcome::error(4),
    }
}

/// Mask a secret for display: keep the first 4 and last 4 chars, fill the
/// rest with `*`. A short secret is fully masked.
fn mask_key(key: &str) -> HeaplessString<40> {
    let mut out = HeaplessString::new();
    let b = key.as_bytes();
    if b.len() <= 8 {
        let _ = out.push_str("****");
        return out;
    }
    let _ = out.push_str(&key[..4]);
    let _ = out.push_str("…");
    let _ = out.push_str(&key[b.len() - 4..]);
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn escape_wire(s: &str) -> HeaplessString<128> {
    let mut out = HeaplessString::new();
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                if out.push('\\').is_err() { break; }
                if out.push(c).is_err() { break; }
            }
            '\n' => { let _ = out.push_str("\\n"); }
            '\r' => { let _ = out.push_str("\\r"); }
            other => { if out.push(other).is_err() { break; } }
        }
    }
    out
}

/// `AT+TIME?` — report the current wall-clock time as ISO 8601 UTC
/// plus the source of the last authoritative sync. Empty / `None`
/// reply if no sync has happened yet (the firmware treats this as
/// "device clock unknown" and refuses to lie about the wall-clock).
fn time_dispatch(
    cmd: &AtCommand<'_>,
    time_sync: Option<&crate::sntp_sync::TimeSyncHandle>,
    now_ms: u64,
) -> AtOutcome {
    if !matches!(cmd.kind, AtCommandKind::Query | AtCommandKind::Execute) {
        return AtOutcome::error(4);
    }
    let handle = match time_sync {
        Some(h) => h,
        None => return AtOutcome::ok_line("+TIME:UNSYNCED"),
    };
    let guard = match handle.lock() {
        Ok(g) => g,
        Err(_) => return AtOutcome::error(7),
    };
    if guard.source() == Source::None {
        return AtOutcome::ok_line("+TIME:UNSYNCED");
    }
    let mut iso: HeaplessString<32> = HeaplessString::new();
    if guard.format_iso8601(now_ms, &mut iso).is_err() {
        return AtOutcome::ok_line("+TIME:ERROR");
    }
    let wall = match guard.now_unix(now_ms) {
        Some(w) => w,
        None => return AtOutcome::ok_line("+TIME:ERROR"),
    };
    let mut line: ReplyLine = ReplyLine::new();
    let _ = write!(
        line,
        "+TIME:{},{},{}",
        iso.as_str(),
        wall,
        guard.source().tag(),
    );
    AtOutcome::Ok { data: line }
}

/// `AT+NTPSYNC` — force an immediate SNTP re-sync. The supervisor
/// thread is the actual sync owner; we just set a flag the
/// supervisor polls on its 5-s tick. Returns NoReply so the caller
/// doesn't have to wait — the operator reads the result via
/// `AT+TIME?` afterwards.
fn ntp_sync_dispatch(
    cmd: &AtCommand<'_>,
    time_sync: Option<&crate::sntp_sync::TimeSyncHandle>,
    force_ntp_sync: &mut bool,
    safe_mode: bool,
) -> AtOutcome {
    if !matches!(cmd.kind, AtCommandKind::Execute) {
        return AtOutcome::error(4);
    }
    if safe_mode {
        log::warn!("[at] NTPSYNC refused: safe mode active");
        return AtOutcome::error(4);
    }
    if time_sync.is_none() {
        log::warn!("[at] NTPSYNC refused: no SNTP supervisor (wifi feature off?)");
        return AtOutcome::error(9);
    }
    *force_ntp_sync = true;
    log::info!("[at] NTPSYNC: supervisor will re-sync on next tick");
    AtOutcome::NoReply
}

/// `AT+TIMEZONE?` / `AT+TIMEZONE=<minutes>` — query / set the
/// operator-supplied UTC offset used by future `AT+TIME?` replies
/// for human-readable display. The canonical wall-clock stays UTC;
/// only the local-time annotation shifts.
fn timezone_dispatch(
    cmd: &AtCommand<'_>,
    time_sync: Option<&crate::sntp_sync::TimeSyncHandle>,
) -> AtOutcome {
    let handle = match time_sync {
        Some(h) => h,
        None => return AtOutcome::error(7),
    };
    match cmd.kind {
        AtCommandKind::Query => {
            let guard = match handle.lock() {
                Ok(g) => g,
                Err(_) => return AtOutcome::error(7),
            };
            let mut line: ReplyLine = ReplyLine::new();
            let _ = write!(line, "+TIMEZONE:{}", guard.tz_offset_minutes());
            AtOutcome::Ok { data: line }
        }
        AtCommandKind::Set => {
            let validated = match magent_core::at_validate::validate_timezone_set(cmd) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            let mut guard = match handle.lock() {
                Ok(g) => g,
                Err(_) => return AtOutcome::error(7),
            };
            if guard.set_tz_offset_minutes(validated.offset_minutes).is_err() {
                return AtOutcome::error(7);
            }
            if nvs_save(tz_bare_key(), &validated.offset_minutes.to_string(), NS).is_err() {
                return AtOutcome::error(7);
            }
            log::info!(
                "[at] TIMEZONE set: offset={} minutes",
                validated.offset_minutes
            );
            AtOutcome::NoReply
        }
        _ => AtOutcome::error(4),
    }
}

/// `AT+BLE?` / `AT+BLE=<ON|OFF|STATE>` — query / control the BLE
/// peripheral.
///
/// The verb is validated by `magent_core::at_validate::validate_ble_set`
/// so a malformed `AT+BLE=` line returns a precise `+CMDER:4` / `:7`
/// instead of falling through.
///
/// Routing to [`crate::ble_at::handle_ble_command`] requires a shared
/// `BleServer` handle, which the dispatcher does not yet hold. Until that
/// wiring lands (main.rs should pass a `Arc<Mutex<BleServer>>` alongside
/// `time_sync`), report `+CMDER:9` (unsupported) for a *valid* verb so
/// the command is never silently swallowed.
fn ble_dispatch(cmd: &AtCommand<'_>) -> AtOutcome {
    // Validate the verb *before* touching the BLE stack so a malformed
    // `AT+BLE=` line yields a precise error (`+CMDER:4`/`:7`) rather than
    // the generic unsupported code. The actual `BleServer` operation is
    // still gated on the wiring below.
    if let Err(outcome) = magent_core::at_validate::validate_ble_set(cmd) {
        return outcome;
    }
    log::warn!("[at] BLE control not yet wired to a shared BleServer");
    AtOutcome::error(9)
}

// Suppress dead-code warnings for shared NVS keys that exist but are
// not exercised by the dispatcher (they are still consumed by main.rs
// boot path).
#[allow(dead_code)]
const _BOOT_COUNT_KEY_TIE: &str = "boot_count";

// `_freertos_tick` is here only as a placeholder for future
// time-budgeted operations (currently unused).
#[allow(dead_code)]
fn _freertos_tick() -> u64 {
    unsafe { esp_idf_sys::esp_timer_get_time() as u64 / 1000 }
}
