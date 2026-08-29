//! Parsing and dispatching of `agent.reason()` action strings.
//!
//! The embedded agent returns free-form decisions; for a device to act on
//! them deterministically we define a tiny command grammar:
//!
//! ```text
//! COMMAND           e.g. "SET_COOLING"
//! COMMAND:value     e.g. "SET_COOLING_PULSE:80"
//! ```
//!
//! [`Action::parse`] splits `name` / optional `value`; [`apply_action`] maps
//! known commands onto a [`HardwareBackend`]. Unknown commands are an explicit
//! error when dispatched directly (never silently dropped).
//!
//! # Action catalogue
//!
//! The known-command set is intentionally small — every entry has to map onto
//! a single [`HardwareBackend`] method so an LLM that emits a stray command
//! gets an explicit error rather than a silent no-op.
//!
//! | Command               | Arg              | Hardware effect          |
//! |-----------------------|------------------|--------------------------|
//! | `FAN_ON`              | none             | `gpio_write(1, 1)`       |
//! | `FAN_OFF`             | none             | `gpio_write(1, 0)`       |
//! | `SET_COOLING`         | none             | `gpio_write(1, 1)`       |
//! | `SET_COOLING_PULSE`   | duty `0..=100`   | `pwm_set(2, duty)`       |
//! | `GPIO_WRITE`          | `pin,level`      | `gpio_write(pin, level)` |
//! | `LED_SET`             | `pin,level`      | `gpio_write(pin, level)` |
//! | `BUZZER`              | duty `0..=100`   | `pwm_set(3, duty)`       |
//! | `BLE_SEND`            | payload bytes    | `ble_send(payload)`      |
//! | `POWER_SET`           | `0..=3` profile  | `power_set(profile)`     |
//!
//! All numeric arguments are parsed defensively (bad inputs are rejected
//! with an explicit error and never silently coerced).

use crate::hardware::HardwareBackend;

/// Maximum payload bytes accepted for `BLE_SEND` (defensive cap mirroring
/// the Lua binding's [`crate::vm::MAX_PAYLOAD_LEN`]).
pub const MAX_BLE_PAYLOAD: usize = 4096;

/// A parsed agent action: an uppercase command name plus an optional argument,
/// e.g. `SET_COOLING_PULSE:80` → `name = "SET_COOLING_PULSE"`,
/// `value = Some("80")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action<'a> {
    /// The command name (already trimmed, original case preserved).
    pub name: &'a str,
    /// Optional argument after the `:` separator.
    pub value: Option<&'a str>,
}

impl<'a> Action<'a> {
    /// Parse a command string. Returns `None` for empty / malformed input.
    ///
    /// The first `:` is the name / value separator; any subsequent `:` stays
    /// in the value (so `a:b:c` parses as `name="a", value="b:c"`).
    pub fn parse(s: &'a str) -> Option<Action<'a>> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (name, value) = match s.split_once(':') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (s, None),
        };
        if name.is_empty() {
            None
        } else {
            Some(Action { name, value })
        }
    }

    /// Whether the command name equals `expected` (case-insensitive).
    pub fn is(&self, expected: &str) -> bool {
        self.name.eq_ignore_ascii_case(expected)
    }

    /// Whether this is a command the built-in dispatcher recognises.
    ///
    /// [`crate::runtime::AppRuntime::tick`] uses this to decide whether to
    /// dispatch a returned string as an action vs. treat it as informational
    /// text.
    pub fn is_known(&self) -> bool {
        KNOWN.iter().any(|k| self.name.eq_ignore_ascii_case(k))
    }
}

/// The known-command catalogue. Kept in module scope so tests can iterate it
/// without copying the list.
const KNOWN: &[&str] = &[
    "FAN_ON",
    "FAN_OFF",
    "SET_COOLING",
    "SET_COOLING_PULSE",
    "GPIO_WRITE",
    "LED_SET",
    "BUZZER",
    "BLE_SEND",
    "POWER_SET",
];

/// Parse a `pin,level` (or `pin:level`) pair from an action value.
///
/// Returns `None` for malformed input. `level` is 0 or 1; non-binary values
/// are rejected so the script cannot, e.g., enable pull-ups by accident.
fn parse_pin_level(value: Option<&str>) -> Option<(u8, u8)> {
    let v = value?;
    let (pin_s, level_s) = v.split_once([',', ':'])?;
    let pin: u8 = pin_s.trim().parse().ok()?;
    let level: u8 = level_s.trim().parse().ok()?;
    if level > 1 {
        return None;
    }
    Some((pin, level))
}

