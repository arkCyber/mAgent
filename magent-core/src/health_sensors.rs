//! Health Sensors Module for mAgent
//!
//! Provides health-related sensor interfaces for:
//! - Heart rate monitoring (HR/HRV)
//! - Blood glucose monitoring (CGM)
//! - Electrocardiogram (ECG/EKG)
//! - Skin temperature for stress detection
//!
//! These sensors are typically connected via BLE or I2C to external
//! devices like chest straps, smartwatches, or CGM devices.

use crate::error::{try_heapless, AgentError, Result};
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum history entries for trend analysis
pub const MAX_HEALTH_HISTORY: usize = 144; // 24 hours at 10-minute intervals

/// Heart rate zones for exercise guidance.
///
/// Ordered from rest to danger so callers can test "this band or higher"
/// (`zone() >= Cardio`) rather than an equality against one band — the
/// equality form silently excluded Peak and Danger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HeartRateZone {
    /// Below 50 % of max HR — sitting / standing still.
    Rest = 0,
    /// 50–60 % of max HR — gentle movement.
    WarmUp = 1,
    /// 60–70 % of max HR — primary fat-oxidation band.
    FatBurn = 2,
    /// 70–80 % of max HR — aerobic capacity.
    Cardio = 3,
    /// 80–90 % of max HR — anaerobic threshold work.
    Peak = 4,
    /// ≥ 90 % of max HR — unsafe to sustain.
    Danger = 5,
}

impl HeartRateZone {
    /// Get zone name
    pub fn name(&self) -> &'static str {
        match self {
            HeartRateZone::Rest => "Rest",
            HeartRateZone::WarmUp => "Warm-up",
            HeartRateZone::FatBurn => "Fat Burn",
            HeartRateZone::Cardio => "Cardio",
            HeartRateZone::Peak => "Peak",
            HeartRateZone::Danger => "Danger",
        }
    }

    /// Calculate zone from heart rate and age
    pub fn from_hr_and_age(hr: u16, age: u8) -> Self {
        // Saturating subtraction: when age > 220 the max heart rate
        // formula is undefined — clamp to 0 so the percentage is
        // always 100 % of 0 → Danger zone (safest default).
        Self::from_hr_and_max(hr, 220u16.saturating_sub(age as u16))
    }

    /// Calculate the zone from a heart rate and a maximum heart rate.
    ///
    /// The single place the band boundaries live; [`Self::from_hr_and_age`]
    /// and [`HeartRateData::zone_for_profile`] both route through it, so a
    /// profile's measured `max_hr` and the age formula cannot drift apart.
    ///
    /// A non-zero `max_hr` is required: see [`Self::from_hr_and_age`].
    pub fn from_hr_and_max(hr: u16, max_hr: u16) -> Self {
        if max_hr == 0 {
            return HeartRateZone::Danger;
        }
        let percentage = (hr as f32 / max_hr as f32) * 100.0;

        if percentage < 50.0 {
            HeartRateZone::Rest
        } else if percentage < 60.0 {
            HeartRateZone::WarmUp
        } else if percentage < 70.0 {
            HeartRateZone::FatBurn
        } else if percentage < 80.0 {
            HeartRateZone::Cardio
        } else if percentage < 90.0 {
            HeartRateZone::Peak
        } else {
            HeartRateZone::Danger
        }
    }
}

/// Stress level derived from HRV.
///
/// The variants are ordered low → high so `current_stress` can take the
/// worse of the HRV and heart-rate readings with a plain `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StressLevel {
    /// Relaxed state; HRV ≥ 80 ms.
    Low = 0,
    /// Mild stress; HRV in 50–80 ms.
    Moderate = 1,
    /// Elevated stress; HRV in 25–50 ms.
    High = 2,
    /// Acute stress or recovery deficit; HRV < 25 ms.
    VeryHigh = 3,
}

impl StressLevel {
    /// Get stress level from HRV value (ms)
    pub fn from_hrv(hrv_ms: f32) -> Self {
        if hrv_ms >= 80.0 {
            StressLevel::Low
        } else if hrv_ms >= 50.0 {
            StressLevel::Moderate
        } else if hrv_ms >= 25.0 {
            StressLevel::High
        } else {
            StressLevel::VeryHigh
        }
    }

    /// Get description
    pub fn description(&self) -> &'static str {
        match self {
            StressLevel::Low => "Relaxed",
            StressLevel::Moderate => "Normal",
            StressLevel::High => "Stressed",
            StressLevel::VeryHigh => "Very Stressed",
        }
    }
}

/// Blood glucose level status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlucoseStatus {
    /// < 70 mg/dL — hypoglycaemia, treat immediately.
    Low,
    /// 70–100 mg/dL (fasting) — normal range.
    Normal,
    /// 100–125 mg/dL (fasting) — prediabetes range.
    Elevated,
    /// ≥ 126 mg/dL (fasting) — diabetes range.
    High,
}

impl GlucoseStatus {
    /// Get status from glucose value (mg/dL)
    pub fn from_glucose(glucose: f32) -> Self {
        if glucose < 70.0 {
            GlucoseStatus::Low
        } else if glucose < 100.0 {
            GlucoseStatus::Normal
        } else if glucose < 126.0 {
            GlucoseStatus::Elevated
        } else {
            GlucoseStatus::High
        }
    }

