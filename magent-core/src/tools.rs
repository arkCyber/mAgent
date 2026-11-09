//! Embedded tools for mAgent
//!
//! Provides tool registry and execution for embedded operations:
//! - Sensor reading
//! - GPIO control
//! - Flash storage
//! - BLE communication

use crate::agent::{ToolCall, ToolResult};
use crate::error::{AgentError, Result};
use core::str::FromStr;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum number of tools
const MAX_TOOLS: usize = 16;

/// Largest prefix of `s` that is at most `max_bytes` long **and ends on a
/// UTF-8 character boundary**.
///
/// A bare `&s[..max_bytes]` panics ("byte index is not a char boundary")
/// whenever the cut lands inside a multi-byte code point — the common case,
/// not the rare one, for the Chinese/emoji/accented payloads that arrive via
/// LLM tool arguments and sensor labels.
fn clamp_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A real-hardware tool handler.
///
/// A chip layer (firmware / host) implements this to back `ToolRegistry`
/// tools with actual hardware (GPIO, sensors, flash, ...). Returning `None`
/// from [`Self::handle`] means "this target cannot run that tool": the
/// registry reports the failure to the agent rather than substituting a
/// simulated value (see [`ToolRegistry::execute`]). A handler therefore has
/// to cover every tool it advertises; one it cannot drive is refused, not
/// faked.
pub trait ToolHandler {
    /// Attempt to execute `call` against real hardware. Return `Some(result)`
    /// for a tool this target can run, or `None` for one it cannot — the
    /// registry will not fall back to a simulated value.
    fn handle(&self, call: &ToolCall) -> Option<ToolResult>;

    /// Whether this handler can actually run `name`.
    ///
    /// The registry advertises every *registered* tool to the model, but a
    /// real target implements only a subset: the ESP32 runs GPIO, its internal
    /// temperature sensor and flash, and has no heart-rate or glucose hardware
    /// at all. Returning `false` keeps that tool out of the LLM system prompt
    /// (see [`ToolRegistry::describe_served`]), so the model is not told it can
    /// read a sensor the board does not have.
    ///
    /// The default assumes a handler serves its whole registration, which is
    /// the behaviour before this hook existed and what a fully-implemented
    /// handler wants.
    fn serves(&self, name: &str) -> bool {
        let _ = name;
        true
    }

    /// How `name` should be described in the LLM tool list, or `None` to use
    /// the description the tool was registered with.
    ///
    /// [`Self::serves`] filters *which* tools a target advertises; this
    /// filters *what* a served tool claims. Some tools take an argument that
    /// names a capability — `read_sensor sensor=<name>` being the one that
    /// matters here — and a target can serve the tool while supporting only
    /// part of that argument space. The shared registry description for
    /// `read_sensor` lists twelve sensors; the ESP32 runs the tool but has no
    /// accelerometer, humidity, pressure, light, heart-rate, HRV, glucose, ECG,
    /// stress or battery hardware, so advertising the shared text promised ten
    /// readings the board answers "unsupported sensor" to. A handler that
    /// knows its real capability returns its own description here.
    ///
    /// The default returns `None`, so every existing handler and the simulated
    /// (handler-less) registry are unchanged.
    fn describe_tool(&self, name: &str) -> Option<&'static str> {
        let _ = name;
        None
    }
}

/// Tool registry
pub struct ToolRegistry {
    tools: Vec<Tool, MAX_TOOLS>,
    /// Optional real-hardware handler consulted before the simulated
    /// executors. `None` (the default) keeps the original mock behaviour;
    /// `Some` marks a real target, where an uncovered tool is refused rather
    /// than simulated.
    handler: Option<&'static dyn ToolHandler>,
}

/// Parse a `key=value,key=value` argument string into a tiny lookup table.
///
/// This is the single source of truth for argument parsing across all
/// the `execute_*` methods. It exists because the previous
/// `args.contains("key")` style silently mis-parsed inputs in two
/// realistic ways:
///
/// * Order coupling — `args = "hrv"` matched `heart_rate` first because
///   the verifier ran the `heart_rate` branch before the `hrv` branch.
///   `hrv` was therefore unreachable.
/// * Substring bleed — `args = "10"` matched `state=high` because the
///   raw `args` contained the substring `"1"`. Pin number `10` was
///   treated as `1` + `high` even though the user wanted `10` and the
///   default state.
///
/// The parser below accepts both `key=value` and bare tokens (so
/// `flash_read` still works with `address=0x1000`). Whitespace is
/// trimmed. Values are not unescaped — we surface raw strings to the
/// caller, who can decide whether to JSON-parse them.
pub(crate) fn parse_args(args: &str) -> Vec<(&str, &str), 8> {
    let mut out: Vec<(&str, &str), 8> = Vec::new();
    for part in args.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (k, v) = match trimmed.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => {
                // Bare token — store under a synthetic key so the
                // caller can still inspect it. We use the empty string
                // as the key to distinguish these from explicit `key=`
                // pairs.
                let key: &str = "";
                let value: &str = trimmed;
                let _ = out.push((key, value));
                continue;
            }
        };
        if !k.is_empty() {
            let _ = out.push((k, v));
        }
    }
    out
}

/// Pull the value for `key` from a parsed `Vec<(&str, &str)>`.
/// Returns `default` if the key is absent or the value is empty.
pub(crate) fn arg<'a>(
    args: &'a Vec<(&'a str, &'a str), 8>,
    key: &str,
    default: &'a str,
) -> &'a str {
    for &(k, v) in args.iter() {
        if k == key {
            return if v.is_empty() { default } else { v };
        }
    }
    default
}

/// Every canonical sensor name `read_sensor` resolves.
///
/// Single source of truth for the sensor vocabulary: the executor below,
/// the host simulator ([`crate::simulator`]) and the system prompt the
/// model is handed all have to agree on it, or a prompt tuned with
/// `--mock` behaves differently from the device it is tuned for. Each of
/// those places has a guard test that iterates this list, so a name added
/// here and nowhere else fails a test instead of silently diverging.
pub const SENSOR_NAMES: [&str; 12] = [
    "temperature",
    "accelerometer",
    "humidity",
    "pressure",
    "light",
    "heart_rate",
    "hrv",
    "glucose",
    "ecg",
    "stress",
    "battery",
    "memory",
];

/// Map a user-supplied sensor name (e.g. `"heartrate"`, `"HRV"`,
/// `"temp"`) to the canonical token used by the simulated value
/// table. The previous flat `if/else` chain relied on input order,
/// which meant `hrv` was unreachable once `heart_rate` was checked
/// first. The new lookup has no order coupling and accepts the
/// common aliases.
fn normalize_sensor(name: &str) -> &str {
    match name.trim().to_ascii_lowercase().as_str() {
        "temperature" | "temp" | "die_temp" => "temperature",
        "accelerometer" | "accel" | "imu" => "accelerometer",
        "humidity" | "humid" => "humidity",
        "pressure" | "press" | "baro" => "pressure",
        "light" | "lux" | "als" => "light",
        "heart_rate" | "heartrate" | "hr" | "pulse" => "heart_rate",
        "hrv" | "hrv_ms" => "hrv",
        "glucose" | "bg" | "blood_glucose" => "glucose",
        "ecg" | "ekg" => "ecg",
        "stress" | "stress_level" => "stress",
        "battery" | "batt" => "battery",
        "memory" | "mem" | "heap" | "free_heap" | "sram" => "memory",
        _ => name,
    }
}

