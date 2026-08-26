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

/// Heart rate zones for exercise guidance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        let max_hr = 220 - age as u16;
        let percentage = (hr as f32 / max_hr as f32) * 100.0;

        if percentage < 50.0 {
            HeartRateZone::Rest
        } else if percentage < 60.0 {
            HeartRateZone::WarmUp
        } else if percentage < 70.0 {
            HeartRateZone::FatBurn
        } else if percentage < 85.0 {
            HeartRateZone::Cardio
        } else if percentage < 95.0 {
            HeartRateZone::Peak
        } else {
            HeartRateZone::Danger
        }
    }
}

/// Stress level derived from HRV
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Get heart rate zone (assuming age 30 as default)
    pub fn zone(&self) -> HeartRateZone {
        HeartRateZone::from_hr_and_age(self.hr, 30)
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

/// Body temperature data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemperatureData {
    /// Skin temperature in Celsius
    pub temperature: f32,
    /// Timestamp (ms since boot)
    pub timestamp: u32,
}

impl TemperatureData {
    /// Create new temperature data
    pub fn new(temperature: f32, timestamp: u32) -> Self {
        Self {
            temperature,
            timestamp,
        }
    }

    /// Check if temperature is normal (36.0 - 37.5°C)
    pub fn is_normal(&self) -> bool {
        self.temperature >= 36.0 && self.temperature <= 37.5
    }

    /// Get fever status
    pub fn has_fever(&self) -> bool {
        self.temperature > 37.5
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
        let max_hr = 220 - age as u16;
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

    /// Calculate heart rate reserve (HRR)
    pub fn heart_rate_reserve(&self) -> u16 {
        self.max_hr - self.resting_hr
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
            hr.hr > self.profile.resting_hr + 30
        } else {
            false
        }
    }

    /// Get overall stress level combining HRV and HR
    pub fn current_stress(&self) -> StressLevel {
        if let Some(hr) = self.latest_heart_rate() {
            hr.stress_level()
        } else {
            StressLevel::Low
        }
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

            HeartRateData::new(hr, hrv, Some(98), minutes_elapsed * 60000)
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