    /// Get description
    pub fn description(&self) -> &'static str {
        match self {
            GlucoseStatus::Low => "Low (Hypoglycemia Risk)",
            GlucoseStatus::Normal => "Normal",
            GlucoseStatus::Elevated => "Elevated (Prediabetes Range)",
            GlucoseStatus::High => "High (Diabetes Range)",
        }
    }
}

/// ECG heart rhythm status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeartRhythm {
    /// Sinus rhythm within the expected rate band.
    Normal,
    /// Rate consistently below the low-rate threshold.
    Bradycardia,
    /// Rate consistently above the high-rate threshold.
    Tachycardia,
    /// R-R variability exceeds the irregularity threshold.
    Irregular,
    /// Recognised rhythm but not in the explicit list above (e.g.
    /// paced rhythm, ectopic beats).
    Other,
}

impl HeartRhythm {
    /// Get description
    pub fn description(&self) -> &'static str {
        match self {
            HeartRhythm::Normal => "Normal Sinus Rhythm",
            HeartRhythm::Bradycardia => "Bradycardia (HR < 60 BPM)",
            HeartRhythm::Tachycardia => "Tachycardia (HR > 100 BPM)",
            HeartRhythm::Irregular => "Irregular Rhythm Detected",
            HeartRhythm::Other => "Other Rhythm Pattern",
        }
    }
}

/// Heart rate data point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartRateData {
    /// Heart rate in BPM
    pub hr: u16,
    /// Heart rate variability in ms (RMSSD)
    pub hrv: f32,
    /// SpO2 percentage (if available)
    pub spo2: Option<u8>,
    /// Timestamp (ms since boot)
    pub timestamp: u32,
}

impl HeartRateData {
    /// Create new heart rate data
    pub fn new(hr: u16, hrv: f32, spo2: Option<u8>, timestamp: u32) -> Self {
        Self {
            hr,
            hrv,
            spo2,
            timestamp,
        }
    }

    /// Check if heart rate is in safe range
    pub fn is_safe(&self) -> bool {
        self.hr >= 40 && self.hr <= 180
    }

    /// Get the heart-rate zone for the default adult profile.
    ///
    /// **This assumes a 30-year-old** (max HR 190) and is only a convenience
    /// for callers with no user profile. Anything that acts on the zone —
    /// `sports_coach`'s danger warning, the breathing pattern, the coaching
    /// messages — must use [`Self::zone_for_profile`] instead: a 70-year-old
    /// at 145 BPM is at 97 % of a 150 max and in the danger band, but this
    /// function reports `Cardio` and no warning would be raised.
    pub fn zone(&self) -> HeartRateZone {
        HeartRateZone::from_hr_and_age(self.hr, 30)
    }

    /// Get the heart-rate zone for a specific user profile.
    ///
    /// Uses the profile's stored maximum heart rate when it has one, falling
    /// back to the age formula (`220 - age`) otherwise. Preferring `max_hr`
    /// matters because it is the field a clinician or the user can set
    /// directly, and the age formula is wrong for athletes and for anyone
    /// with a measured maximum.
    pub fn zone_for_profile(&self, profile: &UserProfile) -> HeartRateZone {
        let max_hr = if profile.max_hr > 0 {
            profile.max_hr
        } else {
            220u16.saturating_sub(profile.age as u16)
        };
        HeartRateZone::from_hr_and_max(self.hr, max_hr)
    }

    /// Get stress level from HRV
    pub fn stress_level(&self) -> StressLevel {
        StressLevel::from_hrv(self.hrv)
    }
}

/// Blood glucose data point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlucoseData {
    /// Glucose value in mg/dL
    pub glucose: f32,
    /// Trend direction: -1 falling, 0 stable, 1 rising
    pub trend: i8,
    /// Timestamp (ms since boot)
    pub timestamp: u32,
}

impl GlucoseData {
    /// Create new glucose data
    pub fn new(glucose: f32, trend: i8, timestamp: u32) -> Self {
        Self {
            glucose,
            trend,
            timestamp,
        }
    }

    /// Get glucose status
    pub fn status(&self) -> GlucoseStatus {
        GlucoseStatus::from_glucose(self.glucose)
    }

    /// Check if value is in safe range
    pub fn is_safe(&self) -> bool {
        self.glucose >= 70.0 && self.glucose <= 180.0
    }
}

/// ECG data summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcgData {
    /// Heart rate derived from ECG
    pub hr: u16,
    /// Detected rhythm
    pub rhythm: HeartRhythm,
    /// RR interval in ms
    pub rr_interval: f32,
    /// Signal quality (0-100)
    pub quality: u8,
    /// Timestamp (ms since boot)
    pub timestamp: u32,
}

impl EcgData {
    /// Create new ECG data
    pub fn new(
        hr: u16,
        rhythm: HeartRhythm,
        rr_interval: f32,
        quality: u8,
        timestamp: u32,
    ) -> Self {
        Self {
            hr,
            rhythm,
            rr_interval,
            quality,
            timestamp,
        }
    }

    /// Check if signal quality is acceptable
    pub fn is_quality_acceptable(&self) -> bool {
        self.quality >= 70
    }
}

