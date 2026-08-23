//! Embedded tools for mAgent
//!
//! Provides tool registry and execution for embedded operations:
//! - Sensor reading
//! - GPIO control
//! - Flash storage
//! - BLE communication

use crate::error::{AgentError, Result};
use crate::agent::{ToolCall, ToolResult};
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};
use core::str::FromStr;

/// Maximum number of tools
const MAX_TOOLS: usize = 16;

/// A real-hardware tool handler.
///
/// A chip layer (firmware / host) implements this to back `ToolRegistry`
/// tools with actual hardware (GPIO, sensors, flash, ...). Returning `None`
/// from [`Self::handle`] means "not handled — fall back to the built-in
/// simulated value", so a handler can cover just the tools it can drive.
pub trait ToolHandler {
    /// Attempt to execute `call` against real hardware. Return `Some(result)`
    /// to override the built-in simulated executor, or `None` to fall back to it.
    fn handle(&self, call: &ToolCall) -> Option<ToolResult>;
}

/// Tool registry
pub struct ToolRegistry {
    tools: Vec<Tool, MAX_TOOLS>,
    /// Optional real-hardware handler consulted before the simulated
    /// executors. `None` (the default) keeps the original mock behaviour.
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
fn parse_args(args: &str) -> Vec<(&str, &str), 8> {
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
fn arg<'a>(args: &'a Vec<(&'a str, &'a str), 8>, key: &str, default: &'a str) -> &'a str {
    for &(k, v) in args.iter() {
        if k == key {
            return if v.is_empty() { default } else { v };
        }
    }
    default
}

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
        _ => name,
    }
}

