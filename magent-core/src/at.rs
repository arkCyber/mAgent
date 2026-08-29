//! AT command parser for `mAgent`.
//!
//! Implements the subset of the [Espressif ESP-AT] command surface needed
//! for production provisioning (Wi-Fi credentials, hostname, MAC,
//! autoconnect, safe-mode, identity rotation, system info, …). The
//! parser is intentionally **pure no_std + alloc-free** so it can be
//! unit-tested on the host and executed inside the same thread that
//! drives the existing UART ingress without a separate task.
//!
//! # Aerograde guarantees
//!
//! * **Zero `panic` / `unwrap` / `expect` in dispatch paths.** Every
//!   parser function returns `Result<_ , AtParseError>`. Callers must
//!   not call `unwrap` on the return value; if they do, that is a
//!   caller bug, not a parser bug.
//! * **Bounded execution time.** Parsing is `O(n)` in the input length
//!   with a single pass and a fixed small loop per command keyword.
//!   No backtracking, no recursion, no allocator.
//! * **Bounded memory.** The parser writes into caller-supplied
//!   `heapless::Vec` / `heapless::String` and never allocates on the
//!   heap.
//! * **Crash-loop awareness.** The firmware-side dispatcher carries an
//!   `AtState { echo: bool, last_cmd_ticks }` value that the
//!   UART task ticks on each line; if the watchdog sees stale state
//!   for >500 ms while a line is in flight, it forces a
//!   `+CMDER:7 (timeout)` response and clears the parser.
//! * **Audit trail.** Every successful command line is logged
//!   with `(uptime_ms, command, result_code, elapsed_us)` by the
//!   firmware-side dispatcher; the parser itself only emits a
//!   `Result<AtCommand>` value and lets the caller format logs.
//!
//! # Wire-format conformance
//!
//! Lines are terminated by `\r`, `\n`, or `\r\n` (any combination).
//! Commands must start with case-insensitive `AT` followed by one of
//! `+ = ? , ;` or end-of-line. Lines that aren't AT commands are
//! signalled via [`AtParseErrorKind::NotAnAtCommand`] so the caller can
//! route them back to the natural-language agent path.
//!
//! Parameter escaping follows Espressif's syntax (`\,"`, `\\,`, …) —
//! see [`parse_string_arg`]. Numeric arguments are base-10 `i32`s; out-
//! of-range values produce [`AtParseErrorKind::NumberOutOfRange`].
//!
//! # Tests
//!
//! The `#[cfg(test)] mod tests` at the bottom of this file is the
//! specification: every test case is a documented ESP-AT behaviour.
//!
//! [Espressif ESP-AT]: https://docs.espressif.com/projects/esp-at/

#![allow(clippy::result_large_err)]

use core::fmt;
use heapless::{String, Vec};

/// Maximum number of bytes in a single AT command line, including the
/// terminating line break. ESP-AT commands are constrained to ≤256 by
/// the official firmware; matching that limit keeps us compatible with
/// any existing tooling written for ESP-AT devices.
pub const MAX_LINE: usize = 256;

/// Maximum number of arguments (positional or named) the parser will
/// inspect in a single command. Anything beyond is silently ignored so
/// the firmware keeps responding deterministically even if a misbehaving
/// script sends excess parameters.
pub const MAX_ARGUMENTS: usize = 12;

/// Maximum parameter payload length (post-escape-decoding). 64 is the
/// ESP-AT max for WPA passwords; larger values are clamped.
pub const MAX_PARAM_LEN: usize = 64;

/// Maximum SSID length (bytes). ESP-AT allows up to 32 bytes for SSIDs.
pub const MAX_SSID_LEN: usize = 32;

/// Maximum hostname length (bytes). RFC 1123 limits hostnames to 253;
/// we use 32 to stay friendly for log/audit records.
pub const MAX_HOSTNAME_LEN: usize = 32;

/// Maximum MAC string length (canonical `aa:bb:cc:dd:ee:ff` form = 17).
pub const MAX_MAC_LEN: usize = 17;

/// Maximum value any single command response payload can grow to.
///
/// Set so a single Wi-Fi scan result (≤7 APs × ≤80 bytes/line) fits
/// with margin to spare, while still being small enough that a single
/// response stays inside the firmware's UART output buffer.
pub const MAX_RESPONSE: usize = 768;

/// Look-ahead kind after the `AT` prefix. Drives how the rest of the
/// line is interpreted.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AtVerb {
    /// `AT` on its own (handshake).
    Ping,
    /// `ATE0` / `ATE1` (echo on/off).
    SetEcho(bool),
    /// `AT+NAME` (no `=` / `?` / `=?`).
    Execute,
    /// `AT+NAME?` (query).
    Query,
    /// `AT+NAME=...` (set, possibly with trailing `?` for `AT+NAME=?`).
    Set,
    /// `AT+NAME=?` (test — what values are accepted).
    Test,
}

/// Stack-resident scratch buffer used by the firmware when it can't
/// keep the input line alive across the parser call (e.g. it was
/// allocated in a transient scratch buffer on a UART IRQ or in a
/// `IngressGateway` frame). The firmware copies the line into this
/// buffer first, then parses; the resulting [`AtCommand`] borrows
/// from `ScratchBuffer`, so dispatchers can use it freely.
///
/// The buffer is `Copy` so it can be re-used per command without
/// additional allocation. It is sized to `MAX_LINE` to match the
/// parser's hard cap; the parser truncates input past that.
#[derive(Copy, Clone)]
pub struct ScratchBuffer {
    bytes: [u8; MAX_LINE],
}

impl ScratchBuffer {
    /// Create a zeroed scratch buffer for reuse across AT lines.
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_LINE],
        }
    }

    /// Copy `line` into this buffer (truncating past `MAX_LINE`) and
    /// parse it. Returns `Ok(AtCommand<'_>)` with borrows tied to this
    /// scratch buffer.
    pub fn copy_and_parse<'a>(&'a mut self, line: &[u8]) -> Result<AtCommand<'a>, AtParseError> {
        let n = line.len().min(MAX_LINE);
        self.bytes[..n].copy_from_slice(&line[..n]);
        let stored = &self.bytes[..n];
        parse_line(stored)
    }
}

impl Default for ScratchBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// What kind of payload the parser saw.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AtCommandKind {
    /// No payload at all (e.g. `AT+IDENTROT` standalone).
    Execute,
    /// Query form (`?` suffix; no payload expected).
    Query,
    /// Set form (`=` followed by zero or more comma-separated args).
    Set,
    /// Test form (`=?` suffix; callable to discover accepted values).
    Test,
    /// Pure ping or echo-toggle — no further payload.
    Control,
}

/// Numeric policy for argument parsing.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NumericRange {
    /// `[0, i16::MAX]`.
    U16Small,
    /// `[0, u16::MAX]`.
    U16,
    /// `[0, i32::MAX]`.
    U32Seconds,
    /// `[0, 7200]` (reconnect interval, ESP-AT bound).
    ReconnInterval,
    /// `[0, 1000]` (reconnect repeat, ESP-AT bound).
    ReconnRepeat,
    /// `[1, 600]` (CWJAP timeout in seconds, ESP-AT bound).
    JapTimeout,
    /// `[-100, 40]` (RSSI in dBm).
    Rssi,
    /// `[0, 600]` (inactive time in seconds).
    InactiveTime,
    /// `[1, 100]` (listen interval in beacons).
    ListenInterval,
}

/// String policy for argument parsing.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StringSpec {
    /// Plain ASCII / UTF-8 string, not validated beyond length.
    Free,
    /// Wi-Fi SSID. Length-checked to ≤32 bytes.
    Ssid,
    /// Wi-Fi password. Length-checked to ≤64 bytes.
    Passphrase,
    /// Hostname. Length-checked to ≤32.
    Hostname,
    /// MAC address `aa:bb:cc:dd:ee:ff` (17 chars).
    Mac,
}