/// Where a temperature reading was taken.
///
/// The two sites have **incompatible** normal ranges — core body temperature
/// is 36.0–37.5 °C while skin is several degrees cooler — so a single
/// threshold cannot judge both. The type used to assume core, which made a
/// healthy 34 °C skin reading "not normal, no fever" (neither state) and an
/// exercised 38 °C skin reading a "体温偏高" warning about a fever that the
/// sensor could not have measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TemperatureSite {
    /// Core body temperature (rectal / oral / oesophageal).
    #[default]
    ///
    /// Normal 36.0–37.5 °C; above 37.5 °C is a fever.
    Core,
    /// Skin temperature (wrist, forehead, chest).
    ///
    /// Normal 32.0–36.0 °C. Skin runs cooler than core and rises with
    /// exertion and ambient heat, so it cannot establish a fever — see
    /// [`TemperatureData::has_fever`].
    Skin,
}

impl TemperatureSite {
    /// The inclusive normal range for this site, in °C.
    pub fn normal_range(self) -> (f32, f32) {
        match self {
            TemperatureSite::Core => (36.0, 37.5),
            TemperatureSite::Skin => (32.0, 36.0),
        }
    }
}

/// Temperature data
///
/// Carries the measurement [`TemperatureSite`] because the normal range and
/// the meaning of "fever" depend on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemperatureData {
    /// Temperature in Celsius
    pub temperature: f32,
    /// Where it was measured.
    ///
    /// Defaults to [`TemperatureSite::Core`] when absent so a record written
    /// before this field existed still deserialises — the old type implicitly
    /// meant core, which is exactly what `Default` encodes.
    #[serde(default)]
    pub site: TemperatureSite,
    /// Timestamp (ms since boot)
    pub timestamp: u32,
}

impl TemperatureData {
    /// Create new **core** temperature data.
    ///
    /// Kept for compatibility; it assumes the core site, which is what this
    /// constructor always meant. Use [`Self::at_site`] for a skin reading.
    pub fn new(temperature: f32, timestamp: u32) -> Self {
        Self::at_site(temperature, TemperatureSite::Core, timestamp)
    }

    /// Create temperature data for a specific measurement site.
    pub fn at_site(temperature: f32, site: TemperatureSite, timestamp: u32) -> Self {
        Self {
            temperature,
            site,
            timestamp,
        }
    }

    /// Check if the temperature is within its site's normal range.
    pub fn is_normal(&self) -> bool {
        let (low, high) = self.site.normal_range();
        self.temperature >= low && self.temperature <= high
    }

    /// Whether this reading indicates a fever.
    ///
    /// Only meaningful for [`TemperatureSite::Core`]. A skin sensor cannot
    /// establish a core temperature, so a skin reading **never** reports a
    /// fever — the honest answer — and `false` is returned rather than
    /// comparing a 38 °C skin reading against a core threshold.
    pub fn has_fever(&self) -> bool {
        match self.site {
            TemperatureSite::Core => self.temperature > 37.5,
            TemperatureSite::Skin => false,
        }
    }

    /// Whether the reading is above its site's normal band, without claiming
    /// a diagnosis.
    ///
    /// This is the signal a skin sensor *can* provide: "warmer than usual",
    /// which is what `sports_coach` should warn about.
    pub fn is_elevated(&self) -> bool {
        let (_, high) = self.site.normal_range();
        self.temperature > high
    }
}

/// Health sensor manager
pub struct HealthSensorManager {
    /// Heart rate history
    hr_history: Vec<HeartRateData, MAX_HEALTH_HISTORY>,
    /// Glucose history
    glucose_history: Vec<GlucoseData, MAX_HEALTH_HISTORY>,
    /// ECG history
    ecg_history: Vec<EcgData, 64>,
    /// Temperature history
    temp_history: Vec<TemperatureData, 64>,
    /// User profile
    profile: UserProfile,
}

/// User profile for personalized calculations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    /// User age for HR zone calculation
    pub age: u8,
    /// Resting heart rate
    pub resting_hr: u16,
    /// Maximum heart rate
    pub max_hr: u16,
    /// Emergency contact name
    pub emergency_contact: String<64>,
    /// Emergency contact phone
    pub emergency_phone: String<32>,
    /// Known medical conditions
    pub conditions: Vec<String<32>, 8>,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            age: 30,
            resting_hr: 60,
            max_hr: 190,
            emergency_contact: try_heapless::<64>("Emergency Contact"),
            emergency_phone: try_heapless::<32>("120"),
            conditions: Vec::new(),
        }
    }
}

impl UserProfile {
    /// Create new user profile
    //
    // The two `try_from().map_err(...)` calls below translate a
    // capacity overflow into a typed `AgentError::MemoryAllocation
    // Failed`. Replacing them with the infallible `String::from`
    // would silently panic on overflow, which would be a regression
    // for the public error contract.
    #[allow(clippy::unnecessary_fallible_conversions)]
    pub fn new(age: u8, emergency_contact: &str, emergency_phone: &str) -> Result<Self> {
        let max_hr = 220u16.saturating_sub(age as u16);
        Ok(Self {
            age,
            resting_hr: 60,
            max_hr,
            emergency_contact: String::try_from(emergency_contact).map_err(|_| {
                AgentError::MemoryAllocationFailed {
                    requested: 64,
                    available: 0,
                }
            })?,
            emergency_phone: String::try_from(emergency_phone).map_err(|_| {
                AgentError::MemoryAllocationFailed {
                    requested: 32,
                    available: 0,
                }
            })?,
            conditions: Vec::new(),
        })
    }

