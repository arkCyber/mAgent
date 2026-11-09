//! mAgent Simulator for Testing
//!
//! Provides a realistic simulation of the mAgent embedded AI agent
//! for testing without actual hardware. Uses real HTTP client to
//! connect to Ollama for actual LLM reasoning.

#![cfg(feature = "std")]

// `lib.rs` is `#![no_std]`, so the standard prelude isn't in scope for this
// file. When the `std` feature is enabled `lib.rs` re-imports `std` with
// `#[macro_use]`, which brings `format!`, `vec!`, `println!` and friends
// into scope throughout the crate. We also import the standard collection
// types directly so the rest of the file can write `String` / `Vec` without
// qualifying them.
use crate::early_warning::{EarlyWarningSystem, EmergencyContact, Hospital};
use crate::error::{AgentError, Result};
use crate::health_sensors::HeartRateData;
use crate::sleep_manager::{SleepManager, SleepQuality, SleepRecord};
use crate::sports_coach::{ExerciseGoal, ExerciseType, SportsCoach};
use crate::voice_notification::{VoiceCategory, VoiceNotificationManager};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

/// Simulated sensor types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimSensorType {
    /// Ambient temperature (°C).
    Temperature,
    /// 3-axis accelerometer (m/s²).
    Accelerometer,
    /// Relative humidity (%RH).
    Humidity,
    /// Atmospheric pressure (hPa).
    Pressure,
    /// Ambient light level (lux).
    Light,
    /// Instantaneous heart rate (BPM).
    HeartRate,
    /// Heart-rate variability (RMSSD, ms).
    Hrv,
    /// Blood glucose (mg/dL).
    Glucose,
    /// Single-lead ECG waveform sample.
    Ecg,
    /// Computed stress level index.
    Stress,
    /// Device battery (percentage + millivolts).
    Battery,
    /// Free heap / memory status.
    Memory,
}

/// Simulated GPIO states
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GpioPinState {
    /// Logic-low (0 V).
    Low,
    /// Logic-high (Vcc).
    High,
}

/// Simulated flash storage
pub struct SimFlashStorage {
    data: [u8; 65536],
    writes: usize,
}

impl SimFlashStorage {
    /// Create new simulated flash
    pub fn new() -> Self {
        Self {
            data: [0xFF; 65536],
            writes: 0,
        }
    }

    /// Read from flash
    pub fn read(&self, address: u32, length: usize) -> Result<Vec<u8>> {
        let start = address as usize;
        if start + length > 65536 {
            return Err(AgentError::StorageReadFailed {
                address,
                reason: crate::error::StorageError::BadAddress,
            });
        }
        Ok(self.data[start..start + length].to_vec())
    }

    /// Write to flash
    pub fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        let start = address as usize;
        if start + data.len() > 65536 {
            return Err(AgentError::StorageWriteFailed {
                address,
                reason: crate::error::StorageError::BadAddress,
            });
        }
        for (i, &byte) in data.iter().enumerate() {
            self.data[start + i] = byte;
        }
        self.writes += 1;
        Ok(())
    }

    /// Get write count
    pub fn write_count(&self) -> usize {
        self.writes
    }

    /// Erase a sector (4KB)
    pub fn erase_sector(&mut self, sector: u32) -> Result<()> {
        // `65536 - start` underflowed for any out-of-range sector, and the
        // subsequent `data[start + i]` indexed out of bounds — a panic in debug
        // and in release alike, even though the `Result` return advertises a
        // clean failure. A sector either lies wholly inside the region or is
        // rejected.
        let start = (sector as usize).saturating_mul(4096);
        match start
            .checked_add(4096)
            .filter(|&end| end <= self.data.len())
        {
            Some(end) => {
                for byte in &mut self.data[start..end] {
                    *byte = 0xFF;
                }
                Ok(())
            }
            None => Err(AgentError::StorageWriteFailed {
                address: u32::try_from(start).unwrap_or(u32::MAX),
                reason: crate::error::StorageError::BadAddress,
            }),
        }
    }
}

impl Default for SimFlashStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated sensor readings with realistic variations
pub struct SimSensorManager {
    temperature_base: f32,
    humidity_base: f32,
    pressure_base: f32,
    accel_x: f32,
    accel_y: f32,
    accel_z: f32,
    iteration: usize,
}

impl SimSensorManager {
    /// Create new sensor manager
    pub fn new() -> Self {
        Self {
            temperature_base: 23.5,
            humidity_base: 55.0,
            pressure_base: 1013.25,
            accel_x: 0.0,
            accel_y: 0.0,
            accel_z: 9.8,
            iteration: 0,
        }
    }

    /// Read temperature in Celsius
    pub fn read_temperature(&mut self) -> f32 {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.1).sin() * 2.0)
            + ((self.iteration as f32 * 0.3).cos() * 0.5);
        self.temperature_base + variation
    }

    /// Read humidity in percentage
    pub fn read_humidity(&mut self) -> f32 {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.05).sin() * 5.0) + 2.0;
        self.humidity_base + variation
    }

    /// Read pressure in hPa
    pub fn read_pressure(&mut self) -> f32 {
        self.iteration += 1;
        let variation = (self.iteration as f32 * 0.02).sin() * 2.0;
        self.pressure_base + variation
    }

    /// Read accelerometer in g
    pub fn read_accelerometer(&mut self) -> (f32, f32, f32) {
        self.iteration += 1;
        let noise = || (self.iteration as f32 * 17.3).sin() * 0.01;
        (
            self.accel_x + noise(),
            self.accel_y + noise(),
            self.accel_z + noise(),
        )
    }

    /// Read light level in lux
    pub fn read_light(&mut self) -> f32 {
        self.iteration += 1;
        let cycle = (self.iteration as f32 * 0.01).sin() * 500.0 + 500.0;
        cycle.max(10.0)
    }

    /// Read battery voltage in millivolts (simulated, slowly draining).
    ///
    /// `battery` is part of the shared sensor vocabulary — the device
    /// registry (`tools.rs`) resolves it and the embedded heuristic
    /// (`MiniAgent::pick_tool`) emits it — so the host simulator has to
    /// resolve it too, or a prompt tuned with `--mock` behaves differently
    /// from the device it is tuned for.
    pub fn read_battery_mv(&mut self) -> u32 {
        self.iteration += 1;
        // Start at 85% (4050 mV) — the same reading the device registry's
        // value table reports — and drain slowly towards empty (3200 mV).
        let drained = ((self.iteration / 50) as u32).min(850);
        4050 - drained
    }

    /// Read free heap in bytes (simulated, wandering around a baseline).
    pub fn read_free_heap(&mut self) -> usize {
        self.iteration += 1;
        let wobble = ((self.iteration as f32 * 0.07).sin() * 4096.0) as i64;
        (131_072i64 + wobble).max(0) as usize
    }
}

impl Default for SimSensorManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated GPIO controller
pub struct SimGpioController {
    pins: [GpioPinState; 32],
}

impl SimGpioController {
    /// Create new GPIO controller
    pub fn new() -> Self {
        Self {
            pins: [GpioPinState::Low; 32],
        }
    }

    /// Set pin state
    pub fn set_pin(&mut self, pin: u8, state: GpioPinState) -> Result<()> {
        if pin >= 32 {
            return Err(AgentError::GpioOperationFailed {
                pin,
                operation: crate::error::GpioOperation::Write,
            });
        }
        self.pins[pin as usize] = state;
        Ok(())
    }

    /// Get pin state
    pub fn get_pin(&self, pin: u8) -> Result<GpioPinState> {
        if pin >= 32 {
            return Err(AgentError::GpioOperationFailed {
                pin,
                operation: crate::error::GpioOperation::Read,
            });
        }
        Ok(self.pins[pin as usize])
    }

    /// Toggle pin state
    pub fn toggle_pin(&mut self, pin: u8) -> Result<()> {
        let current = self.get_pin(pin)?;
        let new_state = match current {
            GpioPinState::Low => GpioPinState::High,
            GpioPinState::High => GpioPinState::Low,
        };
        self.set_pin(pin, new_state)
    }
}

impl Default for SimGpioController {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated BLE interface
pub struct SimBleInterface {
    connected: bool,
    messages_sent: usize,
    last_message: Option<String>,
}

impl SimBleInterface {
    /// Create new BLE interface
    pub fn new() -> Self {
        Self {
            connected: false,
            messages_sent: 0,
            last_message: None,
        }
    }

    /// Connect to BLE gateway
    pub fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    /// Disconnect
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Send data via BLE
    pub fn send(&mut self, data: &str) -> Result<()> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: crate::error::NetworkError::ConnectionRefused,
            });
        }
        self.messages_sent += 1;
        self.last_message = Some(data.to_string());
        Ok(())
    }

    /// Get message count
    pub fn message_count(&self) -> usize {
        self.messages_sent
    }

    /// Get last message
    pub fn last_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }
}

impl Default for SimBleInterface {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete agent simulator with health support
pub struct AgentSimulator {
    /// Simulated flash storage backing the agent's persistence layer.
    pub flash: SimFlashStorage,
    /// Simulated sensor manager producing deterministic readings.
    pub sensors: SimSensorManager,
    /// Simulated GPIO controller.
    pub gpio: SimGpioController,
    /// Simulated BLE interface for over-the-air traffic.
    pub ble: SimBleInterface,
    /// Health sensor state
    pub health_state: HealthSimState,
    /// Exercise coaching state machine.
    ///
    /// `sports_coach` shipped with a complete session state machine
    /// (warm-up → active → cool-down), an adaptive goal, and a coaching
    /// message queue — and no caller anywhere in the tree, while the CLI's
    /// default system prompt advertised `start_exercise`, `end_exercise`,
    /// `get_exercise_goal` and `adjust_exercise_goal`. This is that module's
    /// first consumer.
    pub sports_coach: SportsCoach,
    /// Whether a goal has been configured, so `get_exercise_goal` can tell
    /// "the user set this" from "nobody has set anything yet" instead of
    /// reporting a default as though it were a decision.
    exercise_goal_set: bool,
    /// When the current session started, for a *measured* elapsed time. `None`
    /// when no session is running.
    session_started: Option<std::time::Instant>,
    /// Notification and voice-cue pipeline.
    ///
    /// `voice_notification` shipped a queue with priority ordering, a
    /// do-not-disturb window, and a multi-channel health alert — and no caller,
    /// while the CLI's system prompt advertised `queue_voice`,
    /// `send_health_alert`, `send_meditation` and `set_dnd`. This is that
    /// module's first consumer.
    pub voice: VoiceNotificationManager,
    /// Monotonic reference for the timestamps handed to `voice`. The device may
    /// have no wall clock, so "now" is uptime, not a date.
    boot: std::time::Instant,
    /// Sleep, circadian and stress-intervention engine.
    ///
    /// `sleep_manager` shipped a sleep-quality scorer, a 30-night history, a
    /// sleep-debt calculation and a circadian model — and no caller, while the
    /// CLI's system prompt advertised `get_sleep_quality`. This is that
    /// module's first consumer. It starts empty: a freshly-booted device has
    /// not been worn overnight, and reporting a night it never recorded would
    /// be a fabricated health claim.
    pub sleep: SleepManager,
    /// Emergency-contact and nearby-hospital registry, plus the alert history.
    ///
    /// `early_warning` shipped an 1809-line chronic-disease early-warning
    /// engine — a threshold evaluator, a cooldown-limited emergency protocol
    /// that decides *which* contact and *what* message to send, and a
    /// proximity-ranked hospital list — and no caller anywhere in the tree,
    /// while the CLI's system prompt advertised `notify_emergency_contact` and
    /// `find_nearest_hospital`. Those two names had no implementation in any
    /// executor at all. This is that module's first consumer.
    pub early_warning: EarlyWarningSystem,
}

/// Health simulation state
pub struct HealthSimState {
    /// Current exercise state
    pub is_exercising: bool,
    /// Minutes elapsed in simulation
    pub minutes_elapsed: u32,
    /// Current stress level simulation
    pub stress_level: u8,
    /// Last heart rate reading
    pub last_hr: u16,
    /// Last HRV reading
    pub last_hrv: f32,
    /// Last glucose reading
    pub last_glucose: f32,
    /// Hours since meal (for glucose simulation)
    pub hours_since_meal: f32,
}

impl HealthSimState {
    /// Default-construct a state with no in-progress exercise, 30 %
    /// baseline stress, a 72 BPM resting heart rate, HRV of 55 ms,
    /// fasting-glucose 100 mg/dL, and "2 h since last meal".
    pub fn new() -> Self {
        Self {
            is_exercising: false,
            minutes_elapsed: 0,
            stress_level: 30,
            last_hr: 72,
            last_hrv: 55.0,
            last_glucose: 100.0,
            hours_since_meal: 2.0,
        }
    }
}

impl Default for HealthSimState {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentSimulator {
    /// Create new simulator
    pub fn new() -> Self {
        Self {
            flash: SimFlashStorage::new(),
            sensors: SimSensorManager::new(),
            gpio: SimGpioController::new(),
            ble: SimBleInterface::new(),
            health_state: HealthSimState::new(),
            sports_coach: SportsCoach::new(),
            exercise_goal_set: false,
            session_started: None,
            voice: VoiceNotificationManager::new(),
            boot: std::time::Instant::now(),
            sleep: SleepManager::new(),
            early_warning: simulated_emergency_registry(),
        }
    }