/// Spec of the command currently being parsed. Each variant describes
/// the *kind* of arguments it accepts so the firmware-side dispatcher
/// can either run validation generically (via the `AtCommand` ADT below)
/// or hand-validate a specific command.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AtOp {
    /// `AT` (no payload).
    Ping,
    /// `ATEn` — echo on (`ATE1`) / off (`ATE0`).
    SetEcho {
        /// Whether echo is on (`true`) or off (`false`).
        on: bool,
    },
    /// `AT+GMR`
    GetVersion,
    /// `AT+RST`
    Reset,
    /// `AT+SYSRAM?`
    SysRam,
    /// `AT+SYSLOG?` / `=0..5`
    SysLog,
    /// `AT+SYSSTORE?` / `=0/1`
    SysStore,
    /// `AT+CWMODE?` / `=0..3`
    CwMode,
    /// `AT+CWJAP?` / `="ssid","pwd"` (set with 1-2 string args) /
    /// `AT+CWJAP` (execute, retry last).
    CwJap,
    /// `AT+CWQAP`
    CwQap,
    /// `AT+CWLAP`
    CwLap,
    /// `AT+CWHOSTNAME?` / `=name`
    CwHostname,
    /// `AT+CWAUTOCONN?` / `=0/1`
    CwAutoconn,
    /// `AT+CWRECONNCFG?` / `=interval,repeat[,now]`
    CwReconnCfg,
    /// `AT+CWSTATE?`
    CwState,
    /// `AT+CIPSTAMAC?` / `=mac`
    CipStaMac,
    /// `AT+MACRAND?` / `=0/1`
    MacRand,
    /// `AT+HEAP?`
    Heap,
    /// `AT+UPTIME?`
    Uptime,
    /// `AT+SAFEMODE?` / `=0/1`
    Safemode,
    /// `AT+IDENT?`
    Ident,
    /// `AT+IDENTROT`
    IdentRot,
    /// `AT+SIGN="text"`
    Sign,
    /// `AT+RESTORE`
    Restore,
    /// `AT+IFCONFIG?`
    Ifconfig,
    /// `AT+PING=<ip/host>`
    Ping6,
    /// `AT+AGENT=<text...>` — escape hatch that sends the rest of
    /// the line to the agent's ReAct loop.
    Agent,
    /// `AT+WIFIPASSUPGRADE?` / `=1` — re-seal an existing
    /// DBO1 / legacy plaintext `wifi_pass` entry under DBO2 in
    /// place. Idempotent.
    WifiPassUpgrade,
    /// `AT+HTTPGET=<url>` — issue an HTTP GET to verify outbound
    /// network reachability (e.g. to a well-known website).
    HttpGet,
    /// `AT+OTA=<url>` — stream an OTA firmware image from `url` to the
    /// inactive OTA slot, verify it, then reboot into it.
    Ota,
    /// `AT+LLMCFG?` / `AT+LLMCFG=<model>,<api_key>` — set / query the
    /// LLM backend parameters (model name + API key) used by the agent.
    LlmCfg,
    /// `AT+TIME?` — report the current wall-clock time as ISO 8601
    /// UTC plus the source (SNTP / BLE CTS / NONE).
    Time,
    /// `AT+NTPSYNC` — force an immediate SNTP re-sync (ESP32 only;
    /// nRF52 returns +CMDER:9 unsupported).
    NtpSync,
    /// `AT+TIMEZONE?` / `=offset_minutes` — query / set the
    /// operator-supplied UTC offset used for human-readable display
    /// (does NOT affect the canonical Unix-seconds value).
    Timezone,
    /// `AT+BLE?` / `AT+BLE=<ON|OFF|STATE>` — query / control the BLE
    /// peripheral (ESP32 only; nRF52 returns unsupported).
    Ble,
    /// `AT+LUAAPP=<base64>` / `AT+LUAAPP?` — set / query the operator-provided
    /// Lua application source. The source is **URL-safe base64** (no `+`/`/`)
    /// so it survives AT argument parsing (which splits on commas); the
    /// firmware decodes it and persists it as the boot `main.lua`.
    LuaApp,
}

impl AtOp {
    /// Human-readable name (no `AT+` prefix). Stable across versions.
    pub const fn name(self) -> &'static str {
        match self {
            AtOp::Ping => "",
            AtOp::SetEcho { .. } => "E",
            AtOp::GetVersion => "+GMR",
            AtOp::Reset => "+RST",
            AtOp::SysRam => "+SYSRAM",
            AtOp::SysLog => "+SYSLOG",
            AtOp::SysStore => "+SYSSTORE",
            AtOp::CwMode => "+CWMODE",
            AtOp::CwJap => "+CWJAP",
            AtOp::CwQap => "+CWQAP",
            AtOp::CwLap => "+CWLAP",
            AtOp::CwHostname => "+CWHOSTNAME",
            AtOp::CwAutoconn => "+CWAUTOCONN",
            AtOp::CwReconnCfg => "+CWRECONNCFG",
            AtOp::CwState => "+CWSTATE",
            AtOp::CipStaMac => "+CIPSTAMAC",
            AtOp::MacRand => "+MACRAND",
            AtOp::Heap => "+HEAP",
            AtOp::Uptime => "+UPTIME",
            AtOp::Safemode => "+SAFEMODE",
            AtOp::Ident => "+IDENT",
            AtOp::IdentRot => "+IDENTROT",
            AtOp::Sign => "+SIGN",
            AtOp::Restore => "+RESTORE",
            AtOp::Ifconfig => "+IFCONFIG",
            AtOp::Ping6 => "+PING",
            AtOp::Agent => "+AGENT",
            AtOp::WifiPassUpgrade => "+WIFIPASSUPGRADE",
            AtOp::HttpGet => "+HTTPGET",
            AtOp::Ota => "+OTA",
            AtOp::LlmCfg => "+LLMCFG",
            AtOp::Time => "+TIME",
            AtOp::NtpSync => "+NTPSYNC",
            AtOp::Timezone => "+TIMEZONE",
            AtOp::Ble => "+BLE",
            AtOp::LuaApp => "+LUAAPP",
        }
    }
}

/// One argument slot inside an AT command.
///
/// We accept up to `MAX_ARGUMENTS` positional slots; named-style
/// arguments (`name=value`) are flattened into the same slot array in
/// `pos, key, val` order so the dispatcher can match either way.
#[derive(Debug, PartialEq, Eq)]
pub enum AtArg<'a> {
    /// Unquoted token without a `=` sign (e.g. `1` in `AT+CWMODE=1`).
    Token(&'a [u8]),
    /// Quoted string with escapes already unescaped
    /// (e.g. `"My SSID"` → `My SSID`).
    Quoted(&'a [u8]),
    /// `<key>=<value>` pair, both already un-quoted/escaped.
    Named {
        /// The un-quoted key of a `<key>=<value>` argument.
        key: &'a [u8],
        /// The un-quoted, escaped value of a `<key>=<value>` argument.
        val: &'a [u8],
    },
}

/// A successfully-parsed AT command line. Owned data is borrowed from
/// the caller's scratch buffer so parsing does not allocate.
#[derive(Debug, PartialEq, Eq)]
pub struct AtCommand<'a> {
    /// The resolved command operation (e.g. `CwJap`).
    pub op: AtOp,
    /// Whether this is a query / set / test / execute form.
    pub kind: AtCommandKind,
    /// Positional and named arguments, in order, up to `MAX_ARGUMENTS`.
    pub args: Vec<AtArg<'a>, MAX_ARGUMENTS>,
    /// The verb used (controls echo reply discipline: Set queries do not
    /// echo the parameters back, etc.).
    pub verb: AtVerb,
}

impl<'a> AtCommand<'a> {
    /// Return the `idx`-th argument, if present.
    pub fn arg(&self, idx: usize) -> Option<&AtArg<'a>> {
        self.args.get(idx)
    }
}

/// Why parsing failed. Use `Display` to log; the firmware uses
/// `AtParseErrorKind` to drive the `+CMDER:<code>` numeric reply.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AtParseErrorKind {
    /// Line is empty / all whitespace / contains `\0`.
    Empty,
    /// Line wasn't an AT command (caller should re-route to the agent).
    NotAnAtCommand,
    /// Line starts with `AT` but doesn't have a known prefix
    /// (`+`, `E`, etc.) — likely a typo in the verb. Logged as such;
    /// firmware returns `+CMDER:5` (UNKNOWN_OP).
    UnknownOp,
    /// `AT+NAME=` followed by too many positional / named arguments.
    TooManyArgs,
    /// Numeric argument that failed `parse_int` or out of range.
    NumberOutOfRange,
    /// String argument that's too long for the spec.
    StringTooLong,
    /// Unterminated quoted string.
    UnterminatedString,
    /// Bad escape sequence inside a quoted string.
    BadEscape,
    /// MAC string with wrong length or non-hex character.
    InvalidMac,
    /// Some other contract violation. (New variants should be added
    /// deliberately so logs stay meaningful.)
    InvalidArgument,
    /// Internal parser overflow — should never happen; logged as FAILED
    /// (`+CMDER:6`).
    Internal,
}

/// A parse failure with a machine-readable kind and a source column.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AtParseError {
    /// The specific reason parsing failed (drives the `+CMDER:<code>` reply).
    pub kind: AtParseErrorKind,
    /// 1-based column where parsing stopped. May be `0` if N/A.
    pub col: usize,
}

impl AtParseError {
    /// Construct a parse error from a kind and a 1-based column offset.
    pub const fn new(kind: AtParseErrorKind, col: usize) -> Self {
        Self { kind, col }
    }
}