    /// Calculate heart rate reserve (HRR).
    ///
    /// Saturating: `UserProfile::new(250, …)` yields `max_hr == 0` while the
    /// default resting rate is 60, so the old plain subtraction panicked in
    /// debug and wrapped to ~65476 in release (producing nonsense target
    /// zones). `resting_hr` is a public field and can be set past `max_hr`
    /// anyway.
    pub fn heart_rate_reserve(&self) -> u16 {
        self.max_hr.saturating_sub(self.resting_hr)
    }

    /// Calculate target heart rate for a given zone
    pub fn target_hr(&self, zone: HeartRateZone) -> (u16, u16) {
        let hrr = self.heart_rate_reserve() as f32;
        let rest = self.resting_hr as f32;

        match zone {
            HeartRateZone::Rest => (rest as u16, rest as u16),
            HeartRateZone::WarmUp => (rest as u16, (rest + hrr * 0.4) as u16),
            HeartRateZone::FatBurn => ((rest + hrr * 0.4) as u16, (rest + hrr * 0.6) as u16),
            HeartRateZone::Cardio => ((rest + hrr * 0.6) as u16, (rest + hrr * 0.8) as u16),
            HeartRateZone::Peak => ((rest + hrr * 0.8) as u16, self.max_hr),
            HeartRateZone::Danger => (self.max_hr, 220), // Beyond max
        }
    }
}

impl HealthSensorManager {
    /// Create new health sensor manager
    pub fn new() -> Self {
        Self {
            hr_history: Vec::new(),
            glucose_history: Vec::new(),
            ecg_history: Vec::new(),
            temp_history: Vec::new(),
            profile: UserProfile::default(),
        }
    }

    /// Update user profile
    pub fn set_profile(&mut self, profile: UserProfile) {
        self.profile = profile;
    }

    /// Get user profile
    pub fn profile(&self) -> &UserProfile {
        &self.profile
    }

    /// Add heart rate data point
    pub fn add_heart_rate(&mut self, mut data: HeartRateData) -> Result<()> {
        if self.hr_history.push(data.clone()).is_err() {
            // Remove oldest if full
            let _ = self.hr_history.remove(0);
            let _ = self.hr_history.push(data);
        } else {
            // suppress unused mut warning when push succeeds
            let _ = &mut data;
        }
        Ok(())
    }

    /// Get latest heart rate data
    pub fn latest_heart_rate(&self) -> Option<&HeartRateData> {
        self.hr_history.last()
    }

    /// Get heart rate history
    pub fn heart_rate_history(&self) -> &[HeartRateData] {
        &self.hr_history
    }

    /// Calculate average heart rate over last N samples
    pub fn average_hr(&self, samples: usize) -> Option<f32> {
        let count = samples.min(self.hr_history.len());
        if count == 0 {
            return None;
        }
        let sum: u32 = self
            .hr_history
            .iter()
            .rev()
            .take(count)
            .map(|d| d.hr as u32)
            .sum();
        Some(sum as f32 / count as f32)
    }

    /// Calculate average HRV over last N samples
    pub fn average_hrv(&self, samples: usize) -> Option<f32> {
        let count = samples.min(self.hr_history.len());
        if count == 0 {
            return None;
        }
        let sum: f32 = self
            .hr_history
            .iter()
            .rev()
            .take(count)
            .map(|d| d.hrv)
            .sum();
        Some(sum / count as f32)
    }

    /// Add glucose data point
    pub fn add_glucose(&mut self, data: GlucoseData) -> Result<()> {
        if self.glucose_history.push(data.clone()).is_err() {
            let _ = self.glucose_history.remove(0);
            let _ = self.glucose_history.push(data);
        }
        Ok(())
    }

    /// Get latest glucose data
    pub fn latest_glucose(&self) -> Option<&GlucoseData> {
        self.glucose_history.last()
    }

    /// Get glucose history
    pub fn glucose_history(&self) -> &[GlucoseData] {
        &self.glucose_history
    }

    /// Add ECG data point
    pub fn add_ecg(&mut self, data: EcgData) -> Result<()> {
        if self.ecg_history.push(data.clone()).is_err() {
            let _ = self.ecg_history.remove(0);
            let _ = self.ecg_history.push(data);
        }
        Ok(())
    }

    /// Get latest ECG data
    pub fn latest_ecg(&self) -> Option<&EcgData> {
        self.ecg_history.last()
    }

    /// Get ECG history
    pub fn ecg_history(&self) -> &[EcgData] {
        &self.ecg_history
    }

    /// Add temperature data point
    pub fn add_temperature(&mut self, data: TemperatureData) -> Result<()> {
        if self.temp_history.push(data.clone()).is_err() {
            let _ = self.temp_history.remove(0);
            let _ = self.temp_history.push(data);
        }
        Ok(())
    }