    /// Simulate one night of sleep, appended to [`Self::sleep`]'s history.
    ///
    /// A night is the one thing this simulator cannot observe — a run lasts
    /// seconds — so the record is generated from the same resting heart-rate
    /// and HRV model the daytime readings use, and its *quality score* is then
    /// computed by `SleepQuality::new`, not written here. Callers that never
    /// call this get no sleep data at all, which is what `get_sleep_quality`
    /// reports rather than inventing a night.
    pub fn simulate_night(&mut self, duration_min: u32) {
        // A night is rest, so read the resting model rather than whatever the
        // exercise state happens to be.
        let was_exercising = self.health_state.is_exercising;
        self.health_state.is_exercising = false;
        let (hr, hrv) = self.simulate_heart_rate();
        self.health_state.is_exercising = was_exercising;

        let hrv = hrv.clamp(20.0, 80.0);
        // Efficiency, wake count and onset latency all track overnight HRV:
        // a higher HRV (better parasympathetic tone) means fewer awakenings and
        // less time lying awake. The constants are a plausible model, not
        // measurements.
        let efficiency = (92.0 + (hrv - 50.0) * 0.2).clamp(70.0, 99.0);
        let deep = (duration_min as f32 * 0.18) as u16;
        let rem = (duration_min as f32 * 0.22) as u16;
        let wakes = (((60.0 - hrv) / 12.0).clamp(0.0, 8.0)) as u8;
        let latency = (((60.0 - hrv) / 6.0).clamp(1.0, 40.0)) as u8;

        let quality = SleepQuality::new(duration_min, efficiency, deep, rem, wakes, latency);

        // Bedtime 23:00; `duration_minutes()` then recovers `duration_min`
        // (it handles the wrap past midnight).
        let sleep_start_min: u16 = 23 * 60;
        let wake_time_min: u16 = ((sleep_start_min as u32 + duration_min) % 1440) as u16;

        let _ = self.sleep.add_sleep_record(SleepRecord {
            day_index: self.sleep.sleep_history().len() as u32 + 1,
            sleep_start_min,
            wake_time_min,
            quality,
            sleep_hrv: hrv,
            sleep_hr: hr,
            hrv_trend: 0,
        });
    }

    /// Milliseconds since the simulator was created. Used as the timestamp for
    /// queued voice messages and notifications: a monotonic uptime, since the
    /// device has no guaranteed wall clock.
    fn uptime_ms(&self) -> u32 {
        self.boot.elapsed().as_millis().min(u32::MAX as u128) as u32
    }

    /// Hour of day derived from uptime. A freshly-created simulator is at hour
    /// 0, which is what the device sees before its clock is set.
    fn current_hour(&self) -> u8 {
        ((u64::from(self.uptime_ms()) / 3_600_000) % 24) as u8
    }

    /// Execute a tool call
    pub fn execute_tool(&mut self, tool_name: &str, args: &str) -> Result<String> {
        match tool_name {
            "read_sensor" => {
                // Route on the *sensor argument*, not the payload as a whole —
                // this is what `tools.rs::execute_read_sensor` does, and the
                // two must agree: a prompt tuned with `--mock` has to behave
                // the same on the device it is being tuned for. The old
                // `args.contains("...")` chain matched any name anywhere in
                // the payload, so an unrelated field mentioning `heart_rate`
                // hijacked an `hrv` read; and it was case-sensitive with no
                // aliases, so `{"sensor":"Temperature"}` — the capitalisation
                // a model naturally emits — failed here while the firmware
                // read it fine.
                Ok(self.render_sensor(resolve_sensor(args)?))
            }
            // Aliases the system prompt documents as zero-argument calls.
            // They resolve on the device registry (`tools.rs`) and have to
            // resolve here too, or a prompt tuned against the simulator
            // behaves differently from the device it is tuned for.
            "read_heart_rate" => Ok(self.render_sensor(resolve_sensor_as("heart_rate", args)?)),
            "read_glucose" => Ok(self.render_sensor(resolve_sensor_as("glucose", args)?)),
            "read_ecg" => Ok(self.render_sensor(resolve_sensor_as("ecg", args)?)),
            "check_stress_level" => Ok(self.render_sensor(resolve_sensor_as("stress", args)?)),
            "get_sleep_quality" => {
                let Some(record) = self.sleep.latest_sleep() else {
                    // No night has been recorded. Reporting an overall score
                    // anyway would be a fabricated health claim about the user
                    // — the one thing a sleep metric must not do.
                    return Err(missing_field("sleep"));
                };
                let q = &record.quality;
                let mut out = format!(
                    "Last night: {} min in bed, {} min asleep, score {}/100 ({}), \
                     efficiency {:.0}%, deep {} min, REM {} min, {} wake-ups, \
                     onset {} min, sleep HRV {:.1} ms, HR {} BPM",
                    record.duration_minutes(),
                    q.duration_min,
                    q.score,
                    q.rating(),
                    q.efficiency,
                    q.deep_sleep_min,
                    q.rem_sleep_min,
                    q.wake_count,
                    q.onset_latency_min,
                    record.sleep_hrv,
                    record.sleep_hr,
                );
                // The history is the point of keeping one: a single night's
                // score means nothing without the trend behind it. Both figures
                // are capped at what has been recorded, so the label says how
                // many nights they are really over rather than claiming a week.
                let nights = self.sleep.sleep_history().len().min(7);
                if let Some(avg) = self.sleep.average_sleep_score(nights) {
                    out.push_str(&format!(" | last {nights} nights avg {avg:.0}"));
                }
                let debt = self.sleep.calculate_sleep_debt(nights);
                out.push_str(&format!(" | sleep debt {debt:.1} h"));
                if let Some(first) = self.sleep.get_sleep_recommendations().first() {
                    out.push_str(&format!(" | {first}"));
                }
                Ok(out)
            }
            "start_exercise" => {
                let raw = extract_str_arg(args, "type");
                let Some(kind) = raw.as_deref().and_then(parse_exercise_type) else {
                    // Naming no type and naming a type this build does not know
                    // are different mistakes: one is a missing field, the other
                    // an invalid value. Neither may silently become the default
                    // goal, which is how a typo turns into a fabricated session.
                    return Err(AgentError::ConfigurationError {
                        field: "type",
                        reason: if raw.is_none() {
                            crate::error::ConfigError::MissingField
                        } else {
                            crate::error::ConfigError::InvalidValue
                        },
                    });
                };
                self.sports_coach.set_goal(ExerciseGoal::new(kind));
                self.sports_coach.start_session();
                self.exercise_goal_set = true;
                // A running session is a fact the rest of the simulator can
                // see: `simulate_heart_rate` reads it, so the next
                // `read_heart_rate` returns an exercising heart rate rather
                // than a resting one.
                self.health_state.is_exercising = true;
                self.session_started = Some(std::time::Instant::now());
                Ok(format!(
                    "Started {} session — goal: {}",
                    kind.name(),
                    self.sports_coach.goal().format_summary()
                ))
            }
            "end_exercise" => {
                let Some(started) = self.session_started.take() else {
                    // Ending a session that was never started used to be a
                    // no-op that reported success. There is nothing to report;
                    // say so.
                    return Err(AgentError::ConfigurationError {
                        field: "exercise",
                        reason: crate::error::ConfigError::MissingField,
                    });
                };
                let elapsed_s = started.elapsed().as_secs().min(u32::MAX as u64) as u32;
                let kind = self.sports_coach.goal().exercise_type;
                // Advance the coach's own state machine by the time that really
                // passed, so the state it reports at the end is where the
                // session actually got to rather than where it started.
                let (hr, hrv) = self.simulate_heart_rate();
                let hr_data = HeartRateData::new(hr, hrv, None, 0);
                self.sports_coach.update(&hr_data, None, elapsed_s);
                self.sports_coach.end_session();
                self.health_state.is_exercising = false;
                Ok(format!(
                    "Ended {} session after {}s ({}s of {} min goal)",
                    kind.name(),
                    elapsed_s,
                    self.sports_coach.session_duration(),
                    self.sports_coach.goal().duration_min,
                ))
            }
            "get_exercise_goal" => {
                if !self.exercise_goal_set {
                    // Reporting `ExerciseGoal::default()` here would present a
                    // built-in default as the user's goal.
                    return Err(AgentError::ConfigurationError {
                        field: "exercise_goal",
                        reason: crate::error::ConfigError::MissingField,
                    });
                }
                let summary = self.sports_coach.goal().format_summary();
                Ok(summary.as_str().to_string())
            }
            "adjust_exercise_goal" => {
                if !self.exercise_goal_set {
                    return Err(AgentError::ConfigurationError {
                        field: "exercise_goal",
                        reason: crate::error::ConfigError::MissingField,
                    });
                }
                let intensity = extract_float_arg(args, "intensity");
                let duration = extract_int_arg(args, "duration");
                if intensity.is_none() && duration.is_none() {
                    return Err(AgentError::ConfigurationError {
                        field: "intensity",
                        reason: crate::error::ConfigError::MissingField,
                    });
                }
                let mut goal = self.sports_coach.goal().clone();
                let before = goal.format_summary();
                if let Some(i) = intensity {
                    // The goal is "percentage of max HR", so a value outside
                    // 0..=1 is a unit mistake (a model writing `150` when it
                    // means 1.5, or `70` for 70 %) — reject rather than clamp
                    // silently, which would hide the mistake.
                    if !(0.0..=1.0).contains(&i) {
                        return Err(AgentError::ConfigurationError {
                            field: "intensity",
                            reason: crate::error::ConfigError::InvalidValue,
                        });
                    }
                    goal.target_intensity = i;
                }
                if let Some(d) = duration {
                    if d <= 0 {
                        return Err(AgentError::ConfigurationError {
                            field: "duration",
                            reason: crate::error::ConfigError::InvalidValue,
                        });
                    }
                    goal.duration_min = d.min(u16::MAX as i32) as u16;
                }
                goal.add_adjustment("user-requested adjustment");
                self.sports_coach.set_goal(goal);
                Ok(format!(
                    "Exercise goal adjusted: {} → {}",
                    before,
                    self.sports_coach.goal().format_summary()
                ))
            }
            "queue_voice" => {
                let Some(text) = extract_str_arg(args, "text") else {
                    return Err(missing_field("text"));
                };
                // The prompt documents `priority` but not its scale; the module
                // uses 1..=10 and treats >= 8 as urgent enough to bypass
                // do-not-disturb. Anything outside that scale is a unit mistake,
                // so reject rather than clamp it into the queue.
                let priority = match extract_int_arg(args, "priority") {
                    Some(p) if !(1..=10).contains(&p) => return Err(invalid_value("priority")),
                    Some(p) => p as u8,
                    None => 5,
                };
                let ts = self.uptime_ms();
                let before = self.voice.voice_queue_size();
                self.voice
                    .queue_voice(&text, priority, VoiceCategory::System, ts)?;
                let depth = self.voice.voice_queue_size();
                // The queue not growing means one of two things: the manager
                // routed the cue elsewhere (do-not-disturb at priority < 8), or
                // a full queue replaced an equal-priority entry. Only the first
                // is a suppression, so check both rather than blaming DND for a
                // queue that simply stayed the same size.
                if depth == before && self.voice.is_in_dnd_period(self.current_hour()) {
                    Ok(format!(
                        "Voice message not queued — do-not-disturb is active (queue depth {depth})"
                    ))
                } else {
                    Ok(format!(
                        "Queued voice message (priority {priority}, queue depth {depth})"
                    ))
                }
            }
            "send_health_alert" => {
                let Some(alert_type) = extract_str_arg(args, "type") else {
                    return Err(missing_field("type"));
                };
                let Some(severity) = extract_int_arg(args, "severity") else {
                    return Err(missing_field("severity"));
                };
                if !(1..=10).contains(&severity) {
                    return Err(invalid_value("severity"));
                }
                let Some(message) = extract_str_arg(args, "message") else {
                    return Err(missing_field("message"));
                };
                let ts = self.uptime_ms();
                // An alert's channels are not all-or-nothing: the manager
                // routes the cue and the notification independently, and under
                // do-not-disturb it can drop either (priority < 7). Reporting
                // "delivered" unconditionally would be the old fabricated-success
                // bug in a new place, so capture both channels before and after
                // and report what the pipeline actually did.
                let voice_before = self.voice.voice_queue_size();
                let notes_before = self.voice.notification_history().len();
                // The prompt carries no `recommendation` argument; the module's
                // alert body is "{message}。{recommendation}", so an empty
                // recommendation yields a plain sentence rather than invented
                // advice the model never gave.
                self.voice
                    .send_health_alert(&alert_type, severity as u8, &message, "", ts)?;
                Ok(format!(
                    "Health alert {alert_type} (severity {severity}): voice {}, notification {}",
                    if self.voice.voice_queue_size() > voice_before {
                        "queued"
                    } else {
                        "not queued"
                    },
                    if self.voice.notification_history().len() > notes_before {
                        "raised"
                    } else {
                        "not raised"
                    }
                ))
            }
            "send_meditation" => {
                let Some(script) = extract_str_arg(args, "script") else {
                    return Err(missing_field("script"));
                };
                let ts = self.uptime_ms();
                self.voice.send_meditation(&script, ts)?;
                Ok(format!(
                    "Meditation script queued (queue depth {})",
                    self.voice.voice_queue_size()
                ))
            }
            "set_dnd" => {
                // Absent and present-but-not-a-boolean are different mistakes:
                // `{"enabled":"maybe"}` must not read as "you forgot the field".
                let enabled = match extract_bool_arg(args, "enabled") {
                    Some(b) => b,
                    None if extract_str_arg(args, "enabled").is_some() => {
                        return Err(invalid_value("enabled"));
                    }
                    None => return Err(missing_field("enabled")),
                };
                let start = match extract_int_arg(args, "start_hour") {
                    Some(h) => h,
                    None => return Err(missing_field("start_hour")),
                };
                let end = match extract_int_arg(args, "end_hour") {
                    Some(h) => h,
                    None => return Err(missing_field("end_hour")),
                };
                // An hour is 0..=23. Wrapping 24 to 0 would silently invert the
                // window (a `24..7` window becomes `0..7`), so reject it.
                if !(0..=23).contains(&start) {
                    return Err(invalid_value("start_hour"));
                }
                if !(0..=23).contains(&end) {
                    return Err(invalid_value("end_hour"));
                }
                self.voice.set_dnd(enabled, start as u8, end as u8);
                let now = self.current_hour();
                Ok(format!(
                    "Do-not-disturb {} ({}:00–{}:00), currently {}",
                    if enabled { "enabled" } else { "disabled" },
                    start,
                    end,
                    if self.voice.is_in_dnd_period(now) {
                        "active"
                    } else {
                        "inactive"
                    }
                ))
            }
            "notify_emergency_contact" => {
                let requested = extract_str_arg(args, "name")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                // No registry and no *match* are different mistakes. Nothing
                // configured means the device cannot reach anyone — there is no
                // contact to fall back to, and picking one would notify nobody
                // (or, worse, a stranger). A name that is not in the registry is
                // the caller's error, and is worth distinguishing so the model
                // can correct it.
                let contacts = self.early_warning.emergency_contacts();
                if contacts.is_empty() {
                    return Err(missing_field("contact"));
                }
                let contact = match &requested {
                    Some(name) => contacts
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(name))
                        .cloned()
                        .ok_or_else(|| invalid_value("name"))?,
                    // No name given: `add_emergency_contact` keeps the list
                    // sorted by priority, and the point of that ordering is to
                    // have a primary. Take the first SMS-capable entry rather
                    // than refusing a request whose whole intent was "reach my
                    // emergency contact".
                    None => contacts
                        .iter()
                        .find(|c| c.sms_enabled)
                        .cloned()
                        .ok_or_else(|| missing_field("contact"))?,
                };
                // The prompt documents `message`, but a caller that omits it
                // still asked to reach someone; refusing would be pedantry, and
                // inventing medical detail would be worse. Say the one thing
                // that is certainly true.
                let message = extract_str_arg(args, "message").unwrap_or_else(|| {
                    "The device user is trying to reach you and needs help.".to_string()
                });
                let ts = self.uptime_ms();
                // The module decides *what* goes out and *to whom*; it has no
                // transport, so the notification is queued and read back to
                // prove it landed rather than assumed.
                let queued = self
                    .early_warning
                    .queue_notification(&contact, &message, ts);
                let landed = self
                    .early_warning
                    .pending_emergency_notification()
                    .is_some_and(|p| p.contact_phone == queued.contact_phone);
                Ok(format!(
                    "Emergency notification for {} ({}) {} — {}",
                    queued.contact_name,
                    queued.contact_phone,
                    if landed {
                        "queued, awaiting delivery"
                    } else {
                        "NOT queued"
                    },
                    queued.message
                ))
            }
            "find_nearest_hospital" => {
                let specialty = extract_str_arg(args, "specialty")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                if self.early_warning.hospitals().is_empty() {
                    return Err(missing_field("hospital"));
                }
                // `nearest_er` and `nearest_with_specialty` both scan a list the
                // module keeps sorted by distance, so "nearest with cardiology"
                // is decided there rather than by re-sorting here.
                let found = match specialty.as_str() {
                    "" | "er" | "emergency" | "emergency room" => self.early_warning.nearest_er(),
                    "cardiology" | "cardiac" | "heart" => {
                        self.early_warning.nearest_with_specialty(true, false)
                    }
                    "endocrinology" | "endocrine" | "diabetes" => {
                        self.early_warning.nearest_with_specialty(false, true)
                    }
                    _ => return Err(invalid_value("specialty")),
                };
                let Some(h) = found else {
                    // The specialty is one this build understands, but no
                    // registered hospital has it. Reporting a different
                    // hospital as if it were the match would send someone to
                    // the wrong department.
                    return Err(missing_field("hospital"));
                };
                Ok(format!(
                    "{} — {}, {} ({}){}",
                    h.name,
                    h.address,
                    h.phone,
                    h.distance_string(),
                    if h.has_er { ", ER" } else { "" }
                ))
            }
            "write_gpio" => {
                // `as u8` truncated: pin=256 became 0 and passed the range
                // check, so the tool reported success for the wrong pin.
                let pin = u8::try_from(required_int_arg(args, "pin")?).map_err(|_| {
                    AgentError::ConfigurationError {
                        field: "pin",
                        reason: crate::error::ConfigError::InvalidValue,
                    }
                })?;
                // Match the embedded executor (`tools.rs::execute_write_gpio`)
                // rather than the old substring test: accept the same
                // `high|1` / `low|0` spellings, default to `high` when the
                // state is absent, and **reject** anything else. Deciding by
                // `args.contains("high")` drove the pin *low* for `state:"1"`
                // and for any payload that omitted the state — the opposite
                // of what the firmware does — and silently accepted
                // nonsense. A prompt tuned here has to behave the same on
                // the device it is being tuned for.
                let raw_state = extract_str_arg(args, "state");
                let state = match raw_state.as_deref().map(str::trim) {
                    Some("high") | Some("1") => GpioPinState::High,
                    Some("low") | Some("0") => GpioPinState::Low,
                    None => GpioPinState::High,
                    Some(_) => {
                        return Err(AgentError::ConfigurationError {
                            field: "state",
                            reason: crate::error::ConfigError::InvalidValue,
                        })
                    }
                };
                self.gpio.set_pin(pin, state)?;
                let state_str = if matches!(state, GpioPinState::High) {
                    "high"
                } else {
                    "low"
                };
                Ok(format!("GPIO pin {} set to {}", pin, state_str))
            }
            "flash_read" => {
                // Negative addresses used to wrap through `as u32` and were
                // only caught later by the flash bounds check; reject them
                // (and values above u32) at the argument boundary.
                let address = u32::try_from(required_int_arg(args, "address")?).map_err(|_| {
                    AgentError::ConfigurationError {
                        field: "address",
                        reason: crate::error::ConfigError::InvalidValue,
                    }
                })?;
                let data = self.flash.read(address, 64)?;
                let hex: String = data.iter().take(16).map(|b| format!("{:02X}", b)).collect();
                Ok(format!("Flash read at 0x{:04X}: {}", address, hex))
            }
            "flash_write" => {
                let address = u32::try_from(required_int_arg(args, "address")?).map_err(|_| {
                    AgentError::ConfigurationError {
                        field: "address",
                        reason: crate::error::ConfigError::InvalidValue,
                    }
                })?;
                // Store what the caller asked to store. This used to write
                // the literal `data at <address>` — the payload argument was
                // discarded — so a write-then-read round trip handed the
                // model back text it never wrote, and the reported byte count
                // described that fabricated string rather than its data. Both
                // the embedded executor (`tools.rs`) and the host simulator
                // already persist the `data` value; this now matches them.
                let data = extract_str_arg(args, "data").unwrap_or_default();
                self.flash.write(address, data.as_bytes())?;
                Ok(format!(
                    "Wrote {} bytes to flash at 0x{:04X}",
                    data.len(),
                    address
                ))
            }
            "ble_send" => {
                // Send the payload, not the argument envelope. This used to
                // forward the raw args string and report its length, so
                // `{"data":"System health OK"}` went over the wire as
                // 27 bytes of JSON — braces, quotes and key name included —
                // for a 16-character message.
                let data = extract_str_arg(args, "data").unwrap_or_default();
                self.ble.send(&data)?;
                Ok(format!("Sent {} bytes via BLE", data.len()))
            }
            "web_search" => {
                // Delegate to the dedicated web module so the heavy
                // lifting (regex, reqwest, HTML stripping) lives in
                // one place. Tool failures are surfaced as `Ok` with
                // a leading "error:" prefix so the LLM sees the
                // failure (the agent loop matches on the prefix).
                match crate::web::web_search(args) {
                    Ok(s) => Ok(s),
                    Err(e) => Ok(format!("error: web_search failed: {e}")),
                }
            }
            "fetch_url" => match crate::web::fetch_url(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: fetch_url failed: {e}")),
            },
            "webpage_summary" => match crate::web::webpage_summary(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: webpage_summary failed: {e}")),
            },
            "get_weather" => match crate::web::get_weather(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: get_weather failed: {e}")),
            },
            _ => Err(AgentError::ConfigurationError {
                field: "tool",
                reason: crate::error::ConfigError::MissingField,
            }),
        }
    }