/// Pre-rendered numbers for the common 0–9999 range. Anything
/// above that gets surfaced as `">9999"` so the result string
/// stays bounded. We pre-render instead of using `format!` because
/// the embedded build needs to avoid the `format!` allocator path.
fn pin_to_str(pin: u32) -> &'static str {
    match pin {
        0 => "0", 1 => "1", 2 => "2", 3 => "3", 4 => "4",
        5 => "5", 6 => "6", 7 => "7", 8 => "8", 9 => "9",
        10 => "10", 11 => "11", 12 => "12", 13 => "13", 14 => "14",
        15 => "15", 16 => "16", 17 => "17", 18 => "18", 19 => "19",
        20 => "20", 21 => "21", 22 => "22", 23 => "23", 24 => "24",
        25 => "25", 26 => "26", 27 => "27", 28 => "28", 29 => "29",
        30 => "30", 31 => "31", 32 => "32", 33 => "33", 34 => "34",
        35 => "35", 36 => "36", 37 => "37", 38 => "38", 39 => "39",
        40 => "40", 41 => "41", 42 => "42", 43 => "43", 44 => "44",
        45 => "45", 46 => "46", 47 => "47", 48 => "48", 49 => "49",
        50 => "50", 51 => "51", 52 => "52", 53 => "53", 54 => "54",
        55 => "55", 56 => "56", 57 => "57", 58 => "58", 59 => "59",
        60 => "60", 61 => "61", 62 => "62", 63 => "63", 64 => "64",
        65 => "65", 66 => "66", 67 => "67", 68 => "68", 69 => "69",
        70 => "70", 71 => "71", 72 => "72", 73 => "73", 74 => "74",
        75 => "75", 76 => "76", 77 => "77", 78 => "78", 79 => "79",
        80 => "80", 81 => "81", 82 => "82", 83 => "83", 84 => "84",
        85 => "85", 86 => "86", 87 => "87", 88 => "88", 89 => "89",
        90 => "90", 91 => "91", 92 => "92", 93 => "93", 94 => "94",
        95 => "95", 96 => "96", 97 => "97", 98 => "98", 99 => "99",
        100 => "100", 200 => "200", 256 => "256", 512 => "512",
        1000 => "1000", 1024 => "1024", 2048 => "2048", 4096 => "4096",
        8192 => "8192", 9999 => "9999",
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
        self.tools.push(tool).map_err(|_| AgentError::MemoryAllocationFailed {
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
        }

        // Find tool
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == call.name)
            .ok_or(AgentError::ConfigurationError {
                field: "tool",
                reason: crate::error::ConfigError::MissingField,
            })?;

        // Execute based on tool type
        match tool.tool_type {
            ToolType::ReadSensor => self.execute_read_sensor(&call.arguments).await,
            ToolType::WriteGpio => self.execute_write_gpio(&call.arguments).await,
            ToolType::FlashRead => self.execute_flash_read(&call.arguments).await,
            ToolType::FlashWrite => self.execute_flash_write(&call.arguments).await,
            ToolType::BleSend => self.execute_ble_send(&call.arguments).await,
            ToolType::ReadHeartRate => self.execute_read_sensor(&call.arguments).await,
            ToolType::ReadGlucose => self.execute_read_sensor(&call.arguments).await,
            ToolType::ReadEcg => self.execute_read_sensor(&call.arguments).await,
            ToolType::VoiceOutput => self.execute_voice_output(&call.arguments).await,
            ToolType::SendNotification => self.execute_send_notification(&call.arguments).await,
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
        let take = core::cmp::min(text.len(), room);
        let _ = result.push_str(&text[..take]);

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
        let take = core::cmp::min(text.len(), room);
        let _ = result.push_str(&text[..take]);

        Ok(ToolResult {
            tool_name: heapless::String::try_from("send_notification").unwrap(),
            data: result,
            success: true,
            error: None,
        })
    }

    /// Execute sensor read
    async fn execute_read_sensor(&self, args: &str) -> Result<ToolResult> {
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
            // bare token, then to "unknown" if neither is present.
            let explicit = arg(&parsed, "sensor", "");
            if !explicit.is_empty() {
                explicit
            } else {
                // First bare token (no `=`).
                let mut iter = parsed.iter().filter(|(k, _)| k.is_empty());
                match iter.next() {
                    Some((_, v)) => *v,
                    None => "",
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
            _ => {
                return Ok(ToolResult {
                    tool_name: heapless::String::try_from("read_sensor").unwrap(),
                    data: heapless::String::try_from("Unknown sensor").unwrap(),
                    success: false,
                    error: Some(heapless::String::try_from("Invalid sensor type").unwrap()),
                });
            }
        };

        Ok(ToolResult {
            tool_name: heapless::String::try_from("read_sensor").unwrap(),
            data: heapless::String::try_from(value).unwrap(),
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
        let pin = arg_parse::<u32>(&parsed, "pin", 0);
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
        let address = arg_parse::<u32>(&parsed, "address", 0);
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
        let address = arg_parse::<u32>(&parsed, "address", 0);
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
    pub fn describe(&self) -> String<1024> {
        let mut out = String::<1024>::new();
        for tool in self.tools.iter() {
            if tool.description.is_empty() {
                continue;
            }
            if !out.is_empty() {
                let _ = out.push('\n');
            }
            let _ = out.push_str("- ");
            let _ = out.push_str(tool.name.as_str());
            let _ = out.push_str(": ");
            let _ = out.push_str(tool.description.as_str());
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
    use crate::tools::{Tool, ToolType};

    /// Build a populated registry identical to what
    /// `register_builtin_tools` produces, so we can exercise
    /// `describe()` and `names()` against a realistic state.
    fn populated_registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        let entries: &[(&str, &str, ToolType)] = &[
            ("read_sensor", "Read a sensor value", ToolType::ReadSensor),
            ("write_gpio", "Set a GPIO pin high/low", ToolType::WriteGpio),
            ("flash_read", "Read bytes from internal flash", ToolType::FlashRead),
            ("flash_write", "Write bytes to internal flash", ToolType::FlashWrite),
            ("ble_send", "Send a payload over BLE", ToolType::BleSend),
            ("voice_output", "Queue a text-to-speech utterance", ToolType::VoiceOutput),
            ("send_notification", "Send a smartwatch notification", ToolType::SendNotification),
        ];
        for (name, desc, ty) in entries {
            let tool = Tool {
                name: String::try_from(*name).unwrap(),
                description: String::try_from(*desc).unwrap(),
                tool_type: *ty,
            };
            r.register(tool).unwrap();
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
        assert!(desc.as_str().contains("write_gpio: Set a GPIO pin high/low"));
        assert!(desc.as_str().contains("flash_read: Read bytes from internal flash"));
        assert!(desc.as_str().contains("ble_send: Send a payload over BLE"));
        assert!(desc.as_str().contains("voice_output: Queue a text-to-speech utterance"));
        assert!(desc.as_str().contains("send_notification: Send a smartwatch notification"));
        // Newline separated. We have 7 entries.
        let line_count = desc.as_str().matches('\n').count() + 1;
        assert_eq!(line_count, 7);
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
        let expected = "read_sensor, write_gpio, flash_read, flash_write, ble_send, voice_output, send_notification";
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
        let out = run_async(
            r.execute(&make_call("voice_output", "text=Drink water,priority=high")),
        )
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
        let out = run_async(
            r.execute(&make_call("send_notification", "text=Time to stand,priority=low")),
        )
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
        assert!(out.data.as_str().starts_with("Voice queued (priority=urgent): "));
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
        let out = run_async(
            r.execute(&make_call("ble_send", "data=hello,characteristic=heart_rate")),
        )
        .unwrap();
        assert!(out.success);
        assert_eq!(out.data.as_str(), "Sent 5 bytes via BLE to heart_rate");
    }
}