    /// Get latest temperature data
    pub fn latest_temperature(&self) -> Option<&TemperatureData> {
        self.temp_history.last()
    }

    /// Check if heart rate is elevated (for exercise detection)
    pub fn is_exercising(&self) -> bool {
        if let Some(hr) = self.latest_heart_rate() {
            hr.hr > self.profile.resting_hr.saturating_add(30)
        } else {
            false
        }
    }

    /// Overall stress level, combining HRV and heart rate.
    ///
    /// The doc comment always said "combining HRV and HR", but the body only
    /// looked at HRV: a user at 190 BPM with HRV 85 read as `Low("Relaxed")`.
    /// Severe tachycardia is not relaxation, and this value feeds the
    /// intervention and notification paths, so HR now raises the verdict.
    ///
    /// HR contributes when it is clearly above the resting rate (the same
    /// `+30 BPM` band `is_exercising` uses) and scales with how far above it
    /// sits, so ordinary exercise — where high HR is expected, not stress —
    /// does not by itself report maximum stress.
    pub fn current_stress(&self) -> StressLevel {
        let Some(hr) = self.latest_heart_rate() else {
            return StressLevel::Low;
        };
        let from_hrv = hr.stress_level();

        // Heart-rate bands relative to the user's resting rate.
        let elevated = self.profile.resting_hr.saturating_add(30);
        let high = self.profile.resting_hr.saturating_add(60);
        let from_hr = if hr.hr >= high {
            StressLevel::VeryHigh
        } else if hr.hr >= elevated {
            StressLevel::High
        } else {
            StressLevel::Low
        };

        // The worse of the two readings wins: stress is only ever raised by
        // the HR channel, never masked by a reassuring HRV value.
        core::cmp::max(from_hrv, from_hr)
    }

    /// Clear all history
    pub fn clear_history(&mut self) {
        self.hr_history.clear();
        self.glucose_history.clear();
        self.ecg_history.clear();
        self.temp_history.clear();
    }
}

impl Default for HealthSensorManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
mod std_impl {
    use super::*;

    impl HealthSensorManager {
        /// Simulate heart rate data (for testing)
        pub fn simulate_heart_rate(is_exercising: bool, minutes_elapsed: u32) -> HeartRateData {
            let base_hr = if is_exercising { 140 } else { 70 };
            let variation = ((minutes_elapsed as f32 * 0.1).sin() * 10.0) as i16;
            let hr = (base_hr as i16 + variation).clamp(50, 200) as u16;

            let base_hrv = if is_exercising { 20.0 } else { 60.0 };
            let hrv = base_hrv + ((minutes_elapsed as f32 * 0.2).sin() * 5.0);

            // `minutes_elapsed * 60000` overflows `u32` past 71 582 minutes
            // (~50 days of simulated uptime): panic under overflow checks,
            // a wrapped (wrong) timestamp in release. Saturate instead.
            HeartRateData::new(hr, hrv, Some(98), minutes_elapsed.saturating_mul(60_000))
        }

        /// Simulate glucose data (for testing)
        pub fn simulate_glucose(hours_since_meal: f32) -> GlucoseData {
            let base = 100.0;
            let rise = (hours_since_meal * 30.0).min(50.0);
            let variation = (hours_since_meal * 0.5).sin() * 10.0;
            let glucose = base + rise + variation;

            let trend = if hours_since_meal < 2.0 {
                1
            } else if hours_since_meal < 4.0 {
                0
            } else {
                -1
            };

            GlucoseData::new(glucose, trend, (hours_since_meal * 3600.0 * 1000.0) as u32)
        }