    /// Render one sensor reading. Split out of the `read_sensor` arm so the
    /// zero-argument aliases (`read_heart_rate`, `read_glucose`, `read_ecg`,
    /// `check_stress_level`) reuse exactly the same reading and formatting.
    fn render_sensor(&mut self, sensor: SimSensorType) -> String {
        match sensor {
            SimSensorType::Temperature => {
                let temp = self.sensors.read_temperature();
                format!("Temperature: {:.1}°C", temp)
            }
            SimSensorType::Accelerometer => {
                let (x, y, z) = self.sensors.read_accelerometer();
                // Note: the `f` float-formatting flag is not available
                // when building under `#![no_std]` (the `format_args!`
                // macro falls back to the smaller `core::fmt` that
                // doesn't ship float support). We rely on the natural
                // `Display` impl for `f32`, which gives 6 fractional
                // digits by default.
                format!("Accelerometer: X={}g Y={}g Z={}g", x, y, z)
            }
            SimSensorType::Humidity => {
                let humidity = self.sensors.read_humidity();
                format!("Humidity: {:.1}%", humidity)
            }
            SimSensorType::Pressure => {
                let pressure = self.sensors.read_pressure();
                format!("Pressure: {:.1} hPa", pressure)
            }
            SimSensorType::Light => {
                let light = self.sensors.read_light();
                format!("Light: {:.1} lux", light)
            }
            SimSensorType::HeartRate => {
                let hr = self.simulate_heart_rate();
                format!("Heart Rate: {} BPM, HRV: {:.1} ms", hr.0, hr.1)
            }
            SimSensorType::Hrv => {
                let hrv = self.simulate_hrv();
                format!("HRV: {:.1} ms, Stress Level: {}", hrv.0, hrv.1)
            }
            SimSensorType::Glucose => {
                let glucose = self.simulate_glucose();
                format!("Glucose: {:.0} mg/dL, Trend: {}", glucose.0, glucose.1)
            }
            SimSensorType::Ecg => {
                let ecg = self.simulate_ecg();
                format!(
                    "ECG: HR={} BPM, Rhythm={}, Quality={}%",
                    ecg.0, ecg.1, ecg.2
                )
            }
            SimSensorType::Stress => {
                let stress = self.calculate_stress();
                format!(
                    "Stress Level: {} ({}), HRV: {:.1} ms",
                    stress.0, stress.1, stress.2
                )
            }
            SimSensorType::Battery => {
                let mv = self.sensors.read_battery_mv();
                // 3200 mV = empty, 4200 mV = full, the usual LiPo window.
                let pct = ((mv.saturating_sub(3200)) as f32 / 10.0).min(100.0);
                format!("Battery: {:.0}%, Voltage:{}mV", pct, mv)
            }
            SimSensorType::Memory => {
                let free = self.sensors.read_free_heap();
                format!("free_heap={} B", free)
            }
        }
    }

    /// Simulate heart rate based on exercise state
    fn simulate_heart_rate(&mut self) -> (u16, f32) {
        self.health_state.minutes_elapsed += 1;

        let base_hr = if self.health_state.is_exercising {
            140
        } else {
            70
        };
        let variation = ((self.health_state.minutes_elapsed as f32 * 0.1).sin() * 10.0) as i16;
        let hr = (base_hr as i16 + variation).clamp(50, 200) as u16;

        let base_hrv = if self.health_state.is_exercising {
            20.0
        } else {
            55.0
        };
        let hrv = base_hrv + ((self.health_state.minutes_elapsed as f32 * 0.2).sin() * 5.0);

        self.health_state.last_hr = hr;
        self.health_state.last_hrv = hrv;

        (hr, hrv)
    }

    /// Simulate HRV based on stress level
    fn simulate_hrv(&mut self) -> (f32, u8) {
        // HRV decreases with stress
        let base_hrv = 80.0 - (self.health_state.stress_level as f32 * 0.5);
        let hrv = base_hrv + ((self.health_state.minutes_elapsed as f32 * 0.15).sin() * 5.0);
        self.health_state.last_hrv = hrv;
        (hrv, self.health_state.stress_level)
    }

    /// Simulate blood glucose
    fn simulate_glucose(&mut self) -> (f32, &'static str) {
        self.health_state.hours_since_meal += 0.5;
        let base = 100.0;
        let rise = (self.health_state.hours_since_meal * 20.0).min(50.0);
        let variation = (self.health_state.hours_since_meal * 0.5).sin() * 10.0;
        let glucose = base + rise + variation;
        self.health_state.last_glucose = glucose;

        let trend = if self.health_state.hours_since_meal < 2.0 {
            "Rising"
        } else if self.health_state.hours_since_meal < 4.0 {
            "Stable"
        } else {
            "Falling"
        };

        (glucose, trend)
    }

    /// Simulate ECG data
    fn simulate_ecg(&mut self) -> (u16, &'static str, u8) {
        let (hr, _) = self.simulate_heart_rate();
        let rhythm = if self.health_state.stress_level > 70 {
            "Irregular"
        } else {
            "Normal"
        };
        (hr, rhythm, 95)
    }

    /// Calculate stress from HRV
    fn calculate_stress(&self) -> (u8, &'static str, f32) {
        let hrv = self.health_state.last_hrv;
        let (level, desc) = if hrv >= 80.0 {
            (20u8, "Low")
        } else if hrv >= 50.0 {
            (50u8, "Moderate")
        } else if hrv >= 25.0 {
            (75u8, "High")
        } else {
            (95u8, "Very High")
        };
        (level, desc, hrv)
    }