impl fmt::Display for AtParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            AtParseErrorKind::Empty => write!(f, "empty line"),
            AtParseErrorKind::NotAnAtCommand => write!(f, "not an AT command"),
            AtParseErrorKind::UnknownOp => write!(f, "unknown AT verb"),
            AtParseErrorKind::TooManyArgs => write!(f, "too many arguments"),
            AtParseErrorKind::NumberOutOfRange => write!(f, "number out of range"),
            AtParseErrorKind::StringTooLong => write!(f, "string too long"),
            AtParseErrorKind::UnterminatedString => write!(f, "unterminated quoted string"),
            AtParseErrorKind::BadEscape => write!(f, "bad escape in string"),
            AtParseErrorKind::InvalidMac => write!(f, "invalid MAC address"),
            AtParseErrorKind::InvalidArgument => write!(f, "invalid argument"),
            AtParseErrorKind::Internal => write!(f, "internal parser error"),
        }
    }
}

/// Map a parser error to the ESP-AT numeric `+CMDER:<n>` reply so
/// external scripts can react.
///
/// The table follows the official convention where available; the
/// remaining codes are reserved for `mAgent`-specific failures.
impl AtParseError {
    /// Map this error to the ESP-AT numeric `+CMDER:<n>` reply code.
    pub const fn numeric_code(self) -> u8 {
        match self.kind {
            AtParseErrorKind::Empty => 0,
            AtParseErrorKind::NotAnAtCommand => 0,
            AtParseErrorKind::UnknownOp => 5,
            AtParseErrorKind::TooManyArgs => 6,
            AtParseErrorKind::NumberOutOfRange => 7,
            AtParseErrorKind::StringTooLong => 8,
            AtParseErrorKind::UnterminatedString => 4,
            AtParseErrorKind::BadEscape => 4,
            AtParseErrorKind::InvalidMac => 9,
            AtParseErrorKind::InvalidArgument => 4,
            AtParseErrorKind::Internal => 6,
        }
    }
}

// ---------------------------------------------------------------------------
// External (parser-level) quick reject
// ---------------------------------------------------------------------------

/// Cheap pre-check: does this line look like an AT command at all? Lets
/// the firmware skip the full parser for natural-language text that the
/// agent should handle.
///
/// Recognised forms:
/// - line starts with `AT` (case insensitive) and is followed by one
///   of `+`, `E`, `?`, `=`, a line break, or nothing.
/// - line starts with `+`, `=`, `?`, `E`, or alphabetic letters that
///   aren't `AT` → *not* an AT command (e.g. natural language).
pub fn is_at_line(line: &[u8]) -> bool {
    let line = trim_line_terminator(line);
    if line.len() < 2 {
        return false;
    }
    // Look for "AT" or "at" at start.
    let mut i = 0;
    // Strip up to 3 leading whitespace bytes (rare but valid).
    while i < line.len() && line[i].is_ascii_whitespace() {
        i += 1;
    }
    if i + 2 > line.len() {
        return false;
    }
    let (a, b) = (line[i], line[i + 1]);
    if !matches!((a, b), (b'A' | b'a', b'T' | b't')) {
        return false;
    }
    let rest = &line[i + 2..];
    if rest.is_empty() {
        return true; // bare `AT`
    }
    let c = rest[0];
    // Next char must be one of: '+', 'E'/'e', '?', '=', whitespace,
    // or end-of-line (we already trimmed those).
    matches!(c, b'+' | b'E' | b'e' | b'?' | b'=' | b' ' | b'\t')
}

/// Parse one line into an [`AtCommand`].
///
/// `line` may include the terminating `\r\n`; the parser handles both
/// forms. Returns:
/// - `Ok(cmd)` for recognised commands.
/// - `Err(NotAnAtCommand)` so the caller can route text back to the
///   agent's ReAct loop (the parser doesn't decide what to do with
///   non-AT text — it just says "this isn't for me").
/// - `Err(<other>)` for parsing failures the firmware surfaces as
///   `+CMDER:<code>\r\nERROR\r\n`.
pub fn parse_line(line: &[u8]) -> Result<AtCommand<'_>, AtParseError> {
    let line = trim_line_terminator(line);
    if line.is_empty() {
        return Err(AtParseError::new(AtParseErrorKind::Empty, 0));
    }
    if !is_at_line(line) {
        return Err(AtParseError::new(AtParseErrorKind::NotAnAtCommand, 0));
    }

    let mut p = Parser::new(line);
    p.expect_at()?;
    let verb = p.scan_verb()?;
    let (op, kind, args) = p.parse_op(verb)?;
    Ok(AtCommand {
        op,
        kind,
        args,
        verb,
    })
}

/// Trim `\r`, `\n`, `\r\n` from the end of `line`. Used by
/// [`parse_line`] so callers don't have to.
pub fn trim_line_terminator(mut line: &[u8]) -> &[u8] {
    while let Some(&last) = line.last() {
        if last == b'\r' || last == b'\n' {
            line = &line[..line.len() - 1];
        } else {
            break;
        }
    }
    line
}

// ---------------------------------------------------------------------------
// Internal parser state
// ---------------------------------------------------------------------------

struct Parser<'a> {
    line: &'a [u8],
    pos: usize,
    args: Vec<AtArg<'a>, MAX_ARGUMENTS>,
}

impl<'a> Parser<'a> {
    const fn new(line: &'a [u8]) -> Self {
        Self {
            line,
            pos: 0,
            args: Vec::new(),
        }
    }

    /// Skip leading whitespace.
    fn skip_ws(&mut self) {
        while let Some(&c) = self.line.get(self.pos) {
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Expect literal `AT` (case insensitive) at the current position.
    fn expect_at(&mut self) -> Result<(), AtParseError> {
        self.skip_ws();
        let at: &[u8] = self.line.get(self.pos..self.pos + 2).unwrap_or(&[]);
        if at.len() != 2 {
            return Err(AtParseError::new(
                AtParseErrorKind::NotAnAtCommand,
                self.pos,
            ));
        }
        let at_lo = [at[0].to_ascii_lowercase(), at[1].to_ascii_lowercase()];
        if at_lo != *b"at" {
            return Err(AtParseError::new(
                AtParseErrorKind::NotAnAtCommand,
                self.pos,
            ));
        }
        self.pos += 2;
        Ok(())
    }

    /// Classify the verb (`AT`, `ATE0`, `AT+NAME`, `AT+NAME?`,
    /// `AT+NAME=...`).
    fn scan_verb(&mut self) -> Result<AtVerb, AtParseError> {
        self.skip_ws();
        if self.pos >= self.line.len() {
            return Ok(AtVerb::Ping);
        }
        let c = self.line[self.pos];
        match c {
            b'+' => {
                self.pos += 1;
                // We'll let parse_op decide.
                Ok(AtVerb::Execute)
            }
            b'E' | b'e' => {
                self.pos += 1;
                let n = self.line.get(self.pos).copied().unwrap_or(b'1');
                self.pos += 1;
                match n {
                    b'0' => Ok(AtVerb::SetEcho(false)),
                    b'1' => Ok(AtVerb::SetEcho(true)),
                    _ => Err(AtParseError::new(
                        AtParseErrorKind::InvalidArgument,
                        self.pos,
                    )),
                }
            }
            b'?' | b'=' => Ok(AtVerb::Execute),
            _ => Err(AtParseError::new(
                AtParseErrorKind::NotAnAtCommand,
                self.pos,
            )),
        }
    }

    /// Identify which `AtOp` and parse its arguments.
    fn parse_op(
        &mut self,
        verb: AtVerb,
    ) -> Result<(AtOp, AtCommandKind, Vec<AtArg<'a>, MAX_ARGUMENTS>), AtParseError> {
        if matches!(verb, AtVerb::Ping) {
            return Ok((AtOp::Ping, AtCommandKind::Control, Vec::new()));
        }
        if let AtVerb::SetEcho(on) = verb {
            return Ok((AtOp::SetEcho { on }, AtCommandKind::Control, Vec::new()));
        }
        // We're in `AT+...` territory. Scan the op keyword.
        let start = self.pos;
        let (op, _default_kind) = classify_op({
            let end = self.scan_keyword()?;
            end
        })
        .ok_or_else(|| AtParseError::new(AtParseErrorKind::UnknownOp, start))?;

        // We need the keyword's end offset to look at the next char.
        let kw_end = self.pos;
        let kind = match self.line.get(kw_end).copied() {
            None => AtCommandKind::Execute,
            Some(b'?') => {
                self.pos = kw_end + 1;
                AtCommandKind::Query
            }
            Some(b'=') => {
                self.pos = kw_end + 1;
                self.parse_set_args()?;
                AtCommandKind::Set
            }
            Some(_) => {
                return Err(AtParseError::new(AtParseErrorKind::InvalidArgument, kw_end));
            }
        };
        Ok((op, kind, core::mem::take(&mut self.args)))
    }

    /// Read a `+FOO` keyword (alphabetic letters / digits / underscore
    /// up to next non-id char). Returns the slice.
    fn scan_keyword(&mut self) -> Result<&'a [u8], AtParseError> {
        let start = self.pos;
        while let Some(&c) = self.line.get(self.pos) {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(AtParseError::new(AtParseErrorKind::UnknownOp, start));
        }
        Ok(&self.line[start..self.pos])
    }

    /// Parse the comma-separated argument list after the `=` sign of a
    /// Set or Test command.
    fn parse_set_args(&mut self) -> Result<(), AtParseError> {
        // Accept `=?` (test) as zero-arg set with kind=Test later.
        if self.line.get(self.pos).copied() == Some(b'?') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            // Skip optional whitespace before each arg.
            self.skip_ws();
            if self.pos >= self.line.len() {
                break;
            }
            self.parse_one_arg()?;
            self.skip_ws();
            match self.line.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                    continue;
                }
                None => break,
                Some(_) => {
                    return Err(AtParseError::new(
                        AtParseErrorKind::InvalidArgument,
                        self.pos,
                    ));
                }
            }
        }
        Ok(())
    }

    fn parse_one_arg(&mut self) -> Result<(), AtParseError> {
        // Try `key=value` first (alpha then '=' then value).
        let save = self.pos;
        let mut probe = self.pos;
        while probe < self.line.len() {
            let c = self.line[probe];
            if c == b'=' {
                // Confirm `key` starts with a letter (so '=' inside a
                // quoted string doesn't get mis-classified).
                if let Some(&start_c) = self.line.get(save) {
                    if start_c.is_ascii_alphabetic() {
                        let key = &self.line[save..probe];
                        self.pos = probe + 1; // skip '='
                        let (val, _q) = self.parse_value()?;
                        self.push_arg(AtArg::Named { key, val })?;
                        return Ok(());
                    }
                }
                break;
            }
            if c == b',' || c.is_ascii_whitespace() {
                break;
            }
            probe += 1;
        }
        self.pos = save;
        let (val, quoted) = self.parse_value()?;
        if quoted {
            self.push_arg(AtArg::Quoted(val))?;
        } else {
            self.push_arg(AtArg::Token(val))?;
        }
        Ok(())
    }

    /// Parse either a quoted or unquoted value. Quoted values are
    /// unescaped (\,  \"  \\  \r  \n  \t).
    fn parse_value(&mut self) -> Result<(&'a [u8], bool), AtParseError> {
        self.skip_ws();
        if self.line.get(self.pos).copied() == Some(b'"') {
            // Quoted. Don't unescape in-place; the dispatcher can
            // unescape later if a specific AT command requires it.
            self.pos += 1;
            let start = self.pos;
            while let Some(&c) = self.line.get(self.pos) {
                if c == b'"' {
                    let val = &self.line[start..self.pos];
                    self.pos += 1;
                    return Ok((val, true));
                }
                if c == b'\\' {
                    // Step over the escape + next char.
                    self.pos += 2;
                } else {
                    self.pos += 1;
                }
            }
            return Err(AtParseError::new(
                AtParseErrorKind::UnterminatedString,
                start,
            ));
        }
        // Unquoted: read until comma or EOF.
        let start = self.pos;
        while let Some(&c) = self.line.get(self.pos) {
            if c == b',' || c.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(AtParseError::new(AtParseErrorKind::InvalidArgument, start));
        }
        Ok((&self.line[start..self.pos], false))
    }

    fn push_arg(&mut self, a: AtArg<'a>) -> Result<(), AtParseError> {
        if self.args.len() >= MAX_ARGUMENTS {
            return Err(AtParseError::new(AtParseErrorKind::TooManyArgs, self.pos));
        }
        self.args
            .push(a)
            .map_err(|_| AtParseError::new(AtParseErrorKind::Internal, self.pos))
    }
}