        /// Simulate ECG data (for testing)
        pub fn simulate_ecg(is_irregular: bool) -> EcgData {
            let hr = if is_irregular { 75 } else { 72 };
            let rhythm = if is_irregular {
                HeartRhythm::Irregular
            } else {
                HeartRhythm::Normal
            };
            let rr_interval = 60000.0 / hr as f32;

            EcgData::new(hr, rhythm, rr_interval, 95, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heart_rate_zone_names() {
        assert_eq!(HeartRateZone::Rest.name(), "Rest");
        assert_eq!(HeartRateZone::WarmUp.name(), "Warm-up");
        assert_eq!(HeartRateZone::FatBurn.name(), "Fat Burn");
        assert_eq!(HeartRateZone::Cardio.name(), "Cardio");
        assert_eq!(HeartRateZone::Peak.name(), "Peak");
        assert_eq!(HeartRateZone::Danger.name(), "Danger");
    }

    #[test]
    fn heart_rate_zone_matches_documented_bands() {
        // age 30 -> max_hr 190. Bands: <50 Rest, <60 WarmUp, <70 FatBurn,
        // <80 Cardio, <90 Peak, >=90 Danger.
        assert_eq!(HeartRateZone::from_hr_and_age(90, 30), HeartRateZone::Rest);
        assert_eq!(
            HeartRateZone::from_hr_and_age(95, 30),
            HeartRateZone::WarmUp
        );
        assert_eq!(
            HeartRateZone::from_hr_and_age(114, 30),
            HeartRateZone::FatBurn
        );
        assert_eq!(
            HeartRateZone::from_hr_and_age(133, 30),
            HeartRateZone::Cardio
        );
        assert_eq!(HeartRateZone::from_hr_and_age(152, 30), HeartRateZone::Peak);
        assert_eq!(
            HeartRateZone::from_hr_and_age(171, 30),
            HeartRateZone::Danger
        );
        assert_eq!(
            HeartRateZone::from_hr_and_age(189, 30),
            HeartRateZone::Danger
        );
    }

    #[test]
    fn stress_level_from_hrv_and_description() {
        assert_eq!(StressLevel::from_hrv(80.0), StressLevel::Low);
        assert_eq!(StressLevel::from_hrv(79.9), StressLevel::Moderate);
        assert_eq!(StressLevel::from_hrv(50.0), StressLevel::Moderate);
        assert_eq!(StressLevel::from_hrv(49.9), StressLevel::High);
        assert_eq!(StressLevel::from_hrv(25.0), StressLevel::High);
        assert_eq!(StressLevel::from_hrv(24.9), StressLevel::VeryHigh);
        assert_eq!(StressLevel::Low.description(), "Relaxed");
        assert_eq!(StressLevel::Moderate.description(), "Normal");
        assert_eq!(StressLevel::High.description(), "Stressed");
        assert_eq!(StressLevel::VeryHigh.description(), "Very Stressed");
    }

    #[test]
    fn glucose_status_and_descriptions() {
        assert_eq!(GlucoseStatus::from_glucose(69.9), GlucoseStatus::Low);
        assert_eq!(GlucoseStatus::from_glucose(70.0), GlucoseStatus::Normal);
        assert_eq!(GlucoseStatus::from_glucose(99.9), GlucoseStatus::Normal);
        assert_eq!(GlucoseStatus::from_glucose(100.0), GlucoseStatus::Elevated);
        assert_eq!(GlucoseStatus::from_glucose(125.9), GlucoseStatus::Elevated);
        assert_eq!(GlucoseStatus::from_glucose(126.0), GlucoseStatus::High);
        assert!(GlucoseStatus::Low.description().contains("Hypoglycemia"));
        assert!(GlucoseStatus::High.description().contains("Diabetes"));
    }

    #[test]
    fn heart_rhythm_descriptions() {
        assert_eq!(HeartRhythm::Normal.description(), "Normal Sinus Rhythm");
        assert_eq!(
            HeartRhythm::Bradycardia.description(),
            "Bradycardia (HR < 60 BPM)"
        );
        assert_eq!(
            HeartRhythm::Tachycardia.description(),
            "Tachycardia (HR > 100 BPM)"
        );
        assert_eq!(
            HeartRhythm::Irregular.description(),
            "Irregular Rhythm Detected"
        );
        assert_eq!(HeartRhythm::Other.description(), "Other Rhythm Pattern");
    }

    #[test]
    fn sensor_data_methods() {
        let hr = HeartRateData::new(100, 60.0, Some(98), 1000);
        assert!(hr.is_safe());
        assert_eq!(hr.zone(), HeartRateZone::from_hr_and_age(100, 30));
        assert_eq!(hr.stress_level(), StressLevel::Moderate);
        let hi = HeartRateData::new(200, 10.0, None, 0);
        assert!(!hi.is_safe());
        assert_eq!(hi.stress_level(), StressLevel::VeryHigh);

        let g = GlucoseData::new(90.0, 0, 1);
        assert_eq!(g.status(), GlucoseStatus::Normal);
        assert!(g.is_safe());
        let g_high = GlucoseData::new(200.0, 0, 1);
        assert!(!g_high.is_safe());
        assert_eq!(g_high.status(), GlucoseStatus::High);

        let ecg_ok = EcgData::new(72, HeartRhythm::Normal, 833.0, 90, 1);
        assert!(ecg_ok.is_quality_acceptable());
        let ecg_bad = EcgData::new(72, HeartRhythm::Irregular, 833.0, 50, 1);
        assert!(!ecg_bad.is_quality_acceptable());

        assert!(TemperatureData::new(37.0, 1).is_normal());
        assert!(TemperatureData::new(38.0, 1).has_fever());
        assert!(!TemperatureData::new(35.0, 1).is_normal());
        assert!(!TemperatureData::new(35.0, 1).has_fever());
    }

    /// A record written before `site` existed must still deserialise.
    ///
    /// The old type had only `temperature` and `timestamp`, and implicitly
    /// meant core. Without `#[serde(default)]` every stored reading would
    /// fail to parse, which on a device means losing the temperature history.
    #[test]
    fn a_temperature_record_without_a_site_is_read_as_core() {
        let legacy = r#"{"temperature":38.2,"timestamp":1234}"#;
        let parsed: TemperatureData = serde_json::from_str(legacy).expect("legacy JSON");
        assert_eq!(parsed.site, TemperatureSite::Core);
        assert_eq!(parsed.temperature, 38.2);
        assert!(parsed.has_fever(), "a legacy 38.2 reading is a core fever");

        // A new record round-trips with its site.
        let skin = TemperatureData::at_site(34.0, TemperatureSite::Skin, 1);
        let json = serde_json::to_string(&skin).expect("serialise");
        let back: TemperatureData = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.site, TemperatureSite::Skin);
    }

    /// Each site is judged on its own band, and only core can report a fever.
    #[test]
    fn temperature_sites_have_their_own_normal_bands() {
        // Healthy skin (34 °C) is normal on the skin scale, and is neither
        // normal nor a fever on the core scale — the defect this fixes.
        let skin = TemperatureData::at_site(34.0, TemperatureSite::Skin, 0);
        assert!(skin.is_normal());
        assert!(!skin.has_fever());
        assert!(!skin.is_elevated());

        // The same value as a *core* reading is hypothermic, not normal.
        let core = TemperatureData::at_site(34.0, TemperatureSite::Core, 0);
        assert!(!core.is_normal());
        assert!(!core.has_fever());

        // Exercised skin is elevated for its site but is not a fever: a skin
        // sensor cannot establish a core temperature.
        let warm = TemperatureData::at_site(38.0, TemperatureSite::Skin, 0);
        assert!(!warm.is_normal());
        assert!(warm.is_elevated());
        assert!(
            !warm.has_fever(),
            "a skin reading must not claim a fever"
        );

        // Core thresholds still work.
        let fever = TemperatureData::at_site(38.0, TemperatureSite::Core, 0);
        assert!(fever.has_fever());
        assert!(fever.is_elevated());
        assert!(!fever.is_normal());

        // `new` is the core constructor, as it always documented.
        assert_eq!(TemperatureData::new(37.0, 0).site, TemperatureSite::Core);
    }

    #[test]
    fn user_profile_default_new_and_errors() {
        let d = UserProfile::default();
        assert_eq!(d.age, 30);
        assert_eq!(d.resting_hr, 60);
        assert_eq!(d.max_hr, 190);
        assert_eq!(d.heart_rate_reserve(), 130);

        let p = UserProfile::new(40, "Alice", "123").unwrap();
        assert_eq!(p.age, 40);
        assert_eq!(p.max_hr, 180);
        assert_eq!(p.heart_rate_reserve(), 120);

        // Overlong name / contact -> typed error, not a panic.
        let long_name = "x".repeat(65);
        assert!(UserProfile::new(30, &long_name, "123").is_err());
        let long_contact = "1".repeat(33);
        assert!(UserProfile::new(30, "A", &long_contact).is_err());
    }

    /// `age > 220` makes `max_hr == 0`, below the default resting rate.
    /// The reserve and the exercise threshold must saturate, not panic
    /// (debug) or wrap (release).
    #[test]
    fn heart_rate_reserve_saturates_on_an_impossible_profile() {
        let p = UserProfile::new(250, "A", "120").unwrap();
        assert_eq!(p.max_hr, 0);
        assert_eq!(p.heart_rate_reserve(), 0);
        // Multiplying a zero reserve must stay in the resting band.
        let (lo, hi) = p.target_hr(HeartRateZone::Cardio);
        assert_eq!(lo, hi);

        let mut manager = HealthSensorManager::new();
        manager.profile = p;
        assert!(!manager.is_exercising());
    }

    #[test]
    fn target_hr_covers_all_zones() {
        let p = UserProfile::default(); // rest 60, max 190, hrr 130
        assert_eq!(p.target_hr(HeartRateZone::Rest), (60, 60));
        assert_eq!(p.target_hr(HeartRateZone::Danger), (190, 220));
        let (lo, hi) = p.target_hr(HeartRateZone::Cardio);
        assert!(lo <= hi);
        assert!(lo >= 60 && hi <= 190);
    }

    #[test]
    fn manager_tracks_history_and_trends() {
        let mut m = HealthSensorManager::new();
        assert_eq!(m.profile().age, 30);

        assert!(m.latest_heart_rate().is_none());
        assert!(m.average_hr(5).is_none());
        assert!(m.average_hrv(5).is_none());
        assert!(!m.is_exercising());
        assert_eq!(m.current_stress(), StressLevel::Low);

        m.add_heart_rate(HeartRateData::new(70, 60.0, None, 1))
            .unwrap();
        m.add_heart_rate(HeartRateData::new(100, 20.0, None, 2))
            .unwrap();
        m.add_heart_rate(HeartRateData::new(110, 10.0, None, 3))
            .unwrap();

        assert_eq!(m.heart_rate_history().len(), 3);
        assert_eq!(m.latest_heart_rate().unwrap().hr, 110);
        assert!((m.average_hr(3).unwrap() - 93.33).abs() < 0.1);
        assert!((m.average_hrv(2).unwrap() - 15.0).abs() < 0.01);
        assert!(m.is_exercising(), "110 > resting 60 + 30");
        assert_eq!(m.current_stress(), StressLevel::VeryHigh);

        m.add_glucose(GlucoseData::new(95.0, 0, 1)).unwrap();
        assert_eq!(m.latest_glucose().unwrap().glucose, 95.0);
        m.add_ecg(EcgData::new(72, HeartRhythm::Normal, 833.0, 90, 1))
            .unwrap();
        assert_eq!(m.latest_ecg().unwrap().hr, 72);
        m.add_temperature(TemperatureData::new(37.0, 1)).unwrap();
        assert!((m.latest_temperature().unwrap().temperature - 37.0).abs() < 1e-6);

        m.clear_history();
        assert!(m.latest_heart_rate().is_none());
        assert!(m.latest_glucose().is_none());
        assert!(m.latest_ecg().is_none());
        assert!(m.latest_temperature().is_none());
    }

    #[test]
    fn history_caps_at_max_and_drops_oldest() {
        let mut m = HealthSensorManager::new();
        for i in 0..MAX_HEALTH_HISTORY as u32 + 5 {
            m.add_heart_rate(HeartRateData::new((i % 200) as u16, 50.0, None, i))
                .unwrap();
        }
        assert_eq!(m.hr_history.len(), MAX_HEALTH_HISTORY);
        // Oldest (timestamps 0..5) dropped; latest is MAX+4.
        assert_eq!(
            m.latest_heart_rate().unwrap().timestamp,
            MAX_HEALTH_HISTORY as u32 + 4
        );
    }
}

/// The `std`-only simulation helpers back the CLI's `--sim`/demo paths and the
/// `read_sensor` fallbacks. They're feature-gated, so they need their own
/// `#[cfg(all(test, feature = "std"))]` module to be compiled at all.
#[cfg(all(test, feature = "std"))]
mod std_sim_tests {
    use super::*;