/// Parse a single `0..=255` integer (duty / brightness / profile).
fn parse_u8_in_range(value: Option<&str>, lo: u8, hi: u8) -> Option<u8> {
    let v = value?;
    let n: u8 = v.trim().parse().ok()?;
    if n < lo || n > hi {
        return None;
    }
    Some(n)
}

/// Apply a known action to `hw`.
///
/// Unknown commands return `Err` — a silent no-op would let a mis-issuing
/// agent appear to succeed while doing nothing.
///
/// Every numeric / payload argument is bounds-checked before being sent to
/// the backend; an out-of-range value is an explicit error, never a clamp.
pub fn apply_action(
    hw: &mut dyn HardwareBackend,
    action: &Action<'_>,
) -> std::result::Result<(), String> {
    let upper = action.name.to_ascii_uppercase();
    match upper.as_str() {
        "FAN_ON" | "SET_COOLING" => hw.gpio_write(1, 1),
        "FAN_OFF" => hw.gpio_write(1, 0),
        "SET_COOLING_PULSE" => {
            let duty = parse_u8_in_range(action.value, 0, 100)
                .ok_or_else(|| format!("SET_COOLING_PULSE: bad duty {:?}", action.value))?;
            hw.pwm_set(2, duty)
        }
        "GPIO_WRITE" | "LED_SET" => {
            let (pin, level) = parse_pin_level(action.value).ok_or_else(|| {
                format!(
                    "{upper}: expected `pin,level` (level 0|1), got {:?}",
                    action.value
                )
            })?;
            hw.gpio_write(pin, level)
        }
        "BUZZER" => {
            let duty = parse_u8_in_range(action.value, 0, 100)
                .ok_or_else(|| format!("BUZZER: bad duty {:?}", action.value))?;
            hw.pwm_set(3, duty)
        }
        "BLE_SEND" => {
            let payload = action.value.unwrap_or("");
            let bytes = payload.as_bytes();
            if bytes.len() > MAX_BLE_PAYLOAD {
                return Err(format!(
                    "BLE_SEND: payload too long ({} > {MAX_BLE_PAYLOAD})",
                    bytes.len()
                ));
            }
            hw.ble_send(bytes)
        }
        "POWER_SET" => {
            let profile = parse_u8_in_range(action.value, 0, 3)
                .ok_or_else(|| format!("POWER_SET: bad profile {:?}", action.value))?;
            hw.power_set(profile)
        }
        other => Err(format!("unknown action: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every constant in [`KNOWN`] must be parsed back by `is_known` (i.e.
    /// case-insensitive lookup is wired both ways).
    #[test]
    fn known_set_is_self_consistent() {
        for k in KNOWN {
            let a = Action::parse(k).unwrap();
            assert!(a.is_known(), "expected `{k}` to be recognised");
            let lower_str = k.to_ascii_lowercase();
            let lower = Action::parse(&lower_str).unwrap();
            assert!(
                lower.is_known(),
                "expected lowercase `{k}` to be recognised"
            );
        }
    }

    #[test]
    fn unknown_action_is_rejected() {
        let a = Action::parse("DO_THE_IMPOSSIBLE").unwrap();
        assert!(!a.is_known());
    }

    #[test]
    fn parse_pin_level_accepts_comma_and_colon() {
        assert_eq!(parse_pin_level(Some("3,1")), Some((3, 1)));
        assert_eq!(parse_pin_level(Some("7:0")), Some((7, 0)));
        assert_eq!(parse_pin_level(Some("  9 , 1  ")), Some((9, 1)));
    }

    #[test]
    fn parse_pin_level_rejects_bad_input() {
        assert!(parse_pin_level(None).is_none());
        assert!(parse_pin_level(Some("3")).is_none());
        assert!(
            parse_pin_level(Some("3,2")).is_none(),
            "level must be 0 or 1"
        );
        assert!(parse_pin_level(Some("256,0")).is_none(), "pin must fit u8");
        assert!(parse_pin_level(Some("x,1")).is_none());
    }

    #[test]
    fn parse_u8_in_range_enforces_bounds() {
        assert_eq!(parse_u8_in_range(Some("0"), 0, 100), Some(0));
        assert_eq!(parse_u8_in_range(Some("100"), 0, 100), Some(100));
        assert_eq!(parse_u8_in_range(Some("101"), 0, 100), None);
        assert_eq!(parse_u8_in_range(Some("-1"), 0, 100), None);
        assert!(parse_u8_in_range(None, 0, 100).is_none());
    }

    /// `SET_COOLING_PULSE` with no value used to silently default to 50 —
    /// the dispatcher's contract now is to reject. Verifies the new contract.
    #[test]
    fn set_cooling_pulse_rejects_missing_duty() {
        let mut hw = SimHardwareShim::new();
        let a = Action::parse("SET_COOLING_PULSE").unwrap();
        assert!(apply_action(&mut hw, &a).is_err());
    }

    /// `SET_COOLING_PULSE:120` must be an explicit error (no silent clamp).
    #[test]
    fn set_cooling_pulse_rejects_out_of_range_duty() {
        let mut hw = SimHardwareShim::new();
        let a = Action::parse("SET_COOLING_PULSE:120").unwrap();
        assert!(apply_action(&mut hw, &a).is_err());
    }

    #[test]
    fn ble_send_rejects_overlong_payload() {
        let mut hw = SimHardwareShim::new();
        let big = "x".repeat(MAX_BLE_PAYLOAD + 1);
        let cmd = format!("BLE_SEND:{}", big);
        let a = Action::parse(&cmd).unwrap();
        assert!(apply_action(&mut hw, &a).is_err());
    }

    #[test]
    fn ble_send_accepts_max_payload() {
        let mut hw = SimHardwareShim::new();
        let big = "x".repeat(MAX_BLE_PAYLOAD);
        let cmd = format!("BLE_SEND:{}", big);
        let a = Action::parse(&cmd).unwrap();
        apply_action(&mut hw, &a).unwrap();
        assert_eq!(hw.last_ble.len(), MAX_BLE_PAYLOAD);
    }

    #[test]
    fn gpio_write_action_dispatches_to_pin() {
        let mut hw = SimHardwareShim::new();
        let a = Action::parse("GPIO_WRITE:5,1").unwrap();
        apply_action(&mut hw, &a).unwrap();
        assert_eq!(hw.pins.get(&5).copied(), Some(1));
    }

    #[test]
    fn power_set_action_rejects_bad_profile() {
        let mut hw = SimHardwareShim::new();
        let a = Action::parse("POWER_SET:9").unwrap();
        assert!(apply_action(&mut hw, &a).is_err());
    }

    /// Tiny in-test hardware stub: only the methods exercised by the
    /// dispatcher above need to exist, so we don't drag the full
    /// `SimHardware` (which lives behind `magent-hal`'s nrf52840 adapters)
    /// into this module's unit tests.
    struct SimHardwareShim {
        pins: std::collections::BTreeMap<u8, u8>,
        pwm: std::collections::BTreeMap<u8, u8>,
        last_ble: Vec<u8>,
        last_power: Option<u8>,
    }

    impl SimHardwareShim {
        fn new() -> Self {
            Self {
                pins: std::collections::BTreeMap::new(),
                pwm: std::collections::BTreeMap::new(),
                last_ble: Vec::new(),
                last_power: None,
            }
        }
    }

    impl HardwareBackend for SimHardwareShim {
        fn gpio_write(&mut self, pin: u8, level: u8) -> std::result::Result<(), String> {
            self.pins.insert(pin, level);
            Ok(())
        }
        fn gpio_read(&mut self, _pin: u8) -> std::result::Result<u8, String> {
            Ok(0)
        }
        fn sensor_read(&mut self, _name: &str) -> std::result::Result<f64, String> {
            Ok(0.0)
        }
        fn flash_read(
            &mut self,
            _address: u32,
            _len: usize,
        ) -> std::result::Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
        fn flash_write(&mut self, _address: u32, _data: &[u8]) -> std::result::Result<(), String> {
            Ok(())
        }
        fn flash_erase_sector(&mut self, _address: u32) -> std::result::Result<(), String> {
            Ok(())
        }
        fn i2c_read(
            &mut self,
            _addr: u8,
            _reg: u8,
            _len: usize,
        ) -> std::result::Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
        fn i2c_write(
            &mut self,
            _addr: u8,
            _reg: u8,
            _data: &[u8],
        ) -> std::result::Result<(), String> {
            Ok(())
        }
        fn adc_read(&mut self, _pin: u8) -> std::result::Result<f64, String> {
            Ok(0.0)
        }
        fn pwm_set(&mut self, pin: u8, duty: u8) -> std::result::Result<(), String> {
            self.pwm.insert(pin, duty);
            Ok(())
        }
        fn ble_send(&mut self, data: &[u8]) -> std::result::Result<(), String> {
            self.last_ble = data.to_vec();
            Ok(())
        }
        fn power_set(&mut self, profile: u8) -> std::result::Result<(), String> {
            self.last_power = Some(profile);
            Ok(())
        }
    }
}