/// Pre-rendered numbers for the common 0–9999 range. Anything
/// above that gets surfaced as `">9999"` so the result string
/// stays bounded. We pre-render instead of using `format!` because
/// the embedded build needs to avoid the `format!` allocator path.
fn pin_to_str(pin: u32) -> &'static str {
    match pin {
        0 => "0",
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        9 => "9",
        10 => "10",
        11 => "11",
        12 => "12",
        13 => "13",
        14 => "14",
        15 => "15",
        16 => "16",
        17 => "17",
        18 => "18",
        19 => "19",
        20 => "20",
        21 => "21",
        22 => "22",
        23 => "23",
        24 => "24",
        25 => "25",
        26 => "26",
        27 => "27",
        28 => "28",
        29 => "29",
        30 => "30",
        31 => "31",
        32 => "32",
        33 => "33",
        34 => "34",
        35 => "35",
        36 => "36",
        37 => "37",
        38 => "38",
        39 => "39",
        40 => "40",
        41 => "41",
        42 => "42",
        43 => "43",
        44 => "44",
        45 => "45",
        46 => "46",
        47 => "47",
        48 => "48",
        49 => "49",
        50 => "50",
        51 => "51",
        52 => "52",
        53 => "53",
        54 => "54",
        55 => "55",
        56 => "56",
        57 => "57",
        58 => "58",
        59 => "59",
        60 => "60",
        61 => "61",
        62 => "62",
        63 => "63",
        64 => "64",
        65 => "65",
        66 => "66",
        67 => "67",
        68 => "68",
        69 => "69",
        70 => "70",
        71 => "71",
        72 => "72",
        73 => "73",
        74 => "74",
        75 => "75",
        76 => "76",
        77 => "77",
        78 => "78",
        79 => "79",
        80 => "80",
        81 => "81",
        82 => "82",
        83 => "83",
        84 => "84",
        85 => "85",
        86 => "86",
        87 => "87",
        88 => "88",
        89 => "89",
        90 => "90",
        91 => "91",
        92 => "92",
        93 => "93",
        94 => "94",
        95 => "95",
        96 => "96",
        97 => "97",
        98 => "98",
        99 => "99",
        100 => "100",
        200 => "200",
        256 => "256",
        512 => "512",
        1000 => "1000",
        1024 => "1024",
        2048 => "2048",
        4096 => "4096",
        8192 => "8192",
        9999 => "9999",
        _ => ">9999",
    }
}

/// Try to parse a value as a `T` via `FromStr`. Falls back to
/// `default` if the conversion fails. Used for numeric args like
/// pin numbers, addresses, and lengths where a malformed string
/// shouldn't crash the agent.
fn arg_parse<T: FromStr>(args: &Vec<(&str, &str), 8>, key: &str, default: T) -> T {
    for &(k, v) in args.iter() {
        if k == key {
            if let Ok(parsed) = v.parse::<T>() {
                return parsed;
            }
            return default;
        }
    }
    default
}