    #[test]
    fn simulate_heart_rate_exercising_outranks_resting() {
        let resting = HealthSensorManager::simulate_heart_rate(false, 0);
        let active = HealthSensorManager::simulate_heart_rate(true, 0);
        assert!(active.hr > resting.hr, "exercise must raise the simulated HR");
        // The value is always clamped into the physical 50..=200 band.
        for m in [0u32, 60, 600, 3600] {
            let hr = HealthSensorManager::simulate_heart_rate(true, m).hr;
            assert!((50..=200).contains(&hr), "hr {hr} out of band");
        }
        // The timestamp is the elapsed-minutes converted to milliseconds.
        assert_eq!(HealthSensorManager::simulate_heart_rate(false, 3).timestamp, 180_000);
    }

    #[test]
    fn simulate_glucose_rises_with_time_since_meal() {
        let soon = HealthSensorManager::simulate_glucose(1.0);
        let later = HealthSensorManager::simulate_glucose(3.0);
        let much_later = HealthSensorManager::simulate_glucose(8.0);
        // The trend flips from rising to falling as the meal is digested.
        assert_eq!(soon.trend, 1, "within 2h the trend is up");
        assert_eq!(later.trend, 0, "2..4h the trend is flat");
        assert_eq!(much_later.trend, -1, "past 4h the trend is down");
        assert!(much_later.glucose > soon.glucose, "still elevated later");
    }