    /// Set exercise state
    pub fn set_exercising(&mut self, exercising: bool) {
        self.health_state.is_exercising = exercising;
    }

    /// Set stress level
    pub fn set_stress_level(&mut self, level: u8) {
        self.health_state.stress_level = level.clamp(0, 100);
    }

    /// Set hours since meal
    pub fn set_hours_since_meal(&mut self, hours: f32) {
        self.health_state.hours_since_meal = hours;
    }

    /// Get system status
    pub fn get_status(&self) -> String {
        format!(
            "Simulator Status:\n  Flash writes: {}\n  BLE messages: {}\n  BLE connected: {}\n  Health State: HR={} BPM, HRV={:.1} ms, Stress={}",
            self.flash.write_count(),
            self.ble.message_count(),
            self.ble.is_connected(),
            self.health_state.last_hr,
            self.health_state.last_hrv,
            self.health_state.stress_level
        )
    }
}

impl Default for AgentSimulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract integer argument from string
/// Read an integer tool argument, in either of the two shapes that reach this
/// code.
///
/// The ReAct loop serialises `ToolCall::arguments` as **JSON** — that is what
/// `agent_runner::execute_tool` hands the executor, and what a real model
/// produces: `{"pin":13,"state":"high"}`. The older scripted callers and
/// fixture-style tests pass `key=value` pairs instead: `pin=13`. Only the
/// second shape used to be understood, so every JSON payload silently lost its
/// integer argument and fell back to the default — `write_gpio` with
/// `{"pin":13}` set pin **0** and reported success, and `flash_read`/
/// `flash_write` read `0x0000` for any address a model asked for. JSON is
/// tried first because it is the shape the loop actually sends; a number may
/// also arrive as a JSON string (`{"pin":"13"}`), which some models emit.
fn extract_int_arg(s: &str, key: &str) -> Option<i32> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(s) {
        let v = map.get(key)?;
        if let Some(n) = v.as_i64() {
            return i32::try_from(n).ok();
        }
        if let Some(t) = v.as_str() {
            return t.trim().parse().ok();
        }
        return None;
    }
    let prefix = format!("{}=", key);
    s.find(&prefix).and_then(|pos| {
        let start = pos + prefix.len();
        let end = s[start..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| start + i)
            .unwrap_or(s.len());
        s[start..end].parse().ok()
    })
}

/// Read a string tool argument, in either shape [`extract_int_arg`] accepts —
/// the JSON object the ReAct loop sends, or the legacy `key=value` form.
///
/// A JSON number or bool is rendered as its text (`{"state":1}` → `"1"`),
/// matching the spellings the embedded executor accepts.
/// The emergency profile a provisioned device carries: who to reach, and which
/// hospitals are nearby.
///
/// This is simulated data of the same kind as the sensor readings — a device
/// has no way to discover either — and it is deliberately **not** empty, since
/// an empty registry would make `notify_emergency_contact` and
/// `find_nearest_hospital` unable to do anything but fail. Two properties of
/// the reply keep that honest: it reports the notification as *queued*, never
/// as sent (`magent-core` has no transport), and the tool errors when a caller
/// names a contact or specialty the registry does not hold, rather than
/// falling back to some other contact.
fn simulated_emergency_registry() -> EarlyWarningSystem {
    let mut ews = EarlyWarningSystem::new();
    for (name, phone, relationship, priority) in [
        ("Alex (primary)", "+1-555-0100", "spouse", 1u8),
        ("Sam (backup)", "+1-555-0101", "sibling", 2u8),
    ] {
        let _ =
            ews.add_emergency_contact(EmergencyContact::new(name, phone, relationship, priority));
    }
    for (name, address, phone, distance_m, cardiology, endocrinology) in [
        (
            "Central General Hospital",
            "12 Main St",
            "+1-555-0200",
            1200u32,
            true,
            false,
        ),
        (
            "Riverside Clinic",
            "88 River Rd",
            "+1-555-0201",
            400u32,
            false,
            false,
        ),
        (
            "Northside Endocrine Center",
            "5 North Ave",
            "+1-555-0202",
            3500u32,
            false,
            true,
        ),
    ] {
        let mut h = Hospital::new(name, address, phone);
        h.distance_m = Some(distance_m);
        h.has_cardiology = cardiology;
        h.has_endocrinology = endocrinology;
        let _ = ews.add_hospital(h);
    }
    ews
}

fn extract_str_arg(s: &str, key: &str) -> Option<String> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(s) {
        return match map.get(key)? {
            serde_json::Value::String(t) => Some(t.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        };
    }
    let prefix = format!("{}=", key);
    let pos = s.find(&prefix)?;
    let rest = &s[pos + prefix.len()..];
    let end = rest
        .find(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// The sensor a `read_sensor` call names, in either of the shapes the
/// executor accepts: the `sensor` argument, or the legacy bare token
/// (`read_sensor temperature`). Mirrors `tools.rs::execute_read_sensor`, which
/// prefers the explicit key and otherwise takes the first bare token.
fn sensor_arg(args: &str) -> Option<String> {
    if let Some(v) = extract_str_arg(args, "sensor") {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    // A bare token carries no `=` and is not an object payload.
    let t = args.trim();
    if !t.is_empty() && !t.contains('=') && !t.starts_with('{') {
        return Some(t.to_string());
    }
    None
}

/// Map a sensor name to the reading it selects, folding case and the aliases
/// the firmware's `normalize_sensor` accepts — `temp`, `HR`, `bg`, `accel`
/// and the rest. `None` means the name is not one this executor can read.
fn parse_sensor(name: &str) -> Option<SimSensorType> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "temperature" | "temp" | "die_temp" => SimSensorType::Temperature,
        "accelerometer" | "accel" | "imu" => SimSensorType::Accelerometer,
        "humidity" | "humid" => SimSensorType::Humidity,
        "pressure" | "press" | "baro" => SimSensorType::Pressure,
        "light" | "lux" | "als" => SimSensorType::Light,
        "heart_rate" | "heartrate" | "hr" | "pulse" => SimSensorType::HeartRate,
        "hrv" | "hrv_ms" => SimSensorType::Hrv,
        "glucose" | "bg" | "blood_glucose" => SimSensorType::Glucose,
        "ecg" | "ekg" => SimSensorType::Ecg,
        "stress" | "stress_level" => SimSensorType::Stress,
        "battery" | "batt" => SimSensorType::Battery,
        "memory" | "mem" | "heap" | "free_heap" | "sram" => SimSensorType::Memory,
        _ => return None,
    })
}

/// Read a fractional tool argument (`intensity=0.7`), in either the JSON
/// shape the agent loop sends or the legacy `key=value` shape. Kept separate
/// from [`extract_int_arg`] rather than widening it: `0.7` parses as neither
/// an `i64` nor a digit run, so an integer reader would report the argument as
/// absent — turning "set intensity to 0.7" into a missing-field error.
fn extract_float_arg(s: &str, key: &str) -> Option<f32> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(s) {
        let v = map.get(key)?;
        if let Some(n) = v.as_f64() {
            return Some(n as f32);
        }
        if let Some(t) = v.as_str() {
            return t.trim().parse().ok();
        }
        return None;
    }
    let prefix = format!("{}=", key);
    let pos = s.find(&prefix)?;
    let start = pos + prefix.len();
    let end = s[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| start + i)
        .unwrap_or(s.len());
    s[start..end].parse().ok()
}

/// Read a boolean tool argument, in either shape [`extract_int_arg`] accepts.
/// A model emits the JSON spelling (`{"enabled":true}`) or the quoted one
/// (`{"enabled":"true"}`); the legacy form is `enabled=1`.
///
/// An unreadable value is `None` — the caller distinguishes "absent" from
/// "present but not a boolean" by asking [`extract_str_arg`] whether the key
/// exists at all.
fn extract_bool_arg(s: &str, key: &str) -> Option<bool> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(s) {
        return match map.get(key)? {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::String(t) => parse_bool(t),
            serde_json::Value::Number(n) => match n.as_i64()? {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
            _ => None,
        };
    }
    parse_bool(&extract_str_arg(s, key)?)
}

/// The spellings of true/false the firmware and models use.
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "enabled" => Some(true),
        "false" | "0" | "off" | "no" | "disabled" => Some(false),
        _ => None,
    }
}

/// A required argument the call omitted.
fn missing_field(field: &'static str) -> AgentError {
    AgentError::ConfigurationError {
        field,
        reason: crate::error::ConfigError::MissingField,
    }
}

/// An argument that is present but outside the values this build accepts.
fn invalid_value(field: &'static str) -> AgentError {
    AgentError::ConfigurationError {
        field,
        reason: crate::error::ConfigError::InvalidValue,
    }
}

/// Map an exercise name to the activity class it selects. Accepts the
/// spellings the CLI's system prompt documents (`running`, `walking`,
/// `cycling`, `swimming`, `strength`, `yoga`, `hiit`) plus the obvious
/// near-synonyms a model reaches for. `None` means "not a type this build
/// knows" — which is *not* the same as the default, and must not become it.
fn parse_exercise_type(name: &str) -> Option<ExerciseType> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "running" | "run" | "jog" | "jogging" => ExerciseType::Running,
        "walking" | "walk" => ExerciseType::Walking,
        "cycling" | "cycle" | "bike" | "biking" => ExerciseType::Cycling,
        "swimming" | "swim" => ExerciseType::Swimming,
        "strength" | "strength_training" | "weights" | "resistance" => ExerciseType::Strength,
        "yoga" => ExerciseType::Yoga,
        "hiit" | "interval" | "intervals" => ExerciseType::Hiit,
        "rest" | "recovery" => ExerciseType::Rest,
        _ => return None,
    })
}

/// Read a tool argument that the call has to name. An absent key is a missing
/// field, not zero: `unwrap_or(0)` turned a malformed `write_gpio` into a real
/// side effect on pin 0, and `flash_write` with no address into a write at
/// address 0.
fn required_int_arg(args: &str, key: &'static str) -> Result<i32> {
    extract_int_arg(args, key).ok_or(AgentError::ConfigurationError {
        field: key,
        reason: crate::error::ConfigError::MissingField,
    })
}

/// Resolve the sensor named by `args`, or fail. Nothing naming a sensor at all
/// is a missing field; a name that is present but unrecognised is an invalid
/// value.
fn resolve_sensor(args: &str) -> Result<SimSensorType> {
    let raw = sensor_arg(args);
    match raw.as_deref().and_then(parse_sensor) {
        Some(sensor) => Ok(sensor),
        None => Err(AgentError::ConfigurationError {
            field: "sensor",
            reason: if raw.is_none() {
                crate::error::ConfigError::MissingField
            } else {
                crate::error::ConfigError::InvalidValue
            },
        }),
    }
}