/// Parse a tool argument the call has to name. An absent key is a *missing
/// field* and a value that will not parse is an *invalid* one. `arg_parse`'s
/// `default` silently substituted `0` for an absent `pin`/`address`, turning a
/// malformed call into a real action on pin 0 or flash address 0.
fn required_arg_parse<T: FromStr>(args: &Vec<(&str, &str), 8>, key: &'static str) -> Result<T> {
    match args.iter().find(|(k, _)| *k == key) {
        None => Err(AgentError::ConfigurationError {
            field: key,
            reason: crate::error::ConfigError::MissingField,
        }),
        Some((_, v)) => v.parse::<T>().map_err(|_| AgentError::ConfigurationError {
            field: key,
            reason: crate::error::ConfigError::InvalidValue,
        }),
    }
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            handler: None,
        }
    }

    /// Install a real-hardware handler. Tools it can handle are executed
    /// against real hardware instead of the built-in simulation.
    pub fn set_handler(&mut self, handler: &'static dyn ToolHandler) {
        self.handler = Some(handler);
    }

    /// Register a tool
    pub fn register(&mut self, tool: Tool) -> Result<()> {
        if self.tools.len() >= MAX_TOOLS {
            return Err(AgentError::MemoryAllocationFailed {
                requested: 1,
                available: 0,
            });
        }
        self.tools
            .push(tool)
            .map_err(|_| AgentError::MemoryAllocationFailed {
                requested: 1,
                available: 0,
            })?;
        Ok(())
    }

    /// Execute a tool call
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        // Let a real-hardware handler (if installed) take precedence over the
        // built-in simulated executors.
        if let Some(handler) = self.handler {
            if let Some(result) = handler.handle(call) {
                return Ok(result);
            }

            // A handler is installed, so this registry backs a *real* target.
            // The simulated executors below fabricate plausible success for
            // hardware the target may not have at all: `flash_write` answers
            // "Wrote N bytes to address X" without persisting a byte,
            // `ble_send` answers "Sent N bytes via BLE" on a firmware that has
            // no BLE stack, and the vital-sign sensors answer with invented
            // numbers. The agent then reports that to the user as fact — the
            // tool list in the LLM prompt even names them, so it will call
            // them. A tool the target cannot actually run has to say so.
            //
            // Only registered names reach here; an unknown tool still gets the
            // `MissingField` error below.
            if self.tools.iter().any(|t| t.name == call.name) {
                let mut msg = heapless::String::<64>::new();
                let _ = msg.push_str("not implemented on this target");
                return Ok(ToolResult {
                    tool_name: call.name.clone(),
                    data: heapless::String::new(),
                    success: false,
                    error: Some(msg),
                });
            }
        }

        // Find tool
        let tool = self.tools.iter().find(|t| t.name == call.name).ok_or(
            AgentError::ConfigurationError {
                field: "tool",
                reason: crate::error::ConfigError::MissingField,
            },
        )?;

        // Execute based on tool type
        match tool.tool_type {
            ToolType::ReadSensor => self.execute_read_sensor(&call.arguments).await,
            ToolType::WriteGpio => self.execute_write_gpio(&call.arguments).await,
            ToolType::FlashRead => self.execute_flash_read(&call.arguments).await,
            ToolType::FlashWrite => self.execute_flash_write(&call.arguments).await,
            ToolType::BleSend => self.execute_ble_send(&call.arguments).await,
            ToolType::ReadHeartRate => {
                self.execute_read_sensor_as("heart_rate", &call.arguments)
                    .await
            }
            ToolType::ReadGlucose => {
                self.execute_read_sensor_as("glucose", &call.arguments)
                    .await
            }
            ToolType::ReadEcg => self.execute_read_sensor_as("ecg", &call.arguments).await,
            ToolType::StressLevel => self.execute_read_sensor_as("stress", &call.arguments).await,
            ToolType::VoiceOutput => self.execute_voice_output(&call.arguments).await,
            ToolType::SendNotification => self.execute_send_notification(&call.arguments).await,
            // Network tools have no simulation: they are only meaningful when a
            // real-hardware handler (see `set_handler`) executed them above. If
            // we get here the target registered a web tool without a handler —
            // surface that as a configuration error instead of inventing a body.
            ToolType::WebFetch => Err(AgentError::ConfigurationError {
                field: "tool",
                reason: crate::error::ConfigError::NotConfigured,
            }),
        }
    }

    /// Execute voice output. Parses `text=...` and `priority=...` from
    /// the structured args. The previous implementation ignored all
    /// arguments and always returned a fixed placeholder string, so
    /// the LLM had no way to deliver user-specified text.
    async fn execute_voice_output(&self, args: &str) -> Result<ToolResult> {
        let parsed = parse_args(args);
        let text = arg(&parsed, "text", "");
        let priority = match arg(&parsed, "priority", "normal") {
            "low" => "low",
            "high" => "high",
            "urgent" => "urgent",
            _ => "normal",
        };

        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Voice queued (priority=");
        let _ = result.push_str(priority);
        let _ = result.push_str("): ");
        // Trim the text to fit in our bounded buffer. We don't want
        // truncation to look like a panic, so we just clamp — the
        // LLM doesn't need byte-level accuracy to know what was said.
        let room = result.capacity() - result.len();
        let _ = result.push_str(clamp_utf8(text, room));

        Ok(ToolResult {
            tool_name: heapless::String::try_from("voice_output").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute send notification. Parses `text=...` and
    /// `priority=...` from the structured args. The previous
    /// implementation always returned `"Notification sent"` with no
    /// regard for the user-supplied payload.
    async fn execute_send_notification(&self, args: &str) -> Result<ToolResult> {
        let parsed = parse_args(args);
        let text = arg(&parsed, "text", "");
        let priority = match arg(&parsed, "priority", "normal") {
            "low" => "low",
            "high" => "high",
            "urgent" => "urgent",
            _ => "normal",
        };

        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Notification (priority=");
        let _ = result.push_str(priority);
        let _ = result.push_str("): ");
        let room = result.capacity() - result.len();
        let _ = result.push_str(clamp_utf8(text, room));

        Ok(ToolResult {
            tool_name: heapless::String::try_from("send_notification").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute sensor read
    async fn execute_read_sensor(&self, args: &str) -> Result<ToolResult> {
        self.execute_read_sensor_as("", args).await
    }

    /// Execute a sensor read on behalf of an alias tool
    /// (`read_heart_rate`, `read_glucose`, `read_ecg`) that the system prompt
    /// documents as taking no arguments. `implied` is the sensor the alias
    /// stands for; it is used only when the caller named no sensor, so an
    /// explicit `sensor=` argument still wins.
    async fn execute_read_sensor_as(&self, implied: &str, args: &str) -> Result<ToolResult> {
        // Parse the args once with the shared parser so the sensor
        // keywords are matched against the *value* of `sensor=...` (or
        // a bare word) rather than against the whole input string.
        // The previous `args.contains("heart_rate")` chain matched
        // `hrv` here too because both substrings live in the combined
        // `"72 BPM, HRV:55ms"` value — routing every read to the heart
        // rate branch. Parsing the value avoids that.
        let parsed = parse_args(args);
        let sensor = {
            // Prefer an explicit `sensor=` key, fall back to the first
            // bare token, then to the alias's implied sensor, then to
            // "" if none of those is present.
            let explicit = arg(&parsed, "sensor", "");
            if !explicit.is_empty() {
                explicit
            } else {
                // First bare token (no `=`).
                let mut iter = parsed.iter().filter(|(k, _)| k.is_empty());
                match iter.next() {
                    Some((_, v)) => *v,
                    None => implied,
                }
            }
        };

        let sensor = match sensor {
            "" => {
                return Ok(ToolResult {
                    tool_name: heapless::String::try_from("read_sensor").unwrap(),
                    data: heapless::String::try_from("Unknown sensor").unwrap(),
                    success: false,
                    error: Some(heapless::String::try_from("Invalid sensor type").unwrap()),
                });
            }
            s => normalize_sensor(s),
        };

        // In real implementation, this would:
        // 1. Initialize I2C/SPI bus
        // 2. Send sensor command
        // 3. Read sensor data
        // 4. Parse and return value

        // Simulate sensor read with realistic values.
        let value = match sensor {
            "temperature" => "25.5°C",
            "accelerometer" => "X:0.1 Y:0.2 Z:9.8",
            "humidity" => "65%",
            "pressure" => "1013 hPa",
            "light" => "500 lux",
            "heart_rate" => "72 BPM, HRV:55ms",
            "hrv" => "55ms, Stress:Moderate",
            "glucose" => "105 mg/dL, Trend:Rising",
            "ecg" => "HR:72, Rhythm:Normal",
            "stress" => "Level:Moderate, HRV:55ms",
            "battery" => "Battery:85%, Voltage:3700mV",
            "memory" => "free_heap=128000 B min_free_heap=118000 B",
            _ => {
                return Ok(ToolResult {
                    tool_name: heapless::String::try_from("read_sensor").unwrap(),
                    data: heapless::String::try_from("Unknown sensor").unwrap(),
                    success: false,
                    error: Some(heapless::String::try_from("Invalid sensor type").unwrap()),
                });
            }
        };

        // HARDENING (audit-2026-08 H7): previously this was
        // `heapless::String::try_from(value).unwrap()`. A caller-
        // controlled `value` longer than 256 bytes would panic the
        // agent rather than be reported as a tool error. We now
        // truncate at the boundary, log a warning, and return a
        // successful result with the truncated payload — the
        // alternative would be a panic in a worker thread that
        // kills the whole MiniAgent loop. ToolResult.data is
        // 256 bytes; we cap at 240 to leave room for a "..."
        // marker.
        let mut data_buf: heapless::String<256> = heapless::String::new();
        let truncated = if value.len() > 240 {
            log::warn!(
                "[tools] read_sensor data {} bytes exceeds ToolResult.data=256; \
                 truncating to 240 chars",
                value.len()
            );
            // Truncate at the largest character boundary at or below 240
            // bytes: a raw `&value[..240]` would panic if a multi-byte
            // codepoint (e.g. `°`) straddled the cut, killing the worker
            // thread instead of producing a truncated result.
            clamp_utf8(value, 240)
        } else {
            value
        };
        let _ = data_buf.push_str(truncated);
        Ok(ToolResult {
            tool_name: heapless::String::try_from("read_sensor").unwrap(),
            data: data_buf,
            success: true,
            error: None,
        })
    }

    /// Execute GPIO write
    async fn execute_write_gpio(&self, args: &str) -> Result<ToolResult> {
        // Parse `pin=XX,state=YY`. The previous implementation decided
        // `state` by substring-matching the whole args string, which
        // meant `args="10"` (pin 10) was read as `state=high` because
        // `"1"` appears in `"10"`. Parsing the structured args side-
        // steps that completely.
        let parsed = parse_args(args);
        let pin = required_arg_parse::<u32>(&parsed, "pin")?;
        // State must be a single token. We accept the explicit
        // `state=high|low|1|0` form; bare numeric values are NOT
        // treated as state (that's the whole point of the parse).
        let state = match arg(&parsed, "state", "high") {
            "high" | "1" => "high",
            "low" | "0" => "low",
            other => {
                return Ok(ToolResult {
                    tool_name: heapless::String::try_from("write_gpio").unwrap(),
                    data: heapless::String::try_from("Invalid state").unwrap(),
                    success: false,
                    // The `try_from` is intentionally fallible here:
                    // `other` is the raw GPIO state string supplied
                    // by the caller, which can be any length. If it
                    // overflows our 64-byte error buffer we surface
                    // an empty `error` rather than panic. Clippy's
                    // `unnecessary_fallible_conversions` lint
                    // mistakes the `&str` prefix (a known-length
                    // literal) for the whole expression.
                    #[allow(clippy::unnecessary_fallible_conversions)]
                    error: Some(
                        heapless::String::try_from("expected state=high|low|1|0, got ")
                            .ok()
                            .map(|mut s| {
                                let _ = s.push_str(other);
                                s
                            })
                            .unwrap_or_default(),
                    ),
                });
            }
        };

        // In real implementation, this would:
        // 1. Configure GPIO pin as output
        // 2. Set pin state using embedded-hal
        // 3. Return confirmation

        // Manual string construction to avoid format! macro.
        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Pin ");
        let _ = result.push_str(pin_to_str(pin));
        let _ = result.push_str(" set to ");
        let _ = result.push_str(state);

        Ok(ToolResult {
            tool_name: heapless::String::try_from("write_gpio").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute flash read
    async fn execute_flash_read(&self, args: &str) -> Result<ToolResult> {
        // Parse arguments (format: "address=XX,length=YY") using the
        // shared parser so numeric values are validated.
        let parsed = parse_args(args);
        let address = required_arg_parse::<u32>(&parsed, "address")?;
        let length = arg_parse::<u32>(&parsed, "length", 256);

        // In real implementation, this would read from flash
        // For now, simulate with realistic data

        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Read ");
        let _ = result.push_str(pin_to_str(length));
        let _ = result.push_str(" bytes from address ");
        let _ = result.push_str(pin_to_str(address));

        Ok(ToolResult {
            tool_name: heapless::String::try_from("flash_read").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute flash write
    async fn execute_flash_write(&self, args: &str) -> Result<ToolResult> {
        // Parse arguments (format: "address=XX,data=YY") using the
        // shared parser so `data_len` is computed from the raw value
        // length rather than from a substring search.
        let parsed = parse_args(args);
        let address = required_arg_parse::<u32>(&parsed, "address")?;
        let data_len = arg(&parsed, "data", "").len() as u32;

        // In real implementation, this would write to flash
        // For now, simulate with realistic data

        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Wrote ");
        let _ = result.push_str(pin_to_str(data_len));
        let _ = result.push_str(" bytes to address ");
        let _ = result.push_str(pin_to_str(address));

        Ok(ToolResult {
            tool_name: heapless::String::try_from("flash_write").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute BLE send
    async fn execute_ble_send(&self, args: &str) -> Result<ToolResult> {
        // Parse arguments (format: "data=XX,characteristic=YY") using
        // the shared parser. The previous implementation dropped the
        // user-supplied `data=` payload and used the wrong substring
        // when the data contained a comma.
        let parsed = parse_args(args);
        let data_len = arg(&parsed, "data", "").len() as u32;
        let characteristic = arg(&parsed, "characteristic", "default");

        // In real implementation, this would send via BLE
        // For now, simulate with realistic data

        let mut result = heapless::String::<256>::new();
        let _ = result.push_str("Sent ");
        let _ = result.push_str(pin_to_str(data_len));
        let _ = result.push_str(" bytes via BLE to ");
        let _ = result.push_str(characteristic);

        Ok(ToolResult {
            tool_name: heapless::String::try_from("ble_send").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Get all registered tools
    pub fn all_tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Get tool count
    pub fn count(&self) -> usize {
        self.tools.len()
    }

    /// Check if tool exists
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name.as_str() == name)
    }

    /// Return a human-readable description of every registered tool,
    /// one tool per line. The LLM uses this string during prompt
    /// construction so it knows which tools are available and what
    /// arguments they accept.
    ///
    /// The format is intentionally simple and stable:
    ///
    /// ```text
    /// - <name>: <description>
    /// ```
    ///
    /// Tools without a description are skipped. This is what the
    /// built-in tools populate, so in practice the registry is always
    /// fully described.
    /// PATCHED (MicroAgent): capacity 1024 → 2048 B and **never truncate
    /// silently**.
    ///
    /// The caller uses this string as the "Available tools:" section of the LLM
    /// system prompt. With the built-in hardware pack plus four network tools
    /// (`fetch_web`, `get_weather`, `get_news`, `get_flight_price`) the list is
    /// ~1.06 kB, so the previous 1 kB buffer dropped the *last* tool without a
    /// word — the model then truthfully answered "I don't have that tool".
    /// Each entry is now committed only if it fits as a whole (a half-entry could
    /// never be parsed by the model anyway) and any omission is logged.
    pub fn describe(&self) -> String<2048> {
        self.describe_filtered(false)
    }

    /// Like [`Self::describe`], but lists only the tools the installed handler
    /// can actually serve ([`ToolHandler::serves`]). With no handler installed
    /// (the simulated default) this is identical to `describe`.
    ///
    /// This is what the LLM system prompt must use. A tool that is advertised
    /// but unservable is a promise the model will act on and the executor will
    /// refuse: on the ESP32 that covered seven of the fifteen advertised names
    /// (`ble_send`, the health sensors, `voice_output`, `send_notification`),
    /// none of which the board has hardware for.
    pub fn describe_served(&self) -> String<2048> {
        self.describe_filtered(true)
    }

    fn describe_filtered(&self, served_only: bool) -> String<2048> {
        let mut out = String::<2048>::new();
        let mut dropped = 0usize;
        let mut unserved = 0usize;
        let mut described = 0usize;
        for tool in self.tools.iter() {
            if tool.description.is_empty() {
                continue;
            }
            // A handler may override *what* a served tool claims (see
            // [`ToolHandler::describe_tool`]); that override is part of the
            // served-view contract, so only apply it when filtering.
            let mut description = tool.description.as_str();
            if served_only {
                if let Some(handler) = self.handler {
                    if !handler.serves(tool.name.as_str()) {
                        unserved += 1;
                        continue;
                    }
                    if let Some(override_desc) = handler.describe_tool(tool.name.as_str()) {
                        description = override_desc;
                        described += 1;
                    }
                }
            }
            let before = out.len();
            let fits = {
                let mut ok = true;
                if !out.is_empty() {
                    ok &= out.push('\n').is_ok();
                }
                ok &= out.push_str("- ").is_ok();
                ok &= out.push_str(tool.name.as_str()).is_ok();
                ok &= out.push_str(": ").is_ok();
                ok &= out.push_str(description).is_ok();
                ok
            };
            if !fits {
                out.truncate(before);
                dropped += 1;
            }
        }
        if dropped > 0 {
            log::warn!(
                "[tools] describe(): {dropped} tool(s) omitted — the description buffer (2048 B) is full; \
                 the LLM prompt will not advertise them"
            );
        }
        if unserved > 0 {
            log::info!(
                "[tools] describe_served(): {unserved} registered tool(s) not served by the installed \
                 handler — the LLM prompt will not advertise them"
            );
        }
        if described > 0 {
            log::info!(
                "[tools] describe_served(): {described} tool description(s) overridden by the installed \
                 handler to match the hardware it actually has"
            );
        }
        out
    }

    /// Convenience: list the registered tool names in declaration
    /// order. Useful for system prompts when the LLM only needs
    /// to know *what* is available, not *what each does*.
    pub fn names(&self) -> String<512> {
        let mut out = String::<512>::new();
        for (i, tool) in self.tools.iter().enumerate() {
            if i > 0 {
                let _ = out.push_str(", ");
            }
            let _ = out.push_str(tool.name.as_str());
        }
        out
    }
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Tool name
    pub name: String<32>,
    /// Tool description
    pub description: String<128>,
    /// Tool type
    pub tool_type: ToolType,
}

/// Tool type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ToolType {
    /// Read sensor
    ReadSensor = 0,
    /// Write GPIO
    WriteGpio = 1,
    /// Read from flash
    FlashRead = 2,
    /// Write to flash
    FlashWrite = 3,
    /// Send via BLE
    BleSend = 4,
    /// Read heart rate
    ReadHeartRate = 5,
    /// Read glucose
    ReadGlucose = 6,
    /// Read ECG
    ReadEcg = 7,
    /// Voice/TTS output
    VoiceOutput = 8,
    /// Send notification
    SendNotification = 9,
    /// Read the stress level (alias for `read_sensor sensor=stress`)
    StressLevel = 11,
    /// Network fetch (HTTP GET) — e.g. `fetch_web` / `get_weather` /
    /// `get_news`. These have no simulation: they are executed by the
    /// target's real-hardware [`ToolHandler`] (the ESP32 firmware implements
    /// them over `esp_http_client`; the host CLI over its own HTTP stack), so
    /// a registry that advertises one without a handler fails loudly instead of
    /// returning a fabricated body.
    WebFetch = 10,
}

/// Sensor type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorType {
    /// Ambient temperature (°C).
    Temperature,
    /// 3-axis accelerometer (m/s²).
    Accelerometer,
    /// 3-axis angular-rate gyroscope (°/s).
    Gyroscope,
    /// 3-axis magnetometer (μT, compass heading).
    Magnetometer,
    /// Relative humidity (%RH).
    Humidity,
    /// Atmospheric pressure (hPa).
    Pressure,
    /// Ambient light (lux).
    Light,
    /// Instantaneous heart rate (BPM).
    HeartRate,
    /// Heart-rate variability (RMSSD, ms).
    Hrv,
    /// Blood glucose (mg/dL).
    Glucose,
    /// Single-lead ECG waveform.
    Ecg,
    /// Stress level index.
    Stress,
}

/// GPIO pin state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioState {
    /// Logic-low (0 V).
    Low,
    /// Logic-high (Vcc).
    High,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::try_heapless;
    use crate::tools::{Tool, ToolType};

    #[test]
    fn clamp_utf8_returns_short_input_verbatim() {
        assert_eq!(clamp_utf8("hello", 100), "hello");
        assert_eq!(clamp_utf8("", 0), "");
        // Exactly at the limit is still "fits".
        assert_eq!(clamp_utf8("abcd", 4), "abcd");
    }

    #[test]
    fn clamp_utf8_never_lands_inside_a_codepoint() {
        // Each `°` is 2 bytes; `天` is 3. Ask for a cut in the middle of one.
        let s = "°°°°"; // 8 bytes
        assert_eq!(clamp_utf8(s, 3), "°"); // byte 3 is inside the 2nd char
        let t = "天天地"; // 3 + 3 + 3
        assert_eq!(clamp_utf8(t, 4), "天");
        assert_eq!(clamp_utf8(t, 1), "");
    }

    /// The actual bug: `execute_voice_output` / `execute_send_notification`
    /// did `&text[..min(text.len(), room)]`. A Chinese utterance longer than
    /// the 256-byte buffer pushed the cut inside a 3-byte code point and
    /// panicked the worker thread.
    #[test]
    fn voice_output_with_long_chinese_text_does_not_panic() {
        let text = "温度".repeat(200); // 1200 bytes, well past the 256-byte cap
        let out = clamp_utf8(&text, 256);
        assert!(out.len() <= 256);
        assert!(text.starts_with(out));
        // Every returned byte must be valid UTF-8 (i.e. we cut on a boundary).
        assert!(core::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// Build a populated registry identical to what
    /// `register_builtin_tools` produces, so we can exercise
    /// `describe()` and `names()` against a realistic state.
    fn populated_registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        let entries: &[(&str, &str, ToolType)] = &[
            ("read_sensor", "Read a sensor value", ToolType::ReadSensor),
            ("write_gpio", "Set a GPIO pin high/low", ToolType::WriteGpio),
            (
                "flash_read",
                "Read bytes from internal flash",
                ToolType::FlashRead,
            ),
            (
                "flash_write",
                "Write bytes to internal flash",
                ToolType::FlashWrite,
            ),
            ("ble_send", "Send a payload over BLE", ToolType::BleSend),
            (
                "voice_output",
                "Queue a text-to-speech utterance",
                ToolType::VoiceOutput,
            ),
            (
                "send_notification",
                "Send a smartwatch notification",
                ToolType::SendNotification,
            ),
            // Registered by `register_builtin_tools` on real targets; the
            // fixture has to carry them or the alias dispatch is untested.
            (
                "read_heart_rate",
                "Read heart rate (alias for read_sensor sensor=heart_rate)",
                ToolType::ReadHeartRate,
            ),
            (
                "read_glucose",
                "Read glucose level (alias for read_sensor sensor=glucose)",
                ToolType::ReadGlucose,
            ),
            (
                "read_ecg",
                "Read ECG trace (alias for read_sensor sensor=ecg)",
                ToolType::ReadEcg,
            ),
            (
                "check_stress_level",
                "Read stress level (alias for read_sensor sensor=stress)",
                ToolType::StressLevel,
            ),
        ];
        for (name, desc, ty) in entries {
            // HARDENING (audit-2026-08 unwrap sweep): the previous
            // `String::try_from(*name).unwrap()` would panic if a
            // future contributor added a tool name longer than the
            // `String<32>` boundary. `try_heapless` truncates with
            // a warning instead, keeping the static initializer
            // crash-free even if a string is bumped past the limit.
            let tool = Tool {
                name: try_heapless::<32>(name),
                description: try_heapless::<128>(desc),
                tool_type: *ty,
            };
            // HARDENING (audit-2026-08 unwrap sweep):
            // `register` can fail (duplicate tool name). The
            // previous code panicked the whole registry on a name
            // collision; we now log and continue so other tools
            // still get registered.
            if let Err(e) = r.register(tool) {
                log::warn!("[tools] duplicate tool name `{name}`: {e}");
            }
        }
        r
    }

    // ---- parse_args + arg + arg_parse ----

    #[test]
    fn parse_args_handles_key_value_pairs() {
        let parsed = parse_args("pin=10,state=low");
        assert_eq!(arg(&parsed, "pin", "0"), "10");
        assert_eq!(arg(&parsed, "state", "high"), "low");
        assert_eq!(arg(&parsed, "missing", "fallback"), "fallback");
    }

    #[test]
    fn parse_args_trims_whitespace() {
        let parsed = parse_args("  pin = 12 , state = high ");
        assert_eq!(arg(&parsed, "pin", "0"), "12");
        assert_eq!(arg(&parsed, "state", "low"), "high");
    }

    #[test]
    fn parse_args_accepts_bare_tokens() {
        let parsed = parse_args("hrv");
        // Bare tokens are stored under the empty key. The exact
        // iteration order is irrelevant for the bug we care about;
        // what matters is that we surface the token at all.
        assert!(parsed.iter().any(|(k, v)| k.is_empty() && *v == "hrv"));
    }

    #[test]
    fn parse_args_ignores_empty_segments() {
        let parsed = parse_args(",,pin=5,,,state=low,,");
        assert_eq!(arg(&parsed, "pin", "0"), "5");
        assert_eq!(arg(&parsed, "state", "high"), "low");
        // 8 segments max, but only real pairs/values should be stored.
        assert!(parsed.len() <= 8);
    }

    #[test]
    fn arg_parse_falls_back_on_bad_input() {
        let parsed = parse_args("pin=abc,length=oops");
        assert_eq!(arg_parse::<u32>(&parsed, "pin", 0), 0);
        assert_eq!(arg_parse::<u32>(&parsed, "length", 256), 256);
        assert_eq!(arg_parse::<u32>(&parsed, "missing", 42), 42);
    }

    // ---- normalize_sensor ----

    #[test]
    fn normalize_sensor_accepts_hrv_despite_heart_rate_alias() {
        // Regression: the old flat `if/else` chain had `heart_rate`
        // before `hrv`, so `hrv` was unreachable. The new dispatcher
        // has no order coupling.
        assert_eq!(normalize_sensor("hrv"), "hrv");
        assert_eq!(normalize_sensor("HRV"), "hrv");
        assert_eq!(normalize_sensor("hrv_ms"), "hrv");
    }

    #[test]
    fn normalize_sensor_accepts_common_aliases() {
        assert_eq!(normalize_sensor("heart_rate"), "heart_rate");
        assert_eq!(normalize_sensor("heartrate"), "heart_rate");
        assert_eq!(normalize_sensor("HR"), "heart_rate");
        assert_eq!(normalize_sensor("temp"), "temperature");
        assert_eq!(normalize_sensor("accel"), "accelerometer");
        assert_eq!(normalize_sensor("bg"), "glucose");
        assert_eq!(normalize_sensor("battery"), "battery");
    }

    // ---- describe() ----

    #[test]
    fn describe_returns_one_line_per_tool() {
        let r = populated_registry();
        let desc = r.describe();
        assert!(desc.as_str().contains("read_sensor: Read a sensor value"));
        assert!(desc
            .as_str()
            .contains("write_gpio: Set a GPIO pin high/low"));
        assert!(desc
            .as_str()
            .contains("flash_read: Read bytes from internal flash"));
        assert!(desc.as_str().contains("ble_send: Send a payload over BLE"));
        assert!(desc
            .as_str()
            .contains("voice_output: Queue a text-to-speech utterance"));
        assert!(desc
            .as_str()
            .contains("send_notification: Send a smartwatch notification"));
        // Newline separated. We have 11 entries.
        let line_count = desc.as_str().matches('\n').count() + 1;
        assert_eq!(line_count, 11);
    }

    #[test]
    fn describe_skips_tools_without_description() {
        let mut r = ToolRegistry::new();
        let _ = r.register(Tool {
            name: String::try_from("anonymous").unwrap(),
            description: String::new(),
            tool_type: ToolType::ReadSensor,
        });
        let _ = r.register(Tool {
            name: String::try_from("described").unwrap(),
            description: String::try_from("has a description").unwrap(),
            tool_type: ToolType::ReadSensor,
        });
        let desc = r.describe();
        assert_eq!(desc.as_str(), "- described: has a description");
    }

    #[test]
    fn names_returns_comma_separated_list() {
        let r = populated_registry();
        let names = r.names();
        // Names are in registration order.
        let expected = "read_sensor, write_gpio, flash_read, flash_write, ble_send, \
                        voice_output, send_notification, read_heart_rate, read_glucose, \
                        read_ecg, check_stress_level";
        assert_eq!(names.as_str(), expected);
    }

    // ---- execute_* argument parsing (regression for the 4 backlog bugs) ----

    fn make_call(tool: &str, args: &str) -> ToolCall {
        ToolCall {
            name: String::try_from(tool).unwrap(),
            arguments: String::try_from(args).unwrap(),
        }
    }

    fn run_async<F>(fut: F) -> crate::error::Result<ToolResult>
    where
        F: core::future::Future<Output = crate::error::Result<ToolResult>>,
    {
        // Drive the embedded async fn on the host using a tiny
        // executor shim. The production code never runs this — it
        // runs on the executor baked into the firmware — but the
        // unit tests need *some* way to .await a value.
        futures::executor::block_on(fut)
    }

    /// The prompt advertises `read_heart_rate()`, `read_glucose()` and
    /// `read_ecg()` with **no arguments**, and `register_builtin_tools`
    /// registers them as aliases for `read_sensor`. The dispatch handed
    /// `execute_read_sensor` the caller's empty argument string, so the alias
    /// never supplied the sensor it is an alias *for* and every advertised
    /// zero-arg call answered "Unknown sensor".
    #[test]
    fn sensor_aliases_supply_their_own_sensor() {
        let r = populated_registry();
        for (tool, needle) in [
            ("read_heart_rate", "BPM"),
            ("read_glucose", "mg/dL"),
            ("read_ecg", "Rhythm"),
            ("check_stress_level", "Moderate"),
        ] {
            let out = run_async(r.execute(&make_call(tool, ""))).unwrap();
            assert!(
                out.success,
                "{tool}() with no args must resolve, got {:?}",
                out.error
            );
            assert!(
                out.data.as_str().contains(needle),
                "{tool} produced {}",
                out.data.as_str()
            );
        }
        // An explicit sensor still wins over the alias's implied one.
        let out = run_async(r.execute(&make_call("read_heart_rate", "sensor=hrv"))).unwrap();
        assert!(out.success);
        assert!(out.data.as_str().contains("55ms"), "got {}", out.data);
    }

    #[test]
    fn execute_read_sensor_with_structured_args() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", "sensor=hrv"))).unwrap();
        assert!(out.success);
        // `hrv` now reaches the right branch — the previous bug
        // routed it to `heart_rate` because `contains("heart_rate")`
        // ran first.
        assert!(out.data.as_str().contains("55ms"));
        assert!(!out.data.as_str().contains("BPM"));
    }

    #[test]
    fn execute_read_sensor_with_bare_token() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", "heart_rate"))).unwrap();
        assert!(out.success);
        assert!(out.data.as_str().contains("BPM"));
    }

    #[test]
    fn execute_read_sensor_returns_temperature() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", "sensor=temperature"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "25.5°C");
    }

    #[test]
    fn execute_write_gpio_pin_ten_is_not_state_high() {
        // Regression: pin=10 used to be parsed as pin=1 with state=high
        // because the substring "1" inside "10" matched the high branch.
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("write_gpio", "pin=10,state=low"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Pin 10 set to low");
    }

    #[test]
    fn execute_write_gpio_default_state_is_high() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("write_gpio", "pin=3"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Pin 3 set to high");
    }

    #[test]
    fn execute_write_gpio_rejects_invalid_state() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("write_gpio", "pin=5,state=wobble"))).unwrap();
        assert!(!out.success);
        assert!(out.error.is_some());
    }

    #[test]
    fn execute_voice_output_includes_text_and_priority() {
        let r = populated_registry();
        let out =
            run_async(r.execute(&make_call("voice_output", "text=Drink water,priority=high")))
                .unwrap();
        assert!(out.success);
        assert!(out.data.as_str().contains("high"));
        assert!(out.data.as_str().contains("Drink water"));
        // And the old placeholder is gone.
        assert!(!out.data.as_str().contains("Voice message queued"));
        // The result must mention the priority explicitly so the LLM
        // can see what was applied.
        assert!(out.data.as_str().contains("priority=high"));
    }

    #[test]
    fn execute_send_notification_includes_text_and_priority() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call(
            "send_notification",
            "text=Time to stand,priority=low",
        )))
        .unwrap();
        assert!(out.success);
        assert!(out.data.as_str().contains("low"));
        assert!(out.data.as_str().contains("Time to stand"));
        // And the old placeholder is gone.
        assert!(!out.data.as_str().contains("Notification sent"));
        // The result must mention the priority explicitly so the LLM
        // can see what was applied.
        assert!(out.data.as_str().contains("priority=low"));
    }

    #[test]
    fn execute_voice_output_truncates_long_text_safely() {
        let r = populated_registry();
        // `ToolCall.arguments` is bounded to 128 bytes, so we use a
        // payload that's still larger than the 256-byte result
        // buffer after the prefix is added. 100 x's + the prefix
        // and the priority suffix fits inside 128 bytes.
        let mut args = String::<128>::new();
        let _ = args.push_str("text=");
        for _ in 0..100 {
            let _ = args.push('x');
        }
        let _ = args.push_str(",priority=urgent");
        let call = ToolCall {
            name: String::try_from("voice_output").unwrap(),
            arguments: args,
        };
        let out = run_async(r.execute(&call)).unwrap();
        assert!(out.success);
        // The buffer must not have overflowed — it just truncates.
        assert!(out.data.len() <= 256);
        assert!(out
            .data
            .as_str()
            .starts_with("Voice queued (priority=urgent): "));
        // The truncated text shows up in the result.
        assert!(out.data.as_str().contains("xxx"));
    }

    #[test]
    fn execute_flash_read_parses_address_and_length() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("flash_read", "address=2048,length=64"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Read 64 bytes from address 2048");
    }

    #[test]
    fn execute_ble_send_parses_data_and_characteristic() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call(
            "ble_send",
            "data=hello,characteristic=heart_rate",
        )))
        .unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Sent 5 bytes via BLE to heart_rate");
    }

    #[test]
    fn execute_flash_write_parses_address_and_data() {
        let r = populated_registry();
        let out =
            run_async(r.execute(&make_call("flash_write", "address=2048,data=hello"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Wrote 5 bytes to address 2048");
    }

    #[test]
    fn execute_ble_send_empty_data_defaults_to_zero() {
        // `data=` with no payload must report 0 bytes, not mis-parse.
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("ble_send", "data="))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Sent 0 bytes via BLE to default");
    }

    #[test]
    fn execute_read_sensor_unknown_sensor_fails() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", "sensor=zzz"))).unwrap();
        assert!(!out.success);
        assert!(out.error.is_some());
        assert_eq!(out.data.as_str(), "Unknown sensor");
    }

    /// A call that names no `pin` used to drive pin **0**: `arg_parse`'s `0`
    /// default turned a malformed call into a real side effect on the first
    /// pin. An absent argument is a missing field, not a request for zero.
    #[test]
    fn a_missing_pin_or_flash_address_is_an_error_not_a_zero() {
        let r = populated_registry();
        let err = run_async(r.execute(&make_call("write_gpio", "state=low"))).unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "pin",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
        for tool in ["flash_read", "flash_write"] {
            let err = run_async(r.execute(&make_call(tool, "data=x"))).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        field: "address",
                        reason: crate::error::ConfigError::MissingField
                    }
                ),
                "{tool} got {err:?}"
            );
        }
    }

    #[test]
    fn parse_args_value_containing_equals_keeps_rest() {
        // `data=a=b` — only the *first* `=` splits key from value, so the
        // rest (`a=b`) is preserved verbatim rather than re-split.
        let parsed = parse_args("data=a=b");
        assert_eq!(arg(&parsed, "data", ""), "a=b");
    }

    #[test]
    fn normalize_sensor_preserves_unknown_name() {
        // Unknown sensor names fall through unchanged (the executor then
        // rejects them as "Unknown sensor").
        assert_eq!(normalize_sensor("garbage"), "garbage");
        assert_eq!(normalize_sensor(""), "");
    }

    #[test]
    fn pin_to_str_pre_renders_the_common_range_and_caps_above_it() {
        // The pre-rendered table is the contract for every number a
        // tool result embeds in a fixed reply string. Spot the range
        // ends and the sparse high values, plus the `>9999` cap.
        assert_eq!(pin_to_str(0), "0");
        assert_eq!(pin_to_str(9), "9");
        assert_eq!(pin_to_str(10), "10");
        assert_eq!(pin_to_str(99), "99");
        assert_eq!(pin_to_str(100), "100");
        assert_eq!(pin_to_str(200), "200");
        assert_eq!(pin_to_str(256), "256");
        assert_eq!(pin_to_str(512), "512");
        assert_eq!(pin_to_str(1000), "1000");
        assert_eq!(pin_to_str(1024), "1024");
        assert_eq!(pin_to_str(2048), "2048");
        assert_eq!(pin_to_str(4096), "4096");
        assert_eq!(pin_to_str(8192), "8192");
        assert_eq!(pin_to_str(9999), "9999");
        // Above the table everything collapses to the bounded cap so a
        // crafted large pin can never blow the reply buffer.
        assert_eq!(pin_to_str(10_000), ">9999");
        assert_eq!(pin_to_str(u32::MAX), ">9999");
    }


    #[test]
    fn registry_query_methods() {
        let mut r = ToolRegistry::new();
        // Empty registry: every query reflects the absence.
        assert_eq!(r.count(), 0);
        assert!(!r.has_tool("read_sensor"));
        assert_eq!(r.names(), "");
        assert_eq!(r.describe(), "");
        assert!(r.all_tools().is_empty());

        let tool = Tool {
            name: try_heapless::<32>("read_sensor"),
            description: try_heapless::<128>("Read a sensor value"),
            tool_type: ToolType::ReadSensor,
        };
        r.register(tool).unwrap();
        assert_eq!(r.count(), 1);
        assert!(r.has_tool("read_sensor"));
        assert!(!r.has_tool("other"));
        assert_eq!(r.names(), "read_sensor");
        assert!(r.describe().contains("read_sensor: Read a sensor value"));
        assert_eq!(r.all_tools().len(), 1);
    }

    #[test]
    fn register_rejects_overflow() {
        let mut r = ToolRegistry::new();
        // Fill the registry to its capacity.
        for i in 0..MAX_TOOLS {
            let name = format!("tool_{i}");
            let tool = Tool {
                name: try_heapless::<32>(&name),
                description: try_heapless::<128>("d"),
                tool_type: ToolType::ReadSensor,
            };
            r.register(tool).unwrap();
        }
        assert_eq!(r.count(), MAX_TOOLS);
        // The MAX_TOOLS+1th registration must fail loudly, never silently
        // drop the tool.
        let extra = Tool {
            name: try_heapless::<32>("overflow"),
            description: try_heapless::<128>("d"),
            tool_type: ToolType::ReadSensor,
        };
        let err = r.register(extra).unwrap_err();
        assert!(matches!(err, AgentError::MemoryAllocationFailed { .. }));
    }

    #[test]
    fn describe_skips_empty_description_registry() {
        // A tool with an empty description is omitted from describe(),
        // but still counted by count()/has_tool()/names().
        let mut r = ToolRegistry::new();
        let tool = Tool {
            name: try_heapless::<32>("quiet"),
            description: try_heapless::<128>(""),
            tool_type: ToolType::ReadSensor,
        };
        r.register(tool).unwrap();
        assert_eq!(r.count(), 1);
        assert!(r.has_tool("quiet"));
        assert_eq!(r.names(), "quiet");
        assert_eq!(r.describe(), "");
    }

    #[test]
    fn execute_read_sensor_covers_all_sensor_types() {
        let r = populated_registry();
        let cases: &[(&str, &str)] = &[
            ("temperature", "25.5"),
            ("accelerometer", "X:0.1"),
            ("humidity", "65%"),
            ("pressure", "1013 hPa"),
            ("light", "500 lux"),
            ("heart_rate", "BPM"),
            ("hrv", "55ms"),
            ("glucose", "mg/dL"),
            ("ecg", "Rhythm"),
            ("stress", "Moderate"),
            ("battery", "3700mV"),
            ("memory", "free_heap"),
        ];
        // The canonical list is the contract; this table must cover it
        // exactly, so adding a sensor in one place and not the other fails
        // here rather than silently producing "Unknown sensor" at runtime.
        for name in SENSOR_NAMES {
            assert!(
                cases.iter().any(|(s, _)| *s == name),
                "SENSOR_NAMES has {name} but the value table does not"
            );
        }
        assert_eq!(
            cases.len(),
            SENSOR_NAMES.len(),
            "the value table has entries that are not in SENSOR_NAMES"
        );
        for (sensor, needle) in cases {
            let args = format!("sensor={sensor}");
            let out = run_async(r.execute(&make_call("read_sensor", &args))).unwrap();
            assert!(out.success, "sensor {sensor} should succeed");
            assert!(
                out.data.as_str().contains(needle),
                "sensor {sensor} produced: {}",
                out.data.as_str()
            );
        }
    }

    #[test]
    fn execute_read_sensor_unknown_sensor_reports_failure() {
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", "sensor=does_not_exist"))).unwrap();
        assert!(!out.success);
        assert!(out.data.as_str().contains("Unknown sensor"));
    }

    #[test]
    fn execute_write_gpio_numeric_states() {
        // `state=1`/`state=0` map to high/low, matching the docs.
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("write_gpio", "pin=3,state=1"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Pin 3 set to high");
        let out = run_async(r.execute(&make_call("write_gpio", "pin=3,state=0"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Pin 3 set to low");
    }

    #[test]
    fn execute_voice_output_priority_variants() {
        let r = populated_registry();
        // low and urgent are accepted alongside high/default.
        for (prio, needle) in [("low", "priority=low"), ("urgent", "priority=urgent")] {
            let args = format!("text=hi,priority={prio}");
            let out = run_async(r.execute(&make_call("voice_output", &args))).unwrap();
            assert!(out.success);
            assert!(
                out.data.as_str().contains(needle),
                "voice {prio}: {}",
                out.data
            );
        }
        // An unknown priority normalises to "normal".
        let out =
            run_async(r.execute(&make_call("voice_output", "text=hi,priority=weird"))).unwrap();
        assert!(out.data.as_str().contains("priority=normal"));
    }

    #[test]
    fn execute_send_notification_priority_variants() {
        let r = populated_registry();
        for (prio, needle) in [("high", "priority=high"), ("urgent", "priority=urgent")] {
            let args = format!("text=hi,priority={prio}");
            let out = run_async(r.execute(&make_call("send_notification", &args))).unwrap();
            assert!(out.success);
            assert!(
                out.data.as_str().contains(needle),
                "notify {prio}: {}",
                out.data
            );
        }
        // Unknown priority normalises to normal.
        let out = run_async(r.execute(&make_call("send_notification", "text=hi,priority=weird")))
            .unwrap();
        assert!(out.data.as_str().contains("priority=normal"));
    }

    /// A real-target handler that covers exactly one tool, like the ESP32
    /// firmware's `Esp32ToolHandler` covers GPIO/sensor/web but nothing else.
    struct ReadSensorOnly;
    static READ_SENSOR_ONLY: ReadSensorOnly = ReadSensorOnly;

    impl ToolHandler for ReadSensorOnly {
        fn handle(&self, call: &ToolCall) -> Option<ToolResult> {
            if call.name.as_str() == "read_sensor" {
                Some(ToolResult {
                    tool_name: call.name.clone(),
                    data: String::try_from("real: 21.5C").unwrap(),
                    success: true,
                    error: None,
                })
            } else {
                None
            }
        }

        fn serves(&self, name: &str) -> bool {
            name == "read_sensor"
        }
    }

    /// A real-target handler that serves `read_sensor` but has no hardware for
    /// ten of the twelve sensors the shared description names — the ESP32 case.
    struct TemperatureOnlySensor;
    static TEMPERATURE_ONLY_SENSOR: TemperatureOnlySensor = TemperatureOnlySensor;

    impl ToolHandler for TemperatureOnlySensor {
        fn handle(&self, call: &ToolCall) -> Option<ToolResult> {
            if call.name.as_str() == "read_sensor" {
                Some(ToolResult {
                    tool_name: call.name.clone(),
                    data: String::try_from("temperature=42.0 C").unwrap(),
                    success: true,
                    error: None,
                })
            } else {
                None
            }
        }

        fn serves(&self, name: &str) -> bool {
            name == "read_sensor"
        }

        fn describe_tool(&self, name: &str) -> Option<&'static str> {
            match name {
                "read_sensor" => {
                    Some("Read the chip temperature or free heap. args: sensor=temperature|memory")
                }
                _ => None,
            }
        }
    }

    /// Round 24: a served tool must not over-claim its argument space.
    ///
    /// `serves()` (round 21) keeps unserved *tools* out of the prompt, but
    /// `read_sensor` is one tool with twelve sensor names. A target that runs
    /// the tool but has no battery/accelerometer/... hardware still inherited
    /// the shared description and so advertised readings it answers
    /// "unsupported sensor" to. `describe_tool` lets the handler say what it
    /// really reads.
    #[test]
    fn a_handler_can_narrow_a_served_tools_description() {
        // Model the real registry: `register_builtin_tools` registers
        // `read_sensor` with the shared twelve-sensor description.
        let mut r = ToolRegistry::new();
        r.register(Tool {
            name: String::try_from("read_sensor").unwrap(),
            description: String::try_from(
                "Read a sensor value (temperature, accelerometer, humidity, \
                 pressure, light, heart_rate, hrv, glucose, ecg, stress, battery)",
            )
            .unwrap(),
            tool_type: ToolType::ReadSensor,
        })
        .unwrap();
        // The shared description names twelve sensors, `battery` among them.
        assert!(r
            .describe()
            .contains("read_sensor: Read a sensor value (temperature"));
        assert!(r.describe().contains("battery"));

        r.set_handler(&TEMPERATURE_ONLY_SENSOR);
        let served = r.describe_served();
        assert!(
            served.contains("read_sensor: Read the chip temperature or free heap"),
            "the handler's description must replace the registered one: {served}"
        );
        // The over-claimed sensors are gone...
        assert!(
            !served.contains("battery"),
            "a sensor the board cannot read must not be advertised: {served}"
        );
        // ...while `memory`, which the board *can* read, is now named.
        assert!(
            served.contains("memory"),
            "a sensor the board can read must be advertised: {served}"
        );
        // `describe()` is the unfiltered registry view and stays as registered.
        assert!(r.describe().contains("battery"));
    }

    /// Round 21: a registered-but-unserved tool must not reach the LLM prompt.
    /// On the ESP32 seven of the fifteen registered names had no handler arm, so
    /// the prompt advertised the health sensors, `voice_output`,
    /// `send_notification` and `ble_send` on a board that has none of them.
    #[test]
    fn the_prompt_lists_only_the_tools_the_handler_can_serve() {
        let mut r = populated_registry();
        // With no handler installed, every registered tool is advertised — the
        // simulated default must not change.
        let full = r.describe();
        assert_eq!(
            full,
            r.describe_served(),
            "no handler must leave the listing unchanged"
        );
        assert!(full.contains("ble_send"));

        // With a handler that serves one tool, only that one is advertised.
        r.set_handler(&READ_SENSOR_ONLY);
        let served = r.describe_served();
        assert!(
            served.contains("read_sensor: Read a sensor value"),
            "got {served}"
        );
        for absent in [
            "ble_send",
            "voice_output",
            "send_notification",
            "flash_write",
        ] {
            assert!(
                !served.contains(absent),
                "{absent} must not be advertised: {served}"
            );
        }
        // `describe()` itself still lists everything — only the prompt path
        // filters, so nothing else that reads the registry changes behaviour.
        assert!(r.describe().contains("ble_send"));
    }

    #[test]
    fn real_target_handler_result_wins_over_simulation() {
        let mut r = populated_registry();
        r.set_handler(&READ_SENSOR_ONLY);
        let out = run_async(r.execute(&make_call("read_sensor", "sensor=temperature"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "real: 21.5C");
    }

    #[test]
    fn real_target_refuses_to_simulate_unhandled_hardware_tools() {
        // Regression: with a handler installed, an uncovered tool fell through
        // to the simulated executor and reported fabricated success — e.g.
        // `flash_write` answered "Wrote 5 bytes to address 2048" while nothing
        // was persisted, and `ble_send` answered "Sent 5 bytes via BLE" on a
        // firmware with no BLE stack at all. The LLM prompt lists these tools,
        // so the agent would relay that invented success to the user.
        let mut r = populated_registry();
        r.set_handler(&READ_SENSOR_ONLY);

        for (tool, args) in [
            ("flash_write", "address=2048,data=hello"),
            ("flash_read", "address=2048,length=64"),
            ("ble_send", "data=hello,characteristic=default"),
            ("voice_output", "text=hi"),
            ("send_notification", "text=hi"),
        ] {
            let out = run_async(r.execute(&make_call(tool, args))).unwrap();
            assert!(!out.success, "{tool} must not report success");
            assert_eq!(out.tool_name.as_str(), tool);
            assert_eq!(
                out.error.as_deref(),
                Some("not implemented on this target"),
                "{tool} must say why"
            );
        }
    }

    #[test]
    fn real_target_unknown_tool_is_still_a_missing_field_error() {
        // Only *registered* tools get the "not implemented" result; a name the
        // registry never heard of keeps the original error.
        let mut r = populated_registry();
        r.set_handler(&READ_SENSOR_ONLY);
        let err = run_async(r.execute(&make_call("no_such_tool", ""))).unwrap_err();
        assert!(matches!(
            err,
            crate::error::AgentError::ConfigurationError { field: "tool", .. }
        ));
    }

    #[test]
    fn simulated_registry_without_handler_still_answers() {
        // The simulation fallback is the host/test behaviour and must be
        // untouched when no real-target handler is installed.
        let r = populated_registry();
        let out =
            run_async(r.execute(&make_call("flash_write", "address=2048,data=hello"))).unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Wrote 5 bytes to address 2048");
    }

    #[test]
    fn required_arg_parse_reports_an_invalid_value() {
        // A *present* but unparseable `pin` is an invalid value, distinct from
        // the missing-field case covered above.
        let r = populated_registry();
        let err = run_async(r.execute(&make_call("write_gpio", "pin=notanumber,state=low")))
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "pin",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn read_sensor_with_no_sensor_named_is_an_unknown_sensor() {
        // With no explicit `sensor=` and no bare token, the implied sensor is
        // empty → the executor answers "Unknown sensor" rather than guessing.
        let r = populated_registry();
        let out = run_async(r.execute(&make_call("read_sensor", ""))).unwrap();
        assert!(!out.success);
        assert_eq!(out.data.as_str(), "Unknown sensor");
        assert_eq!(out.error.as_deref(), Some("Invalid sensor type"));
    }

    #[test]
    fn a_web_tool_without_a_handler_is_a_configuration_error() {
        // A real-target registry registers `web_fetch` but only a handler can
        // execute it. Without one, the executor refuses rather than inventing a
        // response body.
        let mut r = ToolRegistry::new();
        r.register(Tool {
            name: try_heapless::<32>("web_fetch"),
            description: try_heapless::<128>("Fetch a web page"),
            tool_type: ToolType::WebFetch,
        })
        .unwrap();
        let err = run_async(r.execute(&make_call("web_fetch", "url=https://example.com"))).unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "tool",
                    reason: crate::error::ConfigError::NotConfigured
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn default_tool_registry_matches_new() {
        let a = ToolRegistry::default();
        let b = ToolRegistry::new();
        assert_eq!(a.count(), b.count());
        assert_eq!(a.names(), b.names());
        assert!(!a.has_tool("read_sensor"));
    }

    #[test]
    fn describe_omits_entries_that_do_not_fit() {
        // Fill the registry with long descriptions so the 2048-byte listing
        // buffer overflows and the tail entries are dropped whole (with a
        // warning) rather than truncated mid-entry.
        let mut r = ToolRegistry::new();
        let long = "x".repeat(120);
        for i in 0..MAX_TOOLS {
            r.register(Tool {
                name: try_heapless::<32>(&alloc::format!("tool_{i:02}")),
                description: try_heapless::<128>(&long),
                tool_type: ToolType::ReadSensor,
            })
            .unwrap();
        }
        let desc = r.describe();
        assert!(desc.len() <= 2048);
        // The first entry always fits; a later one is dropped.
        assert!(desc.contains("tool_00"));
        assert!(!desc.contains("tool_15"), "the overflow must be omitted, not truncated");
        // Every advertised line is a complete `- name: description` entry.
        for line in desc.as_str().lines() {
            assert!(line.starts_with("- "), "half an entry leaked: {line}");
            assert!(line.contains(": "), "half an entry leaked: {line}");
        }
    }
}