    #[test]
    fn simulate_ecg_flags_irregular_rhythm() {
        let normal = HealthSensorManager::simulate_ecg(false);
        assert_eq!(normal.rhythm, HeartRhythm::Normal);
        let irregular = HealthSensorManager::simulate_ecg(true);
        assert_eq!(irregular.rhythm, HeartRhythm::Irregular);
        assert!(irregular.is_quality_acceptable(), "simulated ECG is clean");
    }

    #[test]
    fn set_profile_and_history_accessors() {
        let mut m = HealthSensorManager::new();
        let profile = UserProfile::new(40, "Alice", "123").unwrap();
        m.set_profile(profile);
        assert_eq!(m.profile().age, 40);

        m.add_glucose(GlucoseData::new(95.0, 0, 1)).unwrap();
        m.add_ecg(EcgData::new(72, HeartRhythm::Normal, 833.0, 90, 1))
            .unwrap();
        assert_eq!(m.glucose_history().len(), 1);
        assert_eq!(m.ecg_history().len(), 1);
    }

    /// `current_stress` must combine HRV *and* HR, as its contract says.
    ///
    /// It only looked at HRV, so 190 BPM with a reassuring HRV of 85 ms read
    /// as `Low("Relaxed")` — and this value drives intervention prompts and
    /// notifications.
    #[test]
    fn current_stress_is_raised_by_heart_rate_not_only_hrv() {
        let mut m = HealthSensorManager::new();
        // Default profile: resting 60 BPM.
        m.add_heart_rate(HeartRateData::new(190, 85.0, Some(98), 0))
            .unwrap();
        assert_eq!(
            m.current_stress(),
            StressLevel::VeryHigh,
            "severe tachycardia must not read as relaxed"
        );

        // Elevated but below the `resting + 60` band.
        let mut m2 = HealthSensorManager::new();
        m2.add_heart_rate(HeartRateData::new(110, 85.0, Some(98), 0))
            .unwrap();
        assert_eq!(m2.current_stress(), StressLevel::High);

        // Normal HR and good HRV stays relaxed.
        let mut m3 = HealthSensorManager::new();
        m3.add_heart_rate(HeartRateData::new(65, 85.0, Some(98), 0))
            .unwrap();
        assert_eq!(m3.current_stress(), StressLevel::Low);

        // Low HRV still drives the verdict when HR is unremarkable.
        let mut m4 = HealthSensorManager::new();
        m4.add_heart_rate(HeartRateData::new(70, 10.0, Some(98), 0))
            .unwrap();
        assert_eq!(m4.current_stress(), StressLevel::VeryHigh);
    }
}