/// Resolve the sensor for an alias (`read_heart_rate`, ...) whose `implied`
/// sensor stands in only when the caller named none, so an explicit
/// `sensor=` argument still wins.
fn resolve_sensor_as(implied: &str, args: &str) -> Result<SimSensorType> {
    let raw = sensor_arg(args);
    let name = raw.as_deref().unwrap_or(implied);
    parse_sensor(name).ok_or_else(|| AgentError::ConfigurationError {
        field: "sensor",
        reason: if raw.is_none() {
            // Unreachable while the alias table and `parse_sensor` agree;
            // reporting "missing" beats panicking if they ever diverge.
            crate::error::ConfigError::MissingField
        } else {
            crate::error::ConfigError::InvalidValue
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The LLM-shaped payload that `agent_runner::execute_tool`
    /// serialises via `serde_json::to_string`. We assert that the
    /// web tools reach the HTTP layer instead of bouncing back
    /// with "missing url" — the regression that motivated the JSON
    /// parser in `web::extract_query`.
    ///
    /// We don't actually hit the network here: the `fetch_url` path
    /// rejects any URL that doesn't start with `http(s)`, so the
    /// empty-string URL gives us a deterministic "refusing non-http(s)
    /// URL" error which is the layer we want to reach.
    #[test]
    fn execute_tool_routes_web_search_to_web_module() {
        let mut sim = AgentSimulator::new();
        // `query=` ... `=http` makes a key=value arg that
        // `extract_query` can parse. The actual search will fail
        // (no network in unit tests), but the failure surface
        // proves the routing worked.
        let result = sim.execute_tool("web_search", "query=rust");
        assert!(result.is_ok());
        let body = result.unwrap();
        // Either an answer (network is reachable) or a placeholder
        // error string — both prove we reached `web::web_search`.
        assert!(
            body.starts_with('[') || body.starts_with("error: web_search failed"),
            "expected JSON hits or error, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_fetch_url_with_json_args() {
        // The LLM emits arguments as `{"url":"..."}`. The previous
        // implementation fed that string to a `key=value` parser and
        // always returned "missing url" — the dispatch was 100%
        // broken for any LLM-driven caller. We verify the round-trip
        // reaches the URL validator (which rejects non-http(s)).
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool("fetch_url", r#"{"url":""}"#);
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            body.starts_with("error: fetch_url failed"),
            "expected fetch_url to be called, got {body}"
        );
        // The error message should reference the URL scheme check.
        assert!(
            body.contains("refusing non-http"),
            "expected URL scheme rejection, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_fetch_url_with_kv_args() {
        // The embedded planner (no JSON support) emits `url=...`.
        // This path also has to keep working.
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool("fetch_url", "url=");
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(body.contains("error: fetch_url failed"));
    }

    #[test]
    fn execute_tool_routes_webpage_summary_with_json_args() {
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool(
            "webpage_summary",
            r#"{"url":"https://example.com","query":"test"}"#,
        );
        // Network may or may not be available in tests; either way
        // we should NOT see "missing url" because the JSON parser
        // finds the URL.
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            !body.contains("missing 'url'"),
            "webpage_summary should have parsed the JSON url, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_get_weather_with_json_args() {
        let mut sim = AgentSimulator::new();
        // Empty city is a cheap, network-independent failure that still
        // proves the dispatch reached `web::get_weather` (and parsed the
        // JSON `city` arg) rather than bouncing back as an unknown tool.
        let result = sim.execute_tool("get_weather", r#"{"city":""}"#);
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            body.starts_with("error: get_weather failed"),
            "expected get_weather to be called, got {body}"
        );
        assert!(
            body.contains("empty city"),
            "expected the empty-city guard, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_get_weather_with_kv_args() {
        let mut sim = AgentSimulator::new();
        // The embedded planner (no JSON support) emits `city=...`.
        let result = sim.execute_tool("get_weather", "city=");
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(body.contains("error: get_weather failed"));
    }

    /// `read_sensor` routed on `args.contains(...)` over the *whole* payload
    /// while the firmware routes on the parsed `sensor=` value, so the two
    /// disagreed on what a sensor name is: the embedded `normalize_sensor`
    /// lowercases and accepts aliases (`temp`, `HR`, `bg`, `accel`), and this
    /// branch did neither. A model that capitalised a name — `"Temperature"`
    /// — failed outright on the simulator and worked on the device.
    #[test]
    fn read_sensor_accepts_the_names_and_case_the_firmware_accepts() {
        let cases: &[(&str, &str)] = &[
            (r#"{"sensor":"Temperature"}"#, "°C"),
            (r#"{"sensor":"temp"}"#, "°C"),
            (r#"{"sensor":"HR"}"#, "BPM"),
            (r#"{"sensor":"accel"}"#, "X="),
            (r#"{"sensor":"bg"}"#, "mg/dL"),
            (r#"{"sensor":"ekg"}"#, "Rhythm"),
        ];
        for (args, needle) in cases {
            let out = AgentSimulator::new()
                .execute_tool("read_sensor", args)
                .unwrap_or_else(|e| panic!("{args} should be readable, got {e:?}"));
            assert!(out.contains(needle), "{args} produced {out:?}");
        }
    }

    /// The system prompt (`HEALTH_SYSTEM_PROMPT`, the CLI's default) documents
    /// `read_heart_rate()`, `read_glucose()`, `read_ecg()` and
    /// `check_stress_level()` as taking **no arguments**, and the device
    /// registry resolves them as `read_sensor` aliases. The host executor had
    /// no arm for any of them, so every one of those advertised calls fell to
    /// the unknown-tool error and no answer ever reached the model.
    #[test]
    fn read_sensor_aliases_resolve_on_the_host_executor() {
        for (tool, needle) in [
            ("read_heart_rate", "BPM"),
            ("read_glucose", "mg/dL"),
            ("read_ecg", "Rhythm"),
            ("check_stress_level", "Stress Level"),
        ] {
            let out = AgentSimulator::new()
                .execute_tool(tool, "")
                .unwrap_or_else(|e| panic!("{tool}() must resolve, got {e:?}"));
            assert!(out.contains(needle), "{tool}() produced {out:?}");
        }
        // An explicit sensor still wins over the alias's implied one.
        let out = AgentSimulator::new()
            .execute_tool("read_heart_rate", "sensor=hrv")
            .unwrap();
        assert!(out.starts_with("HRV:"), "got {out:?}");
    }

    #[test]
    fn read_sensor_routes_on_the_sensor_value_not_the_whole_payload() {
        // The value is `hrv`; the extra field merely mentions `heart_rate`.
        // Matching the payload as a whole answered the wrong sensor — the
        // same order-coupled failure `tools.rs`'s comment records.
        let out = AgentSimulator::new()
            .execute_tool(
                "read_sensor",
                r#"{"sensor":"hrv","source":"heart_rate sensor"}"#,
            )
            .unwrap();
        assert!(out.starts_with("HRV:"), "got {out:?}");
        assert!(!out.contains("BPM"), "got {out:?}");
        // `hrv` must not be shadowed by `heart_rate`, whose substring lives
        // inside the combined reading the firmware used to hand back.
        let out = AgentSimulator::new()
            .execute_tool("read_sensor", "sensor=hrv")
            .unwrap();
        assert!(out.starts_with("HRV:"), "got {out:?}");
        // The legacy bare-token form still resolves.
        let out = AgentSimulator::new()
            .execute_tool("read_sensor", "heart_rate")
            .unwrap();
        assert!(out.contains("BPM"), "got {out:?}");
    }

    /// A name that is present but unrecognised is an *invalid value*, not a
    /// missing field — the distinction the GPIO `state` fix also drew.
    #[test]
    fn read_sensor_distinguishes_an_unknown_name_from_a_missing_one() {
        let err = AgentSimulator::new()
            .execute_tool("read_sensor", r#"{"sensor":"zzz"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "sensor",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );
        let err = AgentSimulator::new()
            .execute_tool("read_sensor", r#"{"note":"no sensor named"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "sensor",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
    }

    /// The host simulator and the device registry (`tools.rs`) document a
    /// shared sensor vocabulary — the comment on the `read_sensor` arm says
    /// the two "must agree", or a prompt tuned with `--mock` behaves
    /// differently from the device it is being tuned for. They did not:
    /// `tools.rs` resolves `battery` and `memory` and the embedded heuristic
    /// (`MiniAgent::pick_tool`) emits both, but this executor rejected them
    /// with an `InvalidValue` error. Every canonical name must resolve here.
    #[test]
    fn every_canonical_sensor_resolves_on_the_host_executor() {
        // Anti-vacuity: the list is the contract, so a truncated or empty
        // list must not silently pass.
        assert!(
            crate::tools::SENSOR_NAMES.len() >= 12,
            "the canonical sensor list shrank to {:?}",
            crate::tools::SENSOR_NAMES
        );
        for expected in ["temperature", "heart_rate", "stress", "battery", "memory"] {
            assert!(
                crate::tools::SENSOR_NAMES.contains(&expected),
                "canonical list lost {expected}: {:?}",
                crate::tools::SENSOR_NAMES
            );
        }
        for name in crate::tools::SENSOR_NAMES {
            let mut sim = AgentSimulator::new();
            let out = sim
                .execute_tool("read_sensor", &format!(r#"{{"sensor":"{name}"}}"#))
                .unwrap_or_else(|e| panic!("sensor {name} must resolve on the host, got {e:?}"));
            assert!(!out.trim().is_empty(), "sensor {name} rendered nothing");
        }
        // And the aliases the firmware accepts resolve here too.
        for alias in ["batt", "heap", "free_heap", "sram"] {
            let out = AgentSimulator::new()
                .execute_tool("read_sensor", &format!("sensor={alias}"))
                .unwrap_or_else(|e| panic!("alias {alias} must resolve, got {e:?}"));
            assert!(!out.trim().is_empty(), "alias {alias} rendered nothing");
        }
    }

    /// The prompt's advertised sensor list and the canonical list
    /// (`tools::SENSOR_NAMES`) must be the same vocabulary. Round 20 found
    /// the prompt advertising nine sensors while the executors resolved
    /// twelve: `battery` and `memory` were real capabilities the model was
    /// never told about, and `stress` was reachable only through the
    /// separate `check_stress_level()` alias.
    #[test]
    fn the_prompt_advertises_exactly_the_canonical_sensors() {
        let mut prompt = crate::agent_runner::advertised_sensors();
        let mut canonical = crate::tools::SENSOR_NAMES.to_vec();
        // Anti-vacuity: a parser that returned nothing, or a list that
        // emptied, would otherwise make both sides trivially equal.
        assert!(
            prompt.len() >= 12,
            "the prompt's sensor list parsed down to {prompt:?}"
        );
        assert!(canonical.len() >= 12, "the canonical list shrank");
        prompt.sort_unstable();
        canonical.sort_unstable();
        assert_eq!(
            prompt, canonical,
            "the prompt and the executor disagree on the sensor vocabulary"
        );
    }

    #[test]
    fn sim_flash_storage_read_write_erase() {
        let mut flash = SimFlashStorage::new();
        assert_eq!(flash.write_count(), 0);
        // Initial state is all 0xFF.
        assert_eq!(flash.read(0, 4).unwrap(), vec![0xFF, 0xFF, 0xFF, 0xFF]);
        // Write and read back.
        flash.write(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(flash.write_count(), 1);
        assert_eq!(flash.read(0, 4).unwrap(), vec![1, 2, 3, 4]);
        // Overlapping write.
        flash.write(2, &[9, 9]).unwrap();
        assert_eq!(flash.read(0, 4).unwrap(), vec![1, 2, 9, 9]);
        // Erase resets a sector to 0xFF.
        flash.erase_sector(0).unwrap();
        assert_eq!(flash.read(0, 4).unwrap(), vec![0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn sim_flash_storage_rejects_out_of_bounds() {
        let mut flash = SimFlashStorage::new();
        // Read past the end → StorageReadFailed.
        assert!(matches!(
            flash.read(65530, 10),
            Err(AgentError::StorageReadFailed { .. })
        ));
        // Write past the end → StorageWriteFailed.
        assert!(matches!(
            flash.write(65534, &[1, 2, 3]),
            Err(AgentError::StorageWriteFailed { .. })
        ));
        // Erasing a sector outside the region is a clean error, not the panic
        // the unchecked `65536 - start` used to produce. Sector 16 starts at
        // exactly the end; `u32::MAX` exercises the multiply overflow path.
        assert!(matches!(
            flash.erase_sector(16),
            Err(AgentError::StorageWriteFailed { .. })
        ));
        assert!(matches!(
            flash.erase_sector(u32::MAX),
            Err(AgentError::StorageWriteFailed { .. })
        ));
        // A valid erase still works after the rejections.
        assert!(flash.erase_sector(15).is_ok());
    }

    /// The ReAct loop hands the executor a JSON args payload. `extract_int_arg`
    /// only understood the legacy `pin=13` shape, so every JSON integer was
    /// dropped and the tool fell back to pin 0 — reporting success for a pin
    /// the model never asked for. `flash_read`/`flash_write` had the same
    /// defect on their `address`.
    #[test]
    fn write_gpio_reads_the_pin_from_a_json_payload() {
        let mut sim = AgentSimulator::new();
        let out = sim
            .execute_tool("write_gpio", r#"{"pin":13,"state":"high"}"#)
            .unwrap();
        assert!(out.contains("pin 13"), "got {out:?}");
        assert_eq!(sim.gpio.get_pin(13).unwrap(), GpioPinState::High);
        // The bug's pin, the one that used to be reported and switched
        // instead, must be untouched.
        assert_eq!(sim.gpio.get_pin(0).unwrap(), GpioPinState::Low);
    }

    /// `as u8` / `as u32` truncated out-of-range arguments: `pin=256` became
    /// pin 0 and passed the range check (success for the wrong pin), and a
    /// negative flash address wrapped to a huge `u32`.
    #[test]
    fn gpio_pin_and_flash_address_are_range_checked() {
        let mut sim = AgentSimulator::new();
        assert!(sim
            .execute_tool("write_gpio", r#"{"pin":256,"state":"high"}"#)
            .is_err());
        assert!(sim
            .execute_tool("write_gpio", r#"{"pin":-1,"state":"high"}"#)
            .is_err());
        assert!(sim.execute_tool("flash_read", r#"{"address":-1}"#).is_err());
        assert!(sim
            .execute_tool("flash_write", r#"{"address":-1,"data":"x"}"#)
            .is_err());
        assert_eq!(
            sim.gpio.get_pin(0).unwrap(),
            GpioPinState::Low,
            "256 must not be truncated to pin 0"
        );
    }

    #[test]
    fn flash_tools_read_the_address_from_a_json_payload() {
        // Same defect as the pin above, on the other two `extract_int_arg`
        // call sites: any address a model asked for read as 0x0000.
        let mut sim = AgentSimulator::new();
        let out = sim
            .execute_tool("flash_write", r#"{"address":1024,"data":"log"}"#)
            .unwrap();
        assert!(out.contains("0x0400"), "got {out:?}");
        let out = sim
            .execute_tool("flash_read", r#"{"address":1024}"#)
            .unwrap();
        assert!(out.contains("0x0400"), "got {out:?}");
    }

    #[test]
    fn extract_int_arg_accepts_json_and_key_value_payloads() {
        // What the loop sends: `ToolCall::arguments` serialised as JSON.
        assert_eq!(
            extract_int_arg(r#"{"pin":13,"state":"high"}"#, "pin"),
            Some(13)
        );
        // A number the model wrote as a string.
        assert_eq!(extract_int_arg(r#"{"pin":"7"}"#, "pin"), Some(7));
        // The legacy shape the scripted callers use.
        assert_eq!(extract_int_arg("pin=3,state=low", "pin"), Some(3));
        // Absent key, and payloads that are not object JSON.
        assert_eq!(extract_int_arg(r#"{"state":"high"}"#, "pin"), None);
        assert_eq!(extract_int_arg("state=high", "pin"), None);
        assert_eq!(extract_int_arg("", "pin"), None);
    }

    #[test]
    fn extract_str_arg_accepts_json_and_key_value_payloads() {
        assert_eq!(
            extract_str_arg(r#"{"pin":13,"state":"high"}"#, "state").as_deref(),
            Some("high")
        );
        // A bare number/bool the model wrote unquoted is still the caller's
        // value, not a reason to fall back to a default.
        assert_eq!(
            extract_str_arg(r#"{"state":1}"#, "state").as_deref(),
            Some("1")
        );
        assert_eq!(
            extract_str_arg(r#"{"data":"log"}"#, "data").as_deref(),
            Some("log")
        );
        // The legacy `key=value` shape, terminated by a separator.
        assert_eq!(
            extract_str_arg("pin=13,state=low", "state").as_deref(),
            Some("low")
        );
        assert_eq!(extract_str_arg(r#"{"pin":13}"#, "state"), None);
        assert_eq!(extract_str_arg("pin=13", "state"), None);
        assert_eq!(extract_str_arg("", "state"), None);
    }

    /// `write_gpio` decided the level with `args.contains("high")`, so a
    /// payload that *omitted* `state` — and one that said `"state":"1"` —
    /// both drove the pin **low**, the opposite of the embedded executor's
    /// `high` default, while any nonsense string was accepted silently.
    #[test]
    fn write_gpio_defaults_to_high_and_rejects_a_bogus_state() {
        let mut sim = AgentSimulator::new();
        // State absent → high, matching `tools.rs::execute_write_gpio`.
        sim.execute_tool("write_gpio", r#"{"pin":13}"#).unwrap();
        assert_eq!(sim.gpio.get_pin(13).unwrap(), GpioPinState::High);
        // The firmware's numeric spelling.
        sim.execute_tool("write_gpio", r#"{"pin":14,"state":"1"}"#)
            .unwrap();
        assert_eq!(sim.gpio.get_pin(14).unwrap(), GpioPinState::High);
        // A value that is neither spelling is an error, not a silent low.
        assert!(matches!(
            sim.execute_tool("write_gpio", r#"{"pin":15,"state":"mid"}"#),
            Err(AgentError::ConfigurationError { .. })
        ));
        // ...and the pin was not switched on the way to that error.
        assert_eq!(sim.gpio.get_pin(15).unwrap(), GpioPinState::Low);
    }

    /// A call that names no `pin` used to drive pin **0**: `unwrap_or(0)`
    /// turned a malformed call into a real side effect on the first pin. The
    /// same for a flash address — `flash_write` with no `address` wrote
    /// address 0. An absent argument is a missing field, not a request for
    /// zero.
    #[test]
    fn a_missing_pin_or_flash_address_is_an_error_not_a_zero() {
        let err = AgentSimulator::new()
            .execute_tool("write_gpio", r#"{"state":"high"}"#)
            .unwrap_err();
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
            let err = AgentSimulator::new()
                .execute_tool(tool, r#"{"data":"x"}"#)
                .unwrap_err();
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

    /// Pull the BPM out of a `read_heart_rate` reply, which reads
    /// `Heart Rate: NNN BPM, HRV: X ms`.
    fn bpm_of(reply: &str) -> u16 {
        reply
            .split("BPM")
            .next()
            .and_then(|s| s.split_whitespace().last())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// The four exercise tools the CLI's system prompt documents were answered
    /// by the unknown-tool arm, so a model following the prompt got "missing
    /// field: tool" for every one of them. They are now backed by
    /// `sports_coach`, which had shipped with no caller at all.
    #[test]
    fn the_exercise_tools_run_instead_of_reporting_an_unknown_tool() {
        let mut sim = AgentSimulator::new();
        // A valid lifecycle: neither call may fall through to the unknown-tool
        // arm, which under the old code answered all four with
        // `ConfigurationError { field: "tool" }`.
        for (tool, args) in [
            ("start_exercise", r#"{"type":"running"}"#),
            ("end_exercise", "{}"),
            ("get_exercise_goal", "{}"),
            ("adjust_exercise_goal", r#"{"intensity":0.7}"#),
        ] {
            let out = sim
                .execute_tool(tool, args)
                .unwrap_or_else(|err| panic!("{tool} is still unimplemented: {err:?}"));
            assert!(!out.is_empty(), "{tool} produced no output");
        }
    }

    /// Starting a session has to be visible to the rest of the simulator, not
    /// just recorded: `read_heart_rate` reads `is_exercising`, so this is the
    /// behaviour that proves the session is real rather than a printed string.
    #[test]
    fn a_started_session_raises_the_simulated_heart_rate() {
        let mut sim = AgentSimulator::new();
        let rest = sim
            .execute_tool("read_heart_rate", "{}")
            .expect("a resting read");
        let rest_bpm = bpm_of(&rest);
        assert!(rest_bpm < 100, "expected a resting rate, got {rest:?}");

        let started = sim
            .execute_tool("start_exercise", r#"{"type":"running"}"#)
            .expect("start_exercise must work");
        assert!(started.contains("Running"), "got {started:?}");

        let active = sim
            .execute_tool("read_heart_rate", "{}")
            .expect("an exercising read");
        let active_bpm = bpm_of(&active);
        assert!(
            active_bpm > rest_bpm,
            "the session did not reach the sensor: {rest:?} -> {active:?}"
        );
    }

    /// Ending a session returns the heart rate to rest and reports how long it
    /// ran. The elapsed time is measured from a monotonic clock, so a session
    /// that lasted no measurable time reports zero rather than an invented
    /// duration.
    #[test]
    fn ending_a_session_measures_it_and_clears_the_exercising_flag() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"yoga"}"#)
            .unwrap();
        let out = sim.execute_tool("end_exercise", "{}").unwrap();
        assert!(out.contains("Yoga"), "got {out:?}");
        assert!(
            out.contains("after 0s"),
            "elapsed must be measured: {out:?}"
        );
        assert!(!sim.health_state.is_exercising, "the flag must clear");

        let after = sim.execute_tool("read_heart_rate", "{}").unwrap();
        assert!(
            bpm_of(&after) < 100,
            "still exercising after end: {after:?}"
        );
    }

    /// Ending a session that was never started, and reading a goal nobody
    /// configured, are both "nothing to report" — not successes carrying a
    /// fabricated value. The default `ExerciseGoal` is a built-in, and
    /// reporting it as the user's goal is the difference between a default
    /// and a decision.
    #[test]
    fn an_absent_session_or_goal_is_an_error_not_a_fabricated_success() {
        let mut sim = AgentSimulator::new();
        for tool in ["end_exercise", "get_exercise_goal", "adjust_exercise_goal"] {
            let err = sim.execute_tool(tool, r#"{"intensity":0.5}"#).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        reason: crate::error::ConfigError::MissingField,
                        ..
                    }
                ),
                "{tool} without a session/goal must be a missing field, got {err:?}"
            );
        }
    }

    /// A type the build does not know, and a type the caller never wrote, are
    /// different errors — and neither may fall through to the default goal,
    /// which is how a misspelled activity becomes a real Running session.
    #[test]
    fn an_unknown_or_absent_exercise_type_is_rejected() {
        let mut sim = AgentSimulator::new();
        let absent = sim.execute_tool("start_exercise", "{}").unwrap_err();
        assert!(
            matches!(
                absent,
                AgentError::ConfigurationError {
                    field: "type",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {absent:?}"
        );
        let unknown = sim
            .execute_tool("start_exercise", r#"{"type":"parkour"}"#)
            .unwrap_err();
        assert!(
            matches!(
                unknown,
                AgentError::ConfigurationError {
                    field: "type",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {unknown:?}"
        );
        assert!(
            !sim.health_state.is_exercising,
            "a rejected start must not begin a session"
        );
    }

    /// `adjust_exercise_goal` must change what `get_exercise_goal` then
    /// reports — the two are one round trip, and a goal that is accepted but
    /// not stored would look identical from the adjust reply alone.
    #[test]
    fn an_adjusted_goal_is_what_get_exercise_goal_reports() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"cycling"}"#)
            .unwrap();
        let before = sim.execute_tool("get_exercise_goal", "{}").unwrap();
        assert!(before.contains("40 minutes"), "cycling default: {before:?}");

        let out = sim
            .execute_tool("adjust_exercise_goal", r#"{"duration":15,"intensity":0.5}"#)
            .unwrap();
        assert!(out.contains("40 minutes"), "the old value: {out:?}");
        assert!(out.contains("15 minutes"), "the new value: {out:?}");

        let after = sim.execute_tool("get_exercise_goal", "{}").unwrap();
        assert!(
            after.contains("15 minutes"),
            "the stored goal must be the adjusted one: {after:?}"
        );
        assert!(
            !after.contains("40 minutes"),
            "the pre-adjustment duration survived: {after:?}"
        );
    }

    /// An intensity is a *fraction of max HR*, so `70` (meaning 70 %) is a
    /// unit mistake. Clamping it silently would store an impossible goal and
    /// report success.
    #[test]
    fn an_out_of_range_intensity_is_rejected_rather_than_clamped() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"running"}"#)
            .unwrap();
        for bad in ["70", "1.5", "-0.2"] {
            let args = format!(r#"{{"intensity":{bad}}}"#);
            let err = sim.execute_tool("adjust_exercise_goal", &args).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        field: "intensity",
                        reason: crate::error::ConfigError::InvalidValue
                    }
                ),
                "intensity {bad} got {err:?}"
            );
        }
        // The rejected calls must not have moved the goal.
        let goal = sim.execute_tool("get_exercise_goal", "{}").unwrap();
        assert!(goal.contains("30 minutes"), "goal drifted: {goal:?}");
    }

    /// The fractional reader exists because an integer reader reports `0.7` as
    /// *absent*, which would turn "set intensity to 0.7" into a missing-field
    /// error.
    #[test]
    fn extract_float_arg_reads_both_payload_shapes() {
        assert_eq!(
            extract_float_arg(r#"{"intensity":0.7}"#, "intensity"),
            Some(0.7)
        );
        assert_eq!(
            extract_float_arg(r#"{"intensity":"0.55"}"#, "intensity"),
            Some(0.55)
        );
        assert_eq!(
            extract_float_arg("intensity=0.9,duration=20", "intensity"),
            Some(0.9)
        );
        assert_eq!(extract_float_arg(r#"{"duration":20}"#, "intensity"), None);
    }

    /// The four alert/notification tools the CLI's system prompt documents
    /// (`queue_voice`, `send_health_alert`, `send_meditation`, `set_dnd`) were
    /// answered by the unknown-tool arm, so a model following the prompt was
    /// told the tool does not exist. They are now backed by
    /// `voice_notification`, which had shipped with no caller at all.
    #[test]
    fn the_notification_tools_run_instead_of_reporting_an_unknown_tool() {
        let mut sim = AgentSimulator::new();
        for (tool, args) in [
            ("queue_voice", r#"{"text":"time to hydrate","priority":6}"#),
            (
                "send_health_alert",
                r#"{"type":"heart_rate","severity":9,"message":"HR too high"}"#,
            ),
            (
                "send_meditation",
                r#"{"script":"breathe in, hold, breathe out"}"#,
            ),
            (
                "set_dnd",
                r#"{"enabled":true,"start_hour":22,"end_hour":7}"#,
            ),
        ] {
            let out = sim
                .execute_tool(tool, args)
                .unwrap_or_else(|err| panic!("{tool} is still unimplemented: {err:?}"));
            assert!(!out.is_empty(), "{tool} produced no output");
        }
    }

    /// A queued cue has to reach the queue — the reply saying "queued" is not
    /// the behaviour, the queue depth is.
    #[test]
    fn a_queued_voice_message_reaches_the_queue() {
        let mut sim = AgentSimulator::new();
        assert_eq!(sim.voice.voice_queue_size(), 0);
        sim.execute_tool("queue_voice", r#"{"text":"first","priority":5}"#)
            .unwrap();
        assert_eq!(sim.voice.voice_queue_size(), 1);
        sim.execute_tool("queue_voice", r#"{"text":"second","priority":7}"#)
            .unwrap();
        assert_eq!(sim.voice.voice_queue_size(), 2);
    }

    /// `set_dnd` has to change what `queue_voice` then does, or it is a state
    /// change nothing observes. A fresh simulator is at uptime hour 0, which
    /// lies inside a 22:00–07:00 window, so a low-priority cue is routed away
    /// from the queue while an urgent one still gets through.
    #[test]
    fn do_not_disturb_suppresses_a_low_priority_cue_but_not_an_urgent_one() {
        let mut sim = AgentSimulator::new();
        assert!(!sim.voice.is_in_dnd_period(0), "DND starts off");

        let out = sim
            .execute_tool(
                "set_dnd",
                r#"{"enabled":true,"start_hour":22,"end_hour":7}"#,
            )
            .unwrap();
        assert!(out.contains("active"), "hour 0 is inside 22–7: {out:?}");
        assert!(sim.voice.is_in_dnd_period(0));

        sim.execute_tool("queue_voice", r#"{"text":"low","priority":5}"#)
            .unwrap();
        assert_eq!(
            sim.voice.voice_queue_size(),
            0,
            "a low-priority cue must not be spoken during DND"
        );

        sim.execute_tool("queue_voice", r#"{"text":"urgent","priority":9}"#)
            .unwrap();
        assert_eq!(
            sim.voice.voice_queue_size(),
            1,
            "priority 9 is urgent enough to bypass DND"
        );
    }

    /// A health alert is a multi-channel event, and do-not-disturb can drop
    /// either channel. The reply has to say which happened: reporting
    /// "delivered" for an alert the pipeline discarded is the fabricated-success
    /// bug in a new place.
    #[test]
    fn an_alert_is_not_reported_as_delivered_when_dnd_suppresses_it() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool(
            "set_dnd",
            r#"{"enabled":true,"start_hour":22,"end_hour":7}"#,
        )
        .unwrap();

        // Severity 6 is a warning, not an emergency: both channels are dropped.
        let quiet = sim
            .execute_tool(
                "send_health_alert",
                r#"{"type":"glucose","severity":6,"message":"glucose 210 mg/dL"}"#,
            )
            .unwrap();
        assert!(quiet.contains("not queued"), "{quiet:?}");
        assert!(quiet.contains("not raised"), "{quiet:?}");
        assert_eq!(sim.voice.voice_queue_size(), 0);
        assert_eq!(sim.voice.notification_history().len(), 0);

        // Severity 9 is urgent enough to bypass do-not-disturb on both channels.
        let urgent = sim
            .execute_tool(
                "send_health_alert",
                r#"{"type":"arrhythmia","severity":9,"message":"irregular rhythm"}"#,
            )
            .unwrap();
        assert!(urgent.contains("queued"), "{urgent:?}");
        assert!(urgent.contains("raised"), "{urgent:?}");
    }

    /// A health alert is a multi-channel event (voice + notification), so
    /// proving it ran means proving the notification pipeline saw it — not just
    /// that the call returned `Ok`.
    #[test]
    fn a_health_alert_reaches_the_notification_pipeline() {
        let mut sim = AgentSimulator::new();
        assert_eq!(sim.voice.notification_history().len(), 0);
        sim.execute_tool(
            "send_health_alert",
            r#"{"type":"glucose","severity":6,"message":"glucose 210 mg/dL"}"#,
        )
        .unwrap();
        assert!(
            !sim.voice.notification_history().is_empty(),
            "the alert produced no notification"
        );
        assert_eq!(sim.voice.voice_queue_size(), 1, "severity 6 also speaks");
    }

    /// Absent arguments are missing fields, not defaults. A `queue_voice` with
    /// no text, a `send_meditation` with no script and a `set_dnd` with no
    /// window must all fail rather than queue or configure something invented.
    #[test]
    fn an_absent_notification_argument_is_an_error_not_a_default() {
        let mut sim = AgentSimulator::new();
        for (tool, args) in [
            ("queue_voice", r#"{"priority":5}"#),
            ("send_meditation", "{}"),
            ("send_health_alert", r#"{"severity":5,"message":"x"}"#),
            ("set_dnd", r#"{"start_hour":22,"end_hour":7}"#),
        ] {
            let err = sim.execute_tool(tool, args).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        reason: crate::error::ConfigError::MissingField,
                        ..
                    }
                ),
                "{tool} with an absent argument got {err:?}"
            );
        }
    }

    /// Severity and priority are scales, not free integers. Clamping `70` into
    /// range would store a value the caller never asked for and report success.
    #[test]
    fn an_out_of_range_severity_or_priority_is_rejected_rather_than_clamped() {
        let mut sim = AgentSimulator::new();
        for (tool, args, field) in [
            (
                "send_health_alert",
                r#"{"type":"hr","severity":70,"message":"x"}"#,
                "severity",
            ),
            ("queue_voice", r#"{"text":"x","priority":0}"#, "priority"),
            ("queue_voice", r#"{"text":"x","priority":99}"#, "priority"),
        ] {
            let err = sim.execute_tool(tool, args).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        reason: crate::error::ConfigError::InvalidValue,
                        ..
                    }
                ),
                "{tool} with {args} expected invalid {field}, got {err:?}"
            );
        }
        assert_eq!(sim.voice.voice_queue_size(), 0);
        assert_eq!(sim.voice.notification_history().len(), 0);
    }

    /// An hour is 0..=23. Wrapping 24 to 0 would silently invert the window, so
    /// it is rejected — and the DND state is left as it was.
    #[test]
    fn a_dnd_hour_outside_the_clock_is_rejected() {
        let mut sim = AgentSimulator::new();
        for args in [
            r#"{"enabled":true,"start_hour":24,"end_hour":7}"#,
            r#"{"enabled":true,"start_hour":22,"end_hour":24}"#,
            r#"{"enabled":true,"start_hour":-1,"end_hour":7}"#,
        ] {
            let err = sim.execute_tool("set_dnd", args).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        reason: crate::error::ConfigError::InvalidValue,
                        ..
                    }
                ),
                "got {err:?} for {args}"
            );
        }
        assert!(
            !sim.voice.is_in_dnd_period(0),
            "a rejected set_dnd must not enable DND"
        );
    }

    /// The boolean reader exists because `enabled` arrives as a JSON bool from
    /// a model, a quoted string from some clients, or `1` from the legacy form.
    #[test]
    fn extract_bool_arg_reads_json_strings_and_ints() {
        assert_eq!(
            extract_bool_arg(r#"{"enabled":true}"#, "enabled"),
            Some(true)
        );
        assert_eq!(
            extract_bool_arg(r#"{"enabled":"false"}"#, "enabled"),
            Some(false)
        );
        assert_eq!(extract_bool_arg(r#"{"enabled":1}"#, "enabled"), Some(true));
        assert_eq!(extract_bool_arg("enabled=off", "enabled"), Some(false));
        assert_eq!(extract_bool_arg(r#"{"enabled":"maybe"}"#, "enabled"), None);
        assert_eq!(extract_bool_arg(r#"{"other":true}"#, "enabled"), None);
    }

    /// `get_sleep_quality` was advertised in the system prompt and answered by
    /// the unknown-tool arm. It is now backed by `sleep_manager`, which had
    /// shipped with no caller.
    #[test]
    fn the_sleep_tool_runs_instead_of_reporting_an_unknown_tool() {
        let mut sim = AgentSimulator::new();
        sim.simulate_night(450);
        let out = sim
            .execute_tool("get_sleep_quality", "{}")
            .unwrap_or_else(|err| panic!("get_sleep_quality is unimplemented: {err:?}"));
        assert!(out.contains("Last night"), "got {out:?}");
    }

    /// A device that has not recorded a night has nothing to report. Returning
    /// an overall score anyway would be a fabricated health claim — the one
    /// thing a sleep metric must never do.
    #[test]
    fn sleep_quality_without_a_recorded_night_is_an_error_not_a_score() {
        let mut sim = AgentSimulator::new();
        let err = sim.execute_tool("get_sleep_quality", "{}").unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "sleep",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
    }

    /// The score in the reply is the score the module computed from the record,
    /// not a number formatted into the string by hand — and it has to agree
    /// with the rating band the module assigns it.
    #[test]
    fn a_night_is_scored_by_the_sleep_module_not_by_the_reply() {
        let mut sim = AgentSimulator::new();
        sim.simulate_night(450);
        let record = sim.sleep.latest_sleep().expect("the night was recorded");
        let score = record.quality.score;
        let rating = record.quality.rating();
        assert!(score <= 100, "score out of range: {score}");

        let out = sim.execute_tool("get_sleep_quality", "{}").unwrap();
        assert!(
            out.contains(&format!("score {score}/100 ({rating})")),
            "reply must carry the module's score {score} ({rating}): {out:?}"
        );
    }

    /// A night appended via `simulate_night` has to round-trip through
    /// `SleepRecord::duration_minutes` — bedtime 23:00 plus the duration
    /// crosses midnight, and a record whose two duration notions disagree would
    /// report a night that never happened.
    #[test]
    fn a_simulated_night_records_a_duration_that_round_trips() {
        let mut sim = AgentSimulator::new();
        for minutes in [420u32, 450, 510] {
            sim.simulate_night(minutes);
            let record = sim.sleep.latest_sleep().unwrap();
            assert_eq!(
                record.duration_minutes(),
                minutes,
                "day {} lost its duration",
                record.day_index
            );
            assert_eq!(record.quality.duration_min, minutes);
        }
        assert_eq!(sim.sleep.sleep_history().len(), 3);
    }

    /// The history is the reason to keep one: the reply must carry the trend,
    /// and the debt it quotes must be the module's calculation over those
    /// nights, not a constant.
    #[test]
    fn sleep_quality_reports_the_trend_across_nights() {
        let mut sim = AgentSimulator::new();
        for _ in 0..3 {
            sim.simulate_night(360); // 6 h a night, 3 nights
        }
        let out = sim.execute_tool("get_sleep_quality", "{}").unwrap();
        assert!(
            out.contains("last 3 nights avg"),
            "no trend reported: {out:?}"
        );

        // 3 recorded nights of 6 h against an 8 h target: 3 × 2 h = 6 h short.
        // The window must be capped at the three nights that exist — asking for
        // seven must not turn the four missing ones into four nights of no
        // sleep (which is what it used to do: 56 h - 18 h = 38 h).
        let debt = sim.sleep.calculate_sleep_debt(7);
        assert!((debt - 6.0).abs() < 0.01, "unexpected debt {debt}");
        assert!(
            out.contains(&format!("sleep debt {debt:.1} h")),
            "the reply must quote the module's debt {debt}: {out:?}"
        );
    }

    /// The prompt is a contract: every name it advertises, the model may call.
    /// Rounds 14-18 each fixed one advertised name that this executor could not
    /// route; nothing stopped the next one. This walks the live prompt's tool
    /// list and fails on any name that falls through to the unknown-tool arm.
    ///
    /// The error, not the `Ok`/`Err` split, is what is checked: most of these
    /// tools legitimately fail a bare `{}` call (`get_sleep_quality` with no
    /// recorded night, `start_exercise` with no type, `web_search` with no
    /// query). What must never happen is `MissingField { field: "tool" }` —
    /// the executor not knowing the name at all.
    #[test]
    fn every_tool_the_prompt_advertises_is_routable() {
        let tools = crate::agent_runner::advertised_tools();
        let advertised: Vec<&str> = tools.iter().map(|t| t.name).collect();
        // The parser also feeds the CLI's `/tools` listing, so the signature
        // and description have to come out of the line intact, not just the
        // name.
        let sleep = tools
            .iter()
            .find(|t| t.name == "get_sleep_quality")
            .expect("get_sleep_quality is advertised");
        assert_eq!(sleep.signature, "get_sleep_quality()");
        assert!(
            sleep.description.starts_with("Get last night"),
            "description mis-parsed: {:?}",
            sleep.description
        );
        // Guard against the guard: a parser that silently matched nothing
        // would make the loop below pass while checking nothing at all.
        assert!(
            advertised.len() >= 24,
            "the prompt's tool list parsed down to {} names — the parser lost the list: {advertised:?}",
            advertised.len()
        );
        for expected in [
            "read_sensor",
            "get_sleep_quality",
            "notify_emergency_contact",
            "find_nearest_hospital",
            "get_weather",
        ] {
            assert!(
                advertised.contains(&expected),
                "parser missed {expected}; got {advertised:?}"
            );
        }

        let mut sim = AgentSimulator::new();
        let unrouted: Vec<&str> = advertised
            .iter()
            .copied()
            .filter(|name| {
                matches!(
                    sim.execute_tool(name, "{}"),
                    Err(AgentError::ConfigurationError {
                        field: "tool",
                        reason: crate::error::ConfigError::MissingField
                    })
                )
            })
            .collect();
        assert!(
            unrouted.is_empty(),
            "the prompt advertises tools this executor cannot route: {unrouted:?}"
        );
    }

    /// `notify_emergency_contact` and `find_nearest_hospital` answered the
    /// unknown-tool arm in every executor — they were advertised in the
    /// prompt's EMERGENCY section and implemented nowhere.
    #[test]
    fn the_emergency_tools_are_not_the_unknown_tool_arm() {
        let mut sim = AgentSimulator::new();
        assert!(sim
            .execute_tool("notify_emergency_contact", r#"{"name":"Alex (primary)"}"#)
            .is_ok());
        assert!(sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"er"}"#)
            .is_ok());
    }

    /// An unnamed request resolves to the registry's primary contact — the
    /// priority ordering exists for exactly that — and the notification really
    /// lands in the module's awaiting-delivery slot, rather than the reply
    /// merely saying it did.
    #[test]
    fn notifying_without_a_name_uses_the_primary_contact_and_queues_it() {
        let mut sim = AgentSimulator::new();
        let primary = sim.early_warning.emergency_contacts()[0].clone();
        assert!(sim.early_warning.take_emergency_notification().is_none());

        let out = sim
            .execute_tool(
                "notify_emergency_contact",
                r#"{"message": "Call me back."}"#,
            )
            .unwrap();
        assert!(out.contains("queued"), "not reported as queued: {out:?}");
        assert!(
            out.contains(primary.phone.as_str()),
            "wrong recipient: {out:?}"
        );

        let queued = sim
            .early_warning
            .pending_emergency_notification()
            .expect("the notification must be awaiting delivery");
        assert_eq!(queued.contact_phone, primary.phone);
        assert_eq!(queued.message.as_str(), "Call me back.");
        // 0 means "not raised by an alert crossing a threshold" — reusing the
        // alert counter here would invent an alert that never happened.
        assert_eq!(queued.alert_id, 0);
    }

    /// No registry and a name that is not in it are different mistakes: one is
    /// "nothing is configured", the other is the caller's typo. Reporting the
    /// wrong one sends the model looking for a field that is not missing.
    #[test]
    fn an_empty_registry_and_an_unknown_name_are_different_errors() {
        let mut sim = AgentSimulator::new();
        sim.early_warning = EarlyWarningSystem::new();
        let err = sim
            .execute_tool("notify_emergency_contact", r#"{"name":"Alex"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "contact",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );

        let mut sim = AgentSimulator::new();
        let err = sim
            .execute_tool("notify_emergency_contact", r#"{"name":"Nobody"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "name",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "a registered registry with an unknown name must say so: {err:?}"
        );
    }

    /// The hospital list is ranked by distance, so the generic query must
    /// return the *nearest* ER — and a specialty query must return the nearest
    /// one that actually has that specialty, even when it is farther away.
    #[test]
    fn hospital_lookup_ranks_by_distance_and_honours_the_specialty() {
        let mut sim = AgentSimulator::new();
        let nearest = sim.early_warning.nearest_er().unwrap().name.to_string();
        let cardio = sim
            .early_warning
            .nearest_with_specialty(true, false)
            .unwrap()
            .name
            .to_string();
        assert_ne!(
            cardio, nearest,
            "the fixture must exercise a farther specialty hospital"
        );

        let out = sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"er"}"#)
            .unwrap();
        assert!(
            out.contains(nearest.as_str()),
            "not the nearest ER: {out:?}"
        );

        let out = sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"cardiology"}"#)
            .unwrap();
        assert!(
            out.contains(cardio.as_str()),
            "cardiology must resolve to the cardiology hospital: {out:?}"
        );
    }

    /// An unknown specialty is the caller's error. A *known* specialty that no
    /// registered hospital has must not be answered with a different hospital:
    /// sending someone to the wrong department is worse than saying nothing.
    #[test]
    fn an_unmatched_specialty_is_reported_rather_than_substituted() {
        let mut sim = AgentSimulator::new();
        let err = sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"dermatology"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "specialty",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );

        // A registry that holds only ER hospitals has no cardiology answer.
        let mut sim = AgentSimulator::new();
        sim.early_warning = EarlyWarningSystem::new();
        let mut h = Hospital::new("Plain Clinic", "1 Road", "+1-555-0300");
        h.distance_m = Some(900);
        h.has_cardiology = false;
        sim.early_warning.add_hospital(h).unwrap();
        let err = sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"cardiology"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "hospital",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "a known specialty with no match is a missing hospital, got {err:?}"
        );
    }

    /// A night is rest. Simulating one while the exercise state says "active"
    /// must still record a resting heart rate, and must not clear the flag the
    /// exercise tools own.
    #[test]
    fn a_simulated_night_is_rest_and_leaves_the_exercise_flag_alone() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"running"}"#)
            .unwrap();
        sim.simulate_night(450);
        let record = sim.sleep.latest_sleep().unwrap();
        assert!(
            record.sleep_hr < 100,
            "a night recorded an exercising HR: {}",
            record.sleep_hr
        );
        assert!(
            sim.health_state.is_exercising,
            "simulating a night must not end the session"
        );
    }

    /// `flash_write` discarded the model's `data` and wrote the literal
    /// `data at <address>`, so a write-then-read handed the caller back text
    /// it never wrote.
    #[test]
    fn flash_write_stores_the_payload_the_model_sent() {
        let mut sim = AgentSimulator::new();
        let out = sim
            .execute_tool("flash_write", r#"{"address":1024,"data":"log"}"#)
            .unwrap();
        assert!(out.contains("3 bytes"), "got {out:?}");
        let out = sim
            .execute_tool("flash_read", r#"{"address":1024}"#)
            .unwrap();
        // "log" in hex; the fabricated string would start with 64 61 74 61.
        assert!(out.contains("6C6F67"), "got {out:?}");
    }

    /// `ble_send` forwarded the raw args envelope, so
    /// `{"data":"System health OK"}` went over the air as 27 bytes of JSON
    /// — braces, quotes and key name included — for a 16-character message.
    #[test]
    fn ble_send_transmits_the_message_not_the_args_envelope() {
        let mut sim = AgentSimulator::new();
        sim.ble.connect().unwrap();
        let msg = "System health OK";
        let out = sim
            .execute_tool("ble_send", r#"{"data":"System health OK"}"#)
            .unwrap();
        assert!(out.contains(&format!("{} bytes", msg.len())), "got {out:?}");
        assert_eq!(sim.ble.last_message(), Some(msg));
    }

    #[test]
    fn sim_gpio_controller_set_get_toggle() {
        let mut gpio = SimGpioController::new();
        assert_eq!(gpio.get_pin(0).unwrap(), GpioPinState::Low);
        gpio.set_pin(5, GpioPinState::High).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::High);
        gpio.toggle_pin(5).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::Low);
        gpio.toggle_pin(5).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::High);
        // Out-of-range pins are rejected for read and write.
        assert!(matches!(
            gpio.get_pin(32),
            Err(AgentError::GpioOperationFailed { .. })
        ));
        assert!(matches!(
            gpio.set_pin(32, GpioPinState::High),
            Err(AgentError::GpioOperationFailed { .. })
        ));
    }

    #[test]
    fn sim_ble_interface_connect_send() {
        let mut ble = SimBleInterface::new();
        assert!(!ble.is_connected());
        // Send before connect is refused.
        assert!(matches!(
            ble.send("hi"),
            Err(AgentError::NetworkConnectionFailed { .. })
        ));
        ble.connect().unwrap();
        assert!(ble.is_connected());
        ble.send("hello").unwrap();
        ble.send("world").unwrap();
        assert_eq!(ble.message_count(), 2);
        assert_eq!(ble.last_message(), Some("world"));
        ble.disconnect();
        assert!(!ble.is_connected());
        // Send after disconnect is refused again.
        assert!(ble.send("late").is_err());
    }

    #[test]
    fn sim_sensor_manager_reads_realistic_values() {
        let mut s = SimSensorManager::new();
        // Temperature near base (23.5) with small bounded variation.
        let t = s.read_temperature();
        assert!((18.0..=29.0).contains(&t), "temp {t}");
        // Humidity near base (55).
        let h = s.read_humidity();
        assert!((45.0..=65.0).contains(&h), "humidity {h}");
        // Pressure near base (1013).
        let p = s.read_pressure();
        assert!((1000.0..=1020.0).contains(&p), "pressure {p}");
        // Accelerometer: z ≈ 9.8 with small noise, x/y ≈ 0.
        let (x, y, z) = s.read_accelerometer();
        assert!((z - 9.8).abs() < 0.1, "z {z}");
        assert!(x.abs() < 0.1);
        assert!(y.abs() < 0.1);
        // Light within its cycle range.
        let l = s.read_light();
        assert!((10.0..=1000.0).contains(&l), "light {l}");
        // Readings vary across iterations rather than being constant.
        let t2 = s.read_temperature();
        assert!(t != t2, "temperature should vary between reads");
    }

    /// Every simulated component offers `Default` as an alias for `new()` so
    /// callers that build them generically (e.g. `Default::default()` in a
    /// struct field) get a working, empty instance rather than a panic.
    #[test]
    fn simulator_components_default_matches_new() {
        assert_eq!(SimFlashStorage::default().write_count(), 0);
        assert_eq!(SimGpioController::default().get_pin(0).unwrap(), GpioPinState::Low);
        assert!(!SimBleInterface::default().is_connected());
        // A fresh sensor manager reads without panicking.
        let _ = SimSensorManager::default().read_temperature();
        let _ = HealthSimState::default();
        // The whole simulator defaults to the same working state.
        let mut sim = AgentSimulator::default();
        assert!(sim.execute_tool("read_sensor", r#"{"sensor":"temperature"}"#).is_ok());
    }

    #[test]
    fn get_status_reports_the_accumulated_counters() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("flash_write", r#"{"address":0,"data":"hi"}"#)
            .unwrap();
        let status = sim.get_status();
        assert!(status.starts_with("Simulator Status:"), "got {status:?}");
        assert!(status.contains("Flash writes: 1"), "got {status:?}");
    }

    /// The glucose trend is a function of time since the (simulated) meal.
    /// Driving the elapsed hours through each band proves the report tracks
    /// the state rather than emitting a constant.
    #[test]
    fn the_glucose_trend_follows_hours_since_the_meal() {
        // < 2.0 h → Rising, 2.0..4.0 → Stable, >= 4.0 → Falling.
        // `simulate_glucose` advances the clock by 0.5 per read, so start
        // each probe 0.5 below its band boundary.
        let mut sim = AgentSimulator::new();
        sim.set_hours_since_meal(1.0);
        let rising = sim.execute_tool("read_sensor", r#"{"sensor":"glucose"}"#).unwrap();
        assert!(rising.contains("Rising"), "got {rising:?}");

        let mut sim = AgentSimulator::new();
        sim.set_hours_since_meal(1.9);
        let stable = sim.execute_tool("read_sensor", r#"{"sensor":"glucose"}"#).unwrap();
        assert!(stable.contains("Stable"), "got {stable:?}");

        let mut sim = AgentSimulator::new();
        sim.set_hours_since_meal(3.9);
        let falling = sim.execute_tool("read_sensor", r#"{"sensor":"glucose"}"#).unwrap();
        assert!(falling.contains("Falling"), "got {falling:?}");
    }

    /// ECG rhythm flips to "Irregular" only above a stress threshold, and the
    /// HRV sensor reports the state the manager is holding.
    #[test]
    fn ecg_and_hrv_readings_reflect_the_stress_state() {
        let mut sim = AgentSimulator::new();
        sim.set_stress_level(20);
        let normal = sim.execute_tool("read_sensor", r#"{"sensor":"ecg"}"#).unwrap();
        assert!(normal.contains("Rhythm=Normal"), "got {normal:?}");

        let mut sim = AgentSimulator::new();
        sim.set_stress_level(90);
        let irregular = sim.execute_tool("read_sensor", r#"{"sensor":"ecg"}"#).unwrap();
        assert!(irregular.contains("Rhythm=Irregular"), "got {irregular:?}");

        let hrv = sim.execute_tool("read_sensor", r#"{"sensor":"hrv"}"#).unwrap();
        assert!(hrv.contains("HRV:"), "got {hrv:?}");
        assert!(hrv.contains("Stress Level: 90"), "got {hrv:?}");
    }

    /// `adjust_exercise_goal` requires at least one of intensity/duration.
    /// An empty adjustment is a caller mistake, not a silent no-op.
    #[test]
    fn adjust_exercise_goal_without_a_field_is_a_missing_field_error() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"running"}"#)
            .unwrap();
        let err = sim.execute_tool("adjust_exercise_goal", "{}").unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "intensity",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
    }

    /// A duration of zero (or less) would store a goal with no time budget;
    /// reject it rather than clamp to a meaningless minimum.
    #[test]
    fn a_non_positive_duration_is_rejected_rather_than_clamped() {
        let mut sim = AgentSimulator::new();
        sim.execute_tool("start_exercise", r#"{"type":"cycling"}"#)
            .unwrap();
        let err = sim
            .execute_tool("adjust_exercise_goal", r#"{"duration":0}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "duration",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );
    }

    /// The emergency path: a name that matches queues that contact; a name
    /// that does not match is the caller's error; an absent name falls back to
    /// the primary; a missing message still delivers a truthful one; and an
    /// empty registry has nobody to reach.
    #[test]
    fn notify_emergency_contact_covers_named_absent_unknown_and_empty() {
        // A named contact that exists.
        let mut sim = AgentSimulator::new();
        let out = sim
            .execute_tool(
                "notify_emergency_contact",
                r#"{"name":"Sam (backup)","message":"help"}"#,
            )
            .unwrap();
        assert!(out.contains("Sam (backup)"), "got {out:?}");
        assert!(out.contains("awaiting delivery"), "got {out:?}");

        // Absent name → the primary (first SMS-capable) contact.
        let mut sim = AgentSimulator::new();
        let out = sim.execute_tool("notify_emergency_contact", "{}").unwrap();
        assert!(out.contains("Alex (primary)"), "got {out:?}");
        // No message → the truthful default rather than an invented one.
        assert!(out.contains("needs help"), "got {out:?}");

        // A name not in the registry is an invalid value, not a missing field.
        let mut sim = AgentSimulator::new();
        let err = sim
            .execute_tool("notify_emergency_contact", r#"{"name":"Nobody"}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "name",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );

        // No contacts configured at all → nothing to fall back to.
        let mut sim = AgentSimulator::new();
        sim.early_warning = EarlyWarningSystem::new();
        let err = sim
            .execute_tool("notify_emergency_contact", "{}")
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "contact",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
    }

    /// `send_health_alert` names three required arguments; each absence must be
    /// its own missing-field error, and `type` is checked first.
    #[test]
    fn send_health_alert_reports_each_missing_argument() {
        let mut sim = AgentSimulator::new();
        for (args, field) in [
            (r#"{"severity":5,"message":"x"}"#, "type"),
            (r#"{"type":"hr","message":"x"}"#, "severity"),
            (r#"{"type":"hr","severity":5}"#, "message"),
        ] {
            let err = sim.execute_tool("send_health_alert", args).unwrap_err();
            assert!(
                matches!(
                    err,
                    AgentError::ConfigurationError {
                        reason: crate::error::ConfigError::MissingField,
                        ..
                    }
                ),
                "{args} expected missing {field}, got {err:?}"
            );
        }
    }

    /// The endocrine specialty resolves to the endocrinology hospital, and an
    /// empty hospital registry is a missing hospital rather than a lookup miss.
    #[test]
    fn hospital_lookup_resolves_endocrinology_and_rejects_an_empty_registry() {
        let mut sim = AgentSimulator::new();
        let out = sim
            .execute_tool("find_nearest_hospital", r#"{"specialty":"endocrine"}"#)
            .unwrap();
        assert!(out.contains("Northside Endocrine Center"), "got {out:?}");

        let mut sim = AgentSimulator::new();
        sim.early_warning = EarlyWarningSystem::new();
        let err = sim
            .execute_tool("find_nearest_hospital", "{}")
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "hospital",
                    reason: crate::error::ConfigError::MissingField
                }
            ),
            "got {err:?}"
        );
    }

    /// `enabled` present but not a boolean is a distinct mistake from an absent
    /// one: `{"enabled":"maybe"}` must be an invalid value, not "missing field".
    #[test]
    fn set_dnd_with_a_non_boolean_enabled_is_an_invalid_value() {
        let mut sim = AgentSimulator::new();
        let err = sim
            .execute_tool("set_dnd", r#"{"enabled":"maybe","start_hour":22,"end_hour":7}"#)
            .unwrap_err();
        assert!(
            matches!(
                err,
                AgentError::ConfigurationError {
                    field: "enabled",
                    reason: crate::error::ConfigError::InvalidValue
                }
            ),
            "got {err:?}"
        );
    }
}