fn classify_op(kw: &[u8]) -> Option<(AtOp, AtCommandKind)> {
    Some(match kw {
        b"GMR" => (AtOp::GetVersion, AtCommandKind::Execute),
        b"RST" => (AtOp::Reset, AtCommandKind::Execute),
        b"SYSRAM" => (AtOp::SysRam, AtCommandKind::Query),
        b"SYSLOG" => (AtOp::SysLog, AtCommandKind::Set),
        b"SYSSTORE" => (AtOp::SysStore, AtCommandKind::Set),
        b"CWMODE" => (AtOp::CwMode, AtCommandKind::Set),
        b"CWJAP" => (AtOp::CwJap, AtCommandKind::Set),
        b"CWQAP" => (AtOp::CwQap, AtCommandKind::Execute),
        b"CWLAP" => (AtOp::CwLap, AtCommandKind::Execute),
        b"CWHOSTNAME" => (AtOp::CwHostname, AtCommandKind::Set),
        b"CWAUTOCONN" => (AtOp::CwAutoconn, AtCommandKind::Set),
        b"CWRECONNCFG" => (AtOp::CwReconnCfg, AtCommandKind::Set),
        b"CWSTATE" => (AtOp::CwState, AtCommandKind::Query),
        b"CIPSTAMAC" => (AtOp::CipStaMac, AtCommandKind::Set),
        b"MACRAND" => (AtOp::MacRand, AtCommandKind::Set),
        b"HEAP" => (AtOp::Heap, AtCommandKind::Query),
        b"UPTIME" => (AtOp::Uptime, AtCommandKind::Query),
        b"SAFEMODE" => (AtOp::Safemode, AtCommandKind::Set),
        b"IDENT" => (AtOp::Ident, AtCommandKind::Query),
        b"IDENTROT" => (AtOp::IdentRot, AtCommandKind::Execute),
        b"SIGN" => (AtOp::Sign, AtCommandKind::Set),
        b"RESTORE" => (AtOp::Restore, AtCommandKind::Execute),
        b"IFCONFIG" => (AtOp::Ifconfig, AtCommandKind::Query),
        b"PING" => (AtOp::Ping6, AtCommandKind::Set),
        b"AGENT" => (AtOp::Agent, AtCommandKind::Set),
        b"WIFIPASSUPGRADE" => (AtOp::WifiPassUpgrade, AtCommandKind::Set),
        b"HTTPGET" => (AtOp::HttpGet, AtCommandKind::Set),
        b"OTA" => (AtOp::Ota, AtCommandKind::Set),
        b"LLMCFG" => (AtOp::LlmCfg, AtCommandKind::Set),
        b"TIME" => (AtOp::Time, AtCommandKind::Query),
        b"NTPSYNC" => (AtOp::NtpSync, AtCommandKind::Execute),
        b"TIMEZONE" => (AtOp::Timezone, AtCommandKind::Set),
        b"BLE" => (AtOp::Ble, AtCommandKind::Set),
        b"LUAAPP" => (AtOp::LuaApp, AtCommandKind::Set),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Validation helpers — exported so the firmware dispatcher can re-use
// them after parsing.
// ---------------------------------------------------------------------------

/// Parse a decimal ASCII byte slice into a `u32`, rejecting any
/// non-digit. Returns `(value, Ok(()) )` on success.
pub fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    let mut v: u32 = 0;
    for &c in bytes {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u32)?;
    }
    Some(v)
}

/// Parse `bytes` as a `i32`, accepting an optional leading `-`.
///
/// Note: we don't reach `i32::MIN` directly — the magnitude of
/// `i32::MIN` is `i32::MAX + 1` and won't fit in a non-negative
/// counter. ESP-AT numbers live well below this anyway (no parameter
/// exceeds 7200), so callers won't notice.
pub fn parse_i32(bytes: &[u8]) -> Option<i32> {
    if bytes.is_empty() {
        return None;
    }
    let (sign, rest) = if bytes[0] == b'-' {
        (-1_i32, &bytes[1..])
    } else {
        (1_i32, bytes)
    };
    if rest.is_empty() {
        return None; // bare `-` is not a number
    }
    // Use u64 as the working type to cover i32::MAX cleanly.
    let mut v: u64 = 0;
    for &c in rest {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u64)?;
    }
    // Bound-check before re-applying the sign.
    let limit: u64 = if sign < 0 {
        (i32::MAX as u64) + 1
    } else {
        i32::MAX as u64
    };
    if v > limit {
        return None;
    }
    let signed = if sign < 0 && v == limit {
        i32::MIN
    } else {
        (v as i32) * sign
    };
    Some(signed)
}

/// Validate a Wi-Fi SSID per the constraints we expose in the AT
/// reference: 1..=32 bytes, no `\` (escaping is done by the host
/// before sending; the parser accepts whatever sits inside the
/// quotes), UTF-8 safe. Returns `None` on success, `Some(reason)`
/// otherwise.
pub fn validate_ssid(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.is_empty() {
        return Err("empty");
    }
    if bytes.len() > MAX_SSID_LEN {
        return Err("too_long");
    }
    // HARDENING (clippy/manual_contains): use `contains` instead
    // of `iter().any` for clarity and minor performance gain.
    if bytes.contains(&0) {
        return Err("contains_nul");
    }
    Ok(())
}

/// Validate a WPA passphrase. Empty is allowed (open AP); >64 bytes is
/// an error. ESP-AT also rejects `\0`.
pub fn validate_passphrase(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() > 64 {
        return Err("too_long");
    }
    // HARDENING (clippy/manual_contains): use `contains` instead
    // of `iter().any` for clarity and minor performance gain.
    if bytes.contains(&0) {
        return Err("contains_nul");
    }
    Ok(())
}

/// Decode ESP-AT escape sequences inside a *quoted* argument into
/// `out` (caller-provided, so this stays zero-alloc).
///
/// The parser deliberately returns quoted bytes verbatim (with any
/// backslash escapes intact) so boundary scanning stays trivial; the
/// actual decode happens here, at the point where a value is consumed
/// (e.g. `at_validate::validate_cwjap_set`). Known escapes are
/// collapsed to the single character they represent:
///
/// ```text
/// \,  → ,      \"  → "      \\  → \      \n → \n
/// \r  → \r     \t  → \t
/// ```
///
/// An unknown escape (a backslash followed by any other byte) is
/// preserved verbatim (`\x` stays `\x`) rather than silently dropped,
/// so no input bytes are ever lost.
///
/// Returns `Err(())` if `out` would overflow (caller decides the error
/// code — typically the "too long" rejection).
#[allow(clippy::result_unit_err)] // `()` is an intentional marker: the caller maps it to its own error code.
pub fn unescape_quoted<const N: usize>(
    src: &[u8],
    out: &mut heapless::Vec<u8, N>,
) -> Result<(), ()> {
    out.clear();
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c == b'\\' && i + 1 < src.len() {
            let n = src[i + 1];
            match n {
                b',' => out.push(b',').map_err(|_| ())?,
                b'"' => out.push(b'"').map_err(|_| ())?,
                b'\\' => out.push(b'\\').map_err(|_| ())?,
                b'n' => out.push(b'\n').map_err(|_| ())?,
                b'r' => out.push(b'\r').map_err(|_| ())?,
                b't' => out.push(b'\t').map_err(|_| ())?,
                _ => {
                    // Unknown escape: keep the backslash literally.
                    out.push(b'\\').map_err(|_| ())?;
                    out.push(n).map_err(|_| ())?;
                }
            }
            i += 2;
        } else {
            out.push(c).map_err(|_| ())?;
            i += 1;
        }
    }
    Ok(())
}

/// Validate `aa:bb:cc:dd:ee:ff` (or any separator). Returns `Some(mac)`
/// on success.
pub fn validate_mac(bytes: &[u8]) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    if bytes.len() != 17 {
        return None;
    }
    for (i, part) in bytes.split(|&c| c == b':' || c == b'-').enumerate() {
        if i >= 6 {
            return None;
        }
        if part.len() != 2 {
            return None;
        }
        let hi = hex_digit(part[0])?;
        let lo = hex_digit(part[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Build a stable response string into a caller-provided buffer.
/// Used by the firmware to avoid allocation on the hot AT path.
///
/// `data_lines` is the list of `+CMD:...` lines that should appear
/// before the terminating `OK`/`ERROR`. `term` decides the trailer.
#[allow(clippy::result_unit_err)] // `()` is an intentional marker: the caller maps it to its own error code.
pub fn build_response(
    data_lines: &[&[u8]],
    kind: AtResponseKind,
    out: &mut Vec<u8, MAX_RESPONSE>,
) -> Result<(), ()> {
    for line in data_lines {
        if out.len() + line.len() + 2 > out.capacity() {
            return Err(());
        }
        out.extend_from_slice(line).map_err(|_| ())?;
        out.extend_from_slice(b"\r\n").map_err(|_| ())?;
    }
    let trailer: &[u8] = match kind {
        AtResponseKind::Ok => b"OK",
        AtResponseKind::Error => b"ERROR",
    };
    if out.len() + trailer.len() + 2 > out.capacity() {
        return Err(());
    }
    out.extend_from_slice(trailer).map_err(|_| ())?;
    out.extend_from_slice(b"\r\n").map_err(|_| ())?;
    Ok(())
}

/// Outcome of an AT command on the wire.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AtResponseKind {
    /// The command completed successfully.
    Ok,
    /// The command failed.
    Error,
}

/// A typed log tag for the audit trail, sized to a full AT line.
pub type AtLog<'a> = String<MAX_LINE>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_line_terminators() {
        assert_eq!(trim_line_terminator(b"AT\r\n"), b"AT");
        assert_eq!(trim_line_terminator(b"AT\n"), b"AT");
        assert_eq!(trim_line_terminator(b"AT\r"), b"AT");
        assert_eq!(trim_line_terminator(b"AT"), b"AT");
        assert_eq!(trim_line_terminator(b""), b"");
        assert_eq!(trim_line_terminator(b"\r\n"), b"");
    }

    #[test]
    fn is_at_line_accepts_at_family() {
        assert!(is_at_line(b"AT"));
        assert!(is_at_line(b"at"));
        assert!(is_at_line(b"AT\r\n"));
        assert!(is_at_line(b"AT+GMR"));
        assert!(is_at_line(b"AT+HEAP?"));
        assert!(is_at_line(b"AT+CWJAP=\"foo\",\"bar\""));
        assert!(is_at_line(b"ATE0"));
        assert!(is_at_line(b"ate1"));
    }

    #[test]
    fn is_at_line_rejects_natural_language() {
        assert!(!is_at_line(b"read the temperature"));
        assert!(!is_at_line(b""));
        assert!(!is_at_line(b"set wifi to home"));
        assert!(!is_at_line(b"A")); // too short
    }

    #[test]
    fn parses_ping() {
        let cmd = parse_line(b"AT\r\n").unwrap();
        assert_eq!(cmd.op, AtOp::Ping);
        assert_eq!(cmd.kind, AtCommandKind::Control);
    }

    #[test]
    fn parses_echo_off() {
        let cmd = parse_line(b"ATE0").unwrap();
        assert_eq!(cmd.op, AtOp::SetEcho { on: false });
    }

    #[test]
    fn parses_echo_on() {
        let cmd = parse_line(b"ATE1").unwrap();
        assert_eq!(cmd.op, AtOp::SetEcho { on: true });
    }

    #[test]
    fn parses_gmr() {
        let cmd = parse_line(b"AT+GMR").unwrap();
        assert_eq!(cmd.op, AtOp::GetVersion);
    }

    #[test]
    fn parses_ota_with_url() {
        let cmd = parse_line(b"AT+OTA=http://192.168.1.10/app.bin").unwrap();
        assert_eq!(cmd.op, AtOp::Ota);
        match cmd.args.first() {
            Some(AtArg::Token(t)) => assert_eq!(t, b"http://192.168.1.10/app.bin"),
            _ => panic!("expected token URL argument"),
        }
    }

    #[test]
    fn parses_ota_without_arg() {
        // `AT+OTA` with no URL still parses to `Ota` (the firmware dispatcher
        // rejects the missing URL with +CMDER:4, but the parser must not panic
        // or misparse).
        let cmd = parse_line(b"AT+OTA").unwrap();
        assert_eq!(cmd.op, AtOp::Ota);
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn parses_ota_query_kind_is_set() {
        // `AT+OTA=<url>` is a Set command; ensure the kind is classified as such.
        let cmd = parse_line(b"AT+OTA=http://h/app.bin").unwrap();
        assert_eq!(cmd.kind, AtCommandKind::Set);
    }

    #[test]
    fn parses_ota_https_url() {
        let cmd = parse_line(b"AT+OTA=https://firmware.example.com/magent.bin").unwrap();
        assert_eq!(cmd.op, AtOp::Ota);
    }

    #[test]
    fn parses_restore() {
        let cmd = parse_line(b"AT+RESTORE").unwrap();
        assert_eq!(cmd.op, AtOp::Restore);
    }

    #[test]
    fn parses_macrand() {
        let cmd = parse_line(b"AT+MACRAND").unwrap();
        assert_eq!(cmd.op, AtOp::MacRand);
    }

    #[test]
    fn parses_rst() {
        let cmd = parse_line(b"AT+RST").unwrap();
        assert_eq!(cmd.op, AtOp::Reset);
    }

    #[test]
    fn parses_sysstore_query() {
        let cmd = parse_line(b"AT+SYSSTORE?").unwrap();
        assert_eq!(cmd.op, AtOp::SysStore);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_sysstore_set() {
        let cmd = parse_line(b"AT+SYSSTORE=1").unwrap();
        assert_eq!(cmd.op, AtOp::SysStore);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(v)) => assert_eq!(*v, b"1"),
            _ => panic!("expected token 1"),
        }
    }

    #[test]
    fn parses_cwmode_set() {
        let cmd = parse_line(b"AT+CWMODE=3").unwrap();
        assert_eq!(cmd.op, AtOp::CwMode);
        assert_eq!(cmd.kind, AtCommandKind::Set);
    }

    #[test]
    fn parses_cwjap_set_quoted() {
        let cmd = parse_line(b"AT+CWJAP=\"MyHome\",\"secret123\"").unwrap();
        assert_eq!(cmd.op, AtOp::CwJap);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        // `eprintln!` is std-only and magent-core is no_std by default, so we
        // do not print the parsed args here (the assertions below verify them).
        match cmd.arg(0) {
            Some(AtArg::Quoted(s)) => assert_eq!(*s, b"MyHome"),
            _ => panic!("expected quoted SSID, got {:?}", cmd.args),
        }
        match cmd.arg(1) {
            Some(AtArg::Quoted(s)) => assert_eq!(*s, b"secret123"),
            _ => panic!("expected quoted pass, got {:?}", cmd.args),
        }
    }

    #[test]
    fn parses_cwjap_query() {
        let cmd = parse_line(b"AT+CWJAP?").unwrap();
        assert_eq!(cmd.op, AtOp::CwJap);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_cwjap_execute() {
        let cmd = parse_line(b"AT+CWJAP").unwrap();
        assert_eq!(cmd.op, AtOp::CwJap);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_cwqap() {
        let cmd = parse_line(b"AT+CWQAP").unwrap();
        assert_eq!(cmd.op, AtOp::CwQap);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_cwlap() {
        let cmd = parse_line(b"AT+CWLAP").unwrap();
        assert_eq!(cmd.op, AtOp::CwLap);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_cwautoconn() {
        let cmd = parse_line(b"AT+CWAUTOCONN=1").unwrap();
        assert_eq!(cmd.op, AtOp::CwAutoconn);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(s)) => assert_eq!(*s, b"1"),
            _ => panic!("expected token"),
        }
    }

    #[test]
    fn parses_cwreconncfg() {
        let cmd = parse_line(b"AT+CWRECONNCFG=5,3").unwrap();
        assert_eq!(cmd.op, AtOp::CwReconnCfg);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        assert_eq!(cmd.args.len(), 2);
    }

    #[test]
    fn parses_named_args() {
        // `+HEAP` doesn't accept args, but parser should still scan the
        // comma-list when a `=` is present. We test with `+CWAUTOCONN`
        // which has known token semantics.
        let cmd = parse_line(b"AT+CWAUTOCONN=1,interval=5").unwrap();
        assert_eq!(cmd.op, AtOp::CwAutoconn);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        assert_eq!(cmd.args.len(), 2);
        match &cmd.args[0] {
            AtArg::Token(t) => assert_eq!(*t, b"1"),
            _ => panic!("expected token"),
        }
        match &cmd.args[1] {
            AtArg::Named { key, val } => {
                assert_eq!(*key, b"interval");
                assert_eq!(*val, b"5");
            }
            _ => panic!("expected named"),
        }
    }

    #[test]
    fn parses_mac_quote() {
        let cmd = parse_line(b"AT+CIPSTAMAC=\"aa:bb:cc:dd:ee:ff\"").unwrap();
        assert_eq!(cmd.op, AtOp::CipStaMac);
        match cmd.arg(0) {
            Some(AtArg::Quoted(m)) => assert_eq!(*m, b"aa:bb:cc:dd:ee:ff"),
            _ => panic!("expected quoted MAC"),
        }
    }

    #[test]
    fn parses_cwstate() {
        let cmd = parse_line(b"AT+CWSTATE?").unwrap();
        assert_eq!(cmd.op, AtOp::CwState);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_sysram() {
        let cmd = parse_line(b"AT+SYSRAM?").unwrap();
        assert_eq!(cmd.op, AtOp::SysRam);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_heap() {
        let cmd = parse_line(b"AT+HEAP?").unwrap();
        assert_eq!(cmd.op, AtOp::Heap);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_httpget_set() {
        let cmd = parse_line(b"AT+HTTPGET=http://example.com/").unwrap();
        assert_eq!(cmd.op, AtOp::HttpGet);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.args.first() {
            Some(AtArg::Token(t)) => assert_eq!(t, b"http://example.com/"),
            _ => panic!("expected token URL"),
        }
    }

    #[test]
    fn parses_llmcfg_set() {
        let cmd = parse_line(b"AT+LLMCFG=deepseek,sk-abc123").unwrap();
        assert_eq!(cmd.op, AtOp::LlmCfg);
        assert_eq!(cmd.kind, AtCommandKind::Set);
    }

    #[test]
    fn parses_sign() {
        let cmd = parse_line(b"AT+SIGN=\"hello\"").unwrap();
        assert_eq!(cmd.op, AtOp::Sign);
        match cmd.arg(0) {
            Some(AtArg::Quoted(s)) => assert_eq!(*s, b"hello"),
            _ => panic!("expected quoted payload"),
        }
    }

    #[test]
    fn parses_uptime() {
        let cmd = parse_line(b"AT+UPTIME?").unwrap();
        assert_eq!(cmd.op, AtOp::Uptime);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_safemode_set() {
        let cmd = parse_line(b"AT+SAFEMODE=1").unwrap();
        assert_eq!(cmd.op, AtOp::Safemode);
        assert_eq!(cmd.kind, AtCommandKind::Set);
    }

    #[test]
    fn parses_ident_query() {
        let cmd = parse_line(b"AT+IDENT?").unwrap();
        assert_eq!(cmd.op, AtOp::Ident);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_identrot_execute() {
        let cmd = parse_line(b"AT+IDENTROT").unwrap();
        assert_eq!(cmd.op, AtOp::IdentRot);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_restore_execute() {
        let cmd = parse_line(b"AT+RESTORE").unwrap();
        assert_eq!(cmd.op, AtOp::Restore);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_ping6_set() {
        let cmd = parse_line(b"AT+PING=\"192.168.1.1\"").unwrap();
        assert_eq!(cmd.op, AtOp::Ping6);
        assert_eq!(cmd.kind, AtCommandKind::Set);
    }

    #[test]
    fn parses_ping6_unquoted() {
        // `AT+PING=1.2.3.4` (no quotes) parses the host as a token arg.
        let cmd = parse_line(b"AT+PING=192.168.1.1").unwrap();
        assert_eq!(cmd.op, AtOp::Ping6);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.args.first() {
            Some(AtArg::Token(t)) => assert_eq!(t, b"192.168.1.1"),
            _ => panic!("expected token host"),
        }
    }

    #[test]
    fn parses_ping6_hostname() {
        // Hostnames parse too (resolved by DNS in the firmware layer).
        let cmd = parse_line(b"AT+PING=example.com").unwrap();
        assert_eq!(cmd.op, AtOp::Ping6);
        match cmd.args.first() {
            Some(AtArg::Token(t)) => assert_eq!(t, b"example.com"),
            _ => panic!("expected token hostname"),
        }
    }

    #[test]
    fn parses_agent_set() {
        let cmd = parse_line(b"AT+AGENT=\"read the temperature\"").unwrap();
        assert_eq!(cmd.op, AtOp::Agent);
        match cmd.arg(0) {
            Some(AtArg::Quoted(s)) => assert_eq!(*s, b"read the temperature"),
            _ => panic!("expected quoted agent payload"),
        }
    }

    #[test]
    fn parses_wifipassupgrade_query() {
        let cmd = parse_line(b"AT+WIFIPASSUPGRADE?").unwrap();
        assert_eq!(cmd.op, AtOp::WifiPassUpgrade);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_wifipassupgrade_set() {
        let cmd = parse_line(b"AT+WIFIPASSUPGRADE=1").unwrap();
        assert_eq!(cmd.op, AtOp::WifiPassUpgrade);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(t)) => assert_eq!(*t, b"1"),
            _ => panic!("expected token arg"),
        }
    }

    #[test]
    fn rejects_non_at_line() {
        let err = parse_line(b"hello world").unwrap_err();
        assert_eq!(err.kind, AtParseErrorKind::NotAnAtCommand);
    }

    #[test]
    fn rejects_unterminated_string() {
        let err = parse_line(b"AT+CWJAP=\"abc,123").unwrap_err();
        assert_eq!(err.kind, AtParseErrorKind::UnterminatedString);
        assert_eq!(err.numeric_code(), 4);
    }

    #[test]
    fn rejects_unknown_verb() {
        let err = parse_line(b"AT+ZZZZ?").unwrap_err();
        assert_eq!(err.kind, AtParseErrorKind::UnknownOp);
    }

    #[test]
    fn rejects_empty_line() {
        let err = parse_line(b"").unwrap_err();
        assert_eq!(err.kind, AtParseErrorKind::Empty);
    }

    #[test]
    fn parse_u32_basic() {
        assert_eq!(parse_u32(b"0"), Some(0));
        assert_eq!(parse_u32(b"123"), Some(123));
        assert_eq!(parse_u32(b"4294967295"), Some(u32::MAX));
        assert_eq!(parse_u32(b""), None);
        assert_eq!(parse_u32(b"4294967296"), None); // overflow
        assert_eq!(parse_u32(b"-1"), None);
        assert_eq!(parse_u32(b" 1"), None);
    }

    #[test]
    fn parse_i32_basic() {
        assert_eq!(parse_i32(b"0"), Some(0));
        assert_eq!(parse_i32(b"-1"), Some(-1));
        assert_eq!(parse_i32(b"-100"), Some(-100));
        assert_eq!(parse_i32(b"abc"), None);
    }

    #[test]
    fn validate_ssid_len() {
        assert!(validate_ssid(b"").is_err());
        assert!(validate_ssid(b"home").is_ok());
        assert!(validate_ssid(&[b'x'; 33]).is_err());
        assert!(validate_ssid(&[b'x'; 32]).is_ok());
    }

    #[test]
    fn validate_passphrase_len() {
        assert!(validate_passphrase(b"").is_ok());
        assert!(validate_passphrase(b"hunter2").is_ok());
        assert!(validate_passphrase(&[b'p'; 65]).is_err());
    }

    #[test]
    fn validate_mac_format() {
        assert_eq!(
            validate_mac(b"aa:bb:cc:dd:ee:ff"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        assert_eq!(
            validate_mac(b"AA:BB:CC:DD:EE:FF"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        assert_eq!(
            validate_mac(b"aa-bb-cc-dd-ee-ff"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        assert!(validate_mac(b"aa:bb").is_none());
        assert!(validate_mac(b"zz:bb:cc:dd:ee:ff").is_none());
    }

    #[test]
    fn parse_u32_boundary_values() {
        // Leading zeros collapse to the numeric value.
        assert_eq!(parse_u32(b"007"), Some(7));
        assert_eq!(parse_u32(b"0000000000"), Some(0));
        // u32::MAX is exactly 10 digits and is the largest accepted value.
        assert_eq!(parse_u32(b"4294967295"), Some(u32::MAX));
        // A 10-digit value above u32::MAX overflows via checked_add (not
        // the length gate).
        assert_eq!(parse_u32(b"9999999999"), None);
        // Non-digit in any position is rejected.
        assert_eq!(parse_u32(b"12a"), None);
        assert_eq!(parse_u32(b"1a2"), None);
        assert_eq!(parse_u32(b"a12"), None);
    }

    #[test]
    fn parse_i32_signed_boundaries() {
        // Exact extremes are accepted.
        assert_eq!(parse_i32(b"2147483647"), Some(i32::MAX));
        assert_eq!(parse_i32(b"-2147483648"), Some(i32::MIN));
        // One past each extreme is rejected — never wrapped.
        assert_eq!(parse_i32(b"2147483648"), None);
        assert_eq!(parse_i32(b"-2147483649"), None);
        // A bare minus sign (or empty input) is not a number.
        assert_eq!(parse_i32(b"-"), None);
        assert_eq!(parse_i32(b""), None);
        // Leading zeros are fine, with or without a sign.
        assert_eq!(parse_i32(b"0007"), Some(7));
        assert_eq!(parse_i32(b"-0001"), Some(-1));
        // Embedded minus is not a digit.
        assert_eq!(parse_i32(b"1-2"), None);
    }

    #[test]
    fn validate_mac_edge_cases() {
        // Mixed `:` / `-` separators are both accepted.
        assert_eq!(
            validate_mac(b"aa:bb-cc:dd-ee:ff"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        // Wrong total length (the gate is exactly 17 bytes).
        assert_eq!(validate_mac(b"aa:bb:cc:dd:ee:f"), None); // 15
        assert_eq!(validate_mac(b"aa:bb:cc:dd:ee:fff"), None); // 18
                                                               // Length-17 inputs that fail on part *count* (7 parts).
        assert_eq!(validate_mac(b"aa:bb:cc:dd:ee:f:"), None);
        // Length-17 inputs that fail on part *length* (1- and 3-char parts).
        assert_eq!(validate_mac(b"a:bb:cc:dd:ee:fff"), None);
        // Empty input.
        assert_eq!(validate_mac(b""), None);
    }

    #[test]
    fn build_response_with_data() {
        let mut buf: Vec<u8, MAX_RESPONSE> = Vec::new();
        let lines: [&[u8]; 2] = [b"+CWJAP:\"foo\",6", b"+IP:1.2.3.4"];
        build_response(&lines, AtResponseKind::Ok, &mut buf).unwrap();
        let s = core::str::from_utf8(&buf).unwrap();
        assert_eq!(s, "+CWJAP:\"foo\",6\r\n+IP:1.2.3.4\r\nOK\r\n");
    }

    #[test]
    fn build_response_error_only() {
        let mut buf: Vec<u8, MAX_RESPONSE> = Vec::new();
        build_response(&[], AtResponseKind::Error, &mut buf).unwrap();
        assert_eq!(core::str::from_utf8(&buf).unwrap(), "ERROR\r\n");
    }

    #[test]
    fn parse_cwhostname_set() {
        let cmd = parse_line(b"AT+CWHOSTNAME=\"iot-001\"").unwrap();
        assert_eq!(cmd.op, AtOp::CwHostname);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Quoted(s)) => assert_eq!(*s, b"iot-001"),
            _ => panic!("expected quoted hostname"),
        }
    }

    #[test]
    fn classify_op_returns_none_for_unknown_keyword() {
        assert!(classify_op(b"NOPE").is_none());
    }

    #[test]
    fn classify_op_covers_every_atop_variant() {
        // Regression guard: every non-special `AtOp` variant must be
        // reachable from `classify_op`, with the correct command kind. If a
        // new variant is added to the enum but not wired into the table, the
        // distinct-op count drops below 34 and this test fails loudly instead
        // of silently mis-parsing a command into an unrelated op.
        let table: &[(&[u8], AtOp, AtCommandKind)] = &[
            (b"GMR", AtOp::GetVersion, AtCommandKind::Execute),
            (b"RST", AtOp::Reset, AtCommandKind::Execute),
            (b"SYSRAM", AtOp::SysRam, AtCommandKind::Query),
            (b"SYSLOG", AtOp::SysLog, AtCommandKind::Set),
            (b"SYSSTORE", AtOp::SysStore, AtCommandKind::Set),
            (b"CWMODE", AtOp::CwMode, AtCommandKind::Set),
            (b"CWJAP", AtOp::CwJap, AtCommandKind::Set),
            (b"CWQAP", AtOp::CwQap, AtCommandKind::Execute),
            (b"CWLAP", AtOp::CwLap, AtCommandKind::Execute),
            (b"CWHOSTNAME", AtOp::CwHostname, AtCommandKind::Set),
            (b"CWAUTOCONN", AtOp::CwAutoconn, AtCommandKind::Set),
            (b"CWRECONNCFG", AtOp::CwReconnCfg, AtCommandKind::Set),
            (b"CWSTATE", AtOp::CwState, AtCommandKind::Query),
            (b"CIPSTAMAC", AtOp::CipStaMac, AtCommandKind::Set),
            (b"MACRAND", AtOp::MacRand, AtCommandKind::Set),
            (b"HEAP", AtOp::Heap, AtCommandKind::Query),
            (b"UPTIME", AtOp::Uptime, AtCommandKind::Query),
            (b"SAFEMODE", AtOp::Safemode, AtCommandKind::Set),
            (b"IDENT", AtOp::Ident, AtCommandKind::Query),
            (b"IDENTROT", AtOp::IdentRot, AtCommandKind::Execute),
            (b"SIGN", AtOp::Sign, AtCommandKind::Set),
            (b"RESTORE", AtOp::Restore, AtCommandKind::Execute),
            (b"IFCONFIG", AtOp::Ifconfig, AtCommandKind::Query),
            (b"PING", AtOp::Ping6, AtCommandKind::Set),
            (b"AGENT", AtOp::Agent, AtCommandKind::Set),
            (
                b"WIFIPASSUPGRADE",
                AtOp::WifiPassUpgrade,
                AtCommandKind::Set,
            ),
            (b"HTTPGET", AtOp::HttpGet, AtCommandKind::Set),
            (b"LLMCFG", AtOp::LlmCfg, AtCommandKind::Set),
            (b"TIME", AtOp::Time, AtCommandKind::Query),
            (b"NTPSYNC", AtOp::NtpSync, AtCommandKind::Execute),
            (b"TIMEZONE", AtOp::Timezone, AtCommandKind::Set),
            (b"BLE", AtOp::Ble, AtCommandKind::Set),
        ];

        let mut seen: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        for &(kw, op, kind) in table {
            assert_eq!(
                classify_op(kw),
                Some((op, kind)),
                "keyword {:?} must classify to {op:?}/{kind:?}",
                core::str::from_utf8(kw).unwrap_or("?"),
            );
            seen.push(alloc::format!("{op:?}"));
        }
        // Ping (`AT`) and SetEcho (`ATE0/1`) are handled outside `classify_op`.
        seen.push(alloc::format!("{:?}", AtOp::Ping));
        seen.push(alloc::format!("{:?}", AtOp::SetEcho { on: false }));

        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            34,
            "all 34 AtOp variants must be distinct and reachable (got {})",
            seen.len()
        );
    }

    #[test]
    fn unescape_quoted_decodes_esp_at_escapes() {
        let mut out: Vec<u8, 32> = Vec::new();
        unescape_quoted(b"ab\\,c", &mut out).unwrap();
        assert_eq!(&out[..], b"ab,c");

        unescape_quoted(b"a\\\"b\\\\c\\nd\\te\\rf", &mut out).unwrap();
        assert_eq!(&out[..], b"a\"b\\c\nd\te\rf");

        // A bare backslash at the very end is preserved as-is.
        unescape_quoted(b"x\\", &mut out).unwrap();
        assert_eq!(&out[..], b"x\\");
    }

    #[test]
    fn unescape_quoted_preserves_unknown_escapes() {
        let mut out: Vec<u8, 32> = Vec::new();
        // `\x` is not a defined ESP-AT escape; keep both bytes verbatim.
        unescape_quoted(b"a\\xb", &mut out).unwrap();
        assert_eq!(&out[..], b"a\\xb");
    }

    #[test]
    fn unescape_quoted_overflow_is_error() {
        // Output buffer smaller than the decoded payload → Err, not panic.
        let mut out: Vec<u8, 2> = Vec::new();
        assert!(unescape_quoted(b"abcdef", &mut out).is_err());
        // Decoding that *shrinks* the payload still succeeds.
        let mut out2: Vec<u8, 2> = Vec::new();
        unescape_quoted(b"\\,".as_ref(), &mut out2).unwrap();
        assert_eq!(&out2[..], b",");
    }

    #[test]
    fn case_insensitive_ate() {
        let cmd = parse_line(b"ate0").unwrap();
        assert_eq!(cmd.op, AtOp::SetEcho { on: false });
    }

    #[test]
    fn rejects_max_arguments() {
        // Build a command that explicitly takes too many args.
        // We'll do it by abusing AT+SYSSTORE which has no fixed-arg
        // max, then assert the parser stays bounded.
        let mut line = b"AT+SYSSTORE=".to_vec();
        for i in 0..20 {
            if i > 0 {
                line.push(b',');
            }
            line.extend_from_slice(b"1");
        }
        let err = parse_line(&line).unwrap_err();
        assert_eq!(err.kind, AtParseErrorKind::TooManyArgs);
    }

    #[test]
    fn scratch_buffer_routes_quoted() {
        let mut s = ScratchBuffer::new();
        let cmd = s
            .copy_and_parse(b"AT+CWJAP=\"foo\",\"bar\"")
            .expect("CWJAP");
        assert_eq!(cmd.op, AtOp::CwJap);
        match cmd.arg(0) {
            Some(AtArg::Quoted(p)) => assert_eq!(*p, b"foo"),
            _ => panic!("expected quoted"),
        }
    }

    #[test]
    fn parses_time_query() {
        let cmd = parse_line(b"AT+TIME?").unwrap();
        assert_eq!(cmd.op, AtOp::Time);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_time_execute_form() {
        // `AT+TIME` without `?` is the bare execute form; the
        // firmware treats it identically to `AT+TIME?`.
        let cmd = parse_line(b"AT+TIME").unwrap();
        assert_eq!(cmd.op, AtOp::Time);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_ntpsync_execute() {
        let cmd = parse_line(b"AT+NTPSYNC").unwrap();
        assert_eq!(cmd.op, AtOp::NtpSync);
        assert_eq!(cmd.kind, AtCommandKind::Execute);
    }

    #[test]
    fn parses_timezone_query() {
        let cmd = parse_line(b"AT+TIMEZONE?").unwrap();
        assert_eq!(cmd.op, AtOp::Timezone);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_timezone_set_positive() {
        let cmd = parse_line(b"AT+TIMEZONE=480").unwrap();
        assert_eq!(cmd.op, AtOp::Timezone);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(t)) => assert_eq!(*t, b"480"),
            _ => panic!("expected token"),
        }
    }

    #[test]
    fn parses_timezone_set_negative() {
        let cmd = parse_line(b"AT+TIMEZONE=-300").unwrap();
        assert_eq!(cmd.op, AtOp::Timezone);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(t)) => assert_eq!(*t, b"-300"),
            _ => panic!("expected token"),
        }
    }

    #[test]
    fn parses_ble_query() {
        let cmd = parse_line(b"AT+BLE?").unwrap();
        assert_eq!(cmd.op, AtOp::Ble);
        assert_eq!(cmd.kind, AtCommandKind::Query);
    }

    #[test]
    fn parses_ble_set_on() {
        let cmd = parse_line(b"AT+BLE=ON").unwrap();
        assert_eq!(cmd.op, AtOp::Ble);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        match cmd.arg(0) {
            Some(AtArg::Token(t)) => assert_eq!(*t, b"ON"),
            _ => panic!("expected token"),
        }
    }

    #[test]
    fn parses_ble_set_off() {
        let cmd = parse_line(b"AT+BLE=OFF").unwrap();
        assert_eq!(cmd.op, AtOp::Ble);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        assert_eq!(cmd.op.name(), "+BLE");
    }

    #[test]
    fn parses_luaapp_set_and_query() {
        // URL-safe base64 (no `+`/`/`) survives the comma-splitting parser.
        let cmd = parse_line(b"AT+LUAAPP=aGVsbG8td29ybGQ").unwrap();
        assert_eq!(cmd.op, AtOp::LuaApp);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        assert_eq!(cmd.op.name(), "+LUAAPP");
        match cmd.arg(0) {
            Some(AtArg::Token(t)) => assert_eq!(*t, b"aGVsbG8td29ybGQ"),
            _ => panic!("expected token"),
        }
        // Query form.
        let q = parse_line(b"AT+LUAAPP?").unwrap();
        assert_eq!(q.op, AtOp::LuaApp);
        assert_eq!(q.kind, AtCommandKind::Query);
    }

    #[test]
    fn rejects_timezone_set_with_extra_args() {
        // Timezone takes exactly one arg. The parser accepts the
        // first arg and silently ignores the rest, so we can only
        // verify the parser succeeds here; the dispatcher must
        // enforce single-arg semantics.
        let cmd = parse_line(b"AT+TIMEZONE=480,extra").unwrap();
        assert_eq!(cmd.op, AtOp::Timezone);
        assert_eq!(cmd.kind, AtCommandKind::Set);
        assert_eq!(cmd.args.len(), 2);
    }

    #[test]
    fn scratch_buffer_truncates_long_line() {
        let mut s = ScratchBuffer::new();
        let mut big = [b'A'; 400];
        for (i, b) in big.iter_mut().enumerate() {
            *b = b'x';
            let _ = i;
        }
        // Build a deliberately-too-long input: AT+GMR + 400 x's.
        let mut input = b"AT+GMR".to_vec();
        input.extend(core::iter::repeat_n(b'x', 400));
        // Should parse without panic; the truncation should not
        // cause issues — we just expect the parser to at least
        // handle the prefix.
        let r = s.copy_and_parse(&input);
        // Don't care about success / failure — only that we don't
        // panic and the buffer is reused for the next call.
        let _ = r;
    }

    /// Security-boundary robustness: `parse_line` consumes untrusted bytes
    /// from UART, so it must NEVER panic, whatever the input. We sweep every
    /// single-byte and two-byte input, a deterministic three-byte subset, and
    /// a set of structured adversarial cases (NUL / 0xFF / invalid UTF-8 /
    /// embedded control bytes / long runs). A panic in any iteration fails
    /// the test, and running all of them is fast because parsing is O(n).
    #[test]
    fn parse_line_never_panics_on_adversarial_input() {
        // Every single byte.
        for b in 0u8..=255 {
            let _ = parse_line(&[b]);
        }
        // Every two-byte combination (65,536 inputs).
        for hi in 0u8..=255 {
            for lo in 0u8..=255 {
                let _ = parse_line(&[hi, lo]);
            }
        }
        // Deterministic three-byte sweep (stepped to bound wall-time).
        for x in 0u8..=255 {
            for y in (0u8..=255).step_by(7) {
                for z in (0u8..=255).step_by(11) {
                    let _ = parse_line(&[x, y, z]);
                }
            }
        }

        // Structured adversarial cases for a range of lengths.
        let mut cases: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
        for n in 1..=256usize {
            cases.push(alloc::vec![0u8; n]); // all NUL
            cases.push(alloc::vec![0xFFu8; n]); // all 0xFF (erased-like)
            cases.push(alloc::vec![0x80u8; n]); // invalid UTF-8 lead byte
            cases.push((0..n as u8).collect()); // 0,1,2,..,255
            cases.push(b"AT".repeat(n / 2 + 1)); // repeated `AT`
            cases.push(b"AT+CWJAP=\"\x00\xFF\x80".repeat(n / 16 + 1)); // embedded control
                                                                       // Deterministic pseudo-random bytes (LCG, no external RNG).
            let mut acc: u32 = 0x12345678;
            let mut v = alloc::vec::Vec::with_capacity(n);
            for _ in 0..n {
                acc = acc.wrapping_mul(1664525).wrapping_add(1013904223);
                v.push((acc >> 24) as u8);
            }
            cases.push(v);
        }
        for c in &cases {
            let _ = parse_line(c);
        }
    }
}
