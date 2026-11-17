//! Chronic Disease Early Warning System for mAgent
//!
//! Provides proactive health monitoring and alerting with:
//! - Continuous blood glucose trend monitoring and prediction
//! - ECG/heart rhythm anomaly detection
//! - Automatic emergency contact notification
//! - Nearby hospital recommendation
//! - Alert history tracking
//!
//! This module analyzes health sensor data to predict potential issues
//! before they become critical, enabling early intervention.

use crate::error::{try_heapless, AgentError, Result};
use crate::health_sensors::{
    EcgData, GlucoseData, GlucoseStatus, HeartRateData, HeartRhythm, UserProfile,
};
use core::fmt::Write;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum alert history entries
pub const MAX_ALERT_HISTORY: usize = 50;

/// Maximum emergency contacts
pub const MAX_EMERGENCY_CONTACTS: usize = 5;

/// Whether a cooldown that started at `last` has elapsed by `now`.
///
/// `last == None` means **nothing has been raised yet**, which always counts
/// as elapsed. The three `last_*_ms` fields used to be plain `u32`s starting
/// at `0`, so the cooldown test read "never raised" as "raised at t=0" and
/// suppressed the first alert of a device's life: no glucose alert in the
/// first 5 minutes of uptime, no ECG alert in the first minute, no emergency
/// in the first 30 — the exact readings a freshly booted health monitor exists
/// to report.
///
/// A `now` *before* `last` is a clock that stepped backwards (an NTP
/// correction, a `u32` millisecond counter wrapping after ~49 days); the
/// saturating subtraction reports no elapsed time, which suppresses a
/// duplicate rather than firing one.
fn cooldown_elapsed(last: Option<u32>, now: u32, cooldown_ms: u32) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_sub(last) >= cooldown_ms,
    }
}

/// Per-condition cooldown bookkeeping.
///
/// One slot per *condition* (high/low, low/high/trend), not one per channel.
///
/// Round 20c replaced severity-only escalation with "a different condition
/// also breaks the cooldown". That let an alternating feed through on every
/// sample: HR 205 then 35 then 205 differ in condition each time, so the
/// cooldown was defeated 60 times a second and a 50-entry history filled in
/// under 50 s. The intent was always to collapse *repeats of the same
/// warning*, so the honest model is one cooldown **per warning**: a repeat of
/// the same condition waits, a genuinely different one has its own (already
/// elapsed) slot and is raised immediately.
///
/// A strictly more severe reading of the *same* condition still breaks that
/// condition's cooldown — that is the `escalates` rule and it is unchanged.
/// A *different* condition is not an escalation of the first; it is a
/// separate episode with its own budget.
#[derive(Debug, Clone, Copy)]
struct ConditionCooldown {
    /// When this condition last raised an alert.
    last_ms: u32,
    /// Severity of that alert, so an escalation can break through.
    severity: AlertSeverity,
    /// Slot is unused until the first alert for this condition.
    armed: bool,
}

impl ConditionCooldown {
    /// An unused slot: nothing raised for this condition yet.
    const fn new() -> Self {
        Self {
            last_ms: 0,
            severity: AlertSeverity::Low,
            armed: false,
        }
    }

    /// Record that `severity` was raised at `now`.
    fn record(&mut self, now: u32, severity: AlertSeverity) {
        self.last_ms = now;
        self.severity = severity;
        self.armed = true;
    }

    /// Whether an alert for this condition may be raised at `now`.
    ///
    /// Idempotent: it does not mutate, so the caller records only when it
    /// actually raises (see [`Self::record`]).
    fn allows(&self, now: u32, severity: AlertSeverity, cooldown_ms: u32) -> bool {
        if !self.armed {
            return true;
        }
        now.saturating_sub(self.last_ms) >= cooldown_ms || severity > self.severity
    }
}

/// The condition index for a heart-rate alert (one slot per direction).
fn hr_slot(direction: HrDirection) -> usize {
    match direction {
        HrDirection::High => 0,
        HrDirection::Low => 1,
    }
}

/// How many HR condition slots exist.
const HR_SLOTS: usize = 2;

/// The condition index for a glucose alert.
///
/// Exhaustive on purpose. A wildcard arm made every future `AlertType`
/// (blood pressure, SpO₂, …) share one slot, so two unrelated conditions
/// would silently suppress each other. A new variant now fails to compile
/// here, which is the point: the author must decide its slot.
fn glucose_slot(alert_type: AlertType) -> usize {
    match alert_type {
        AlertType::GlucoseLow => 0,
        AlertType::GlucoseHigh => 1,
        AlertType::GlucoseTrendFalling => 2,
        AlertType::GlucoseTrendRising => 3,
        // Not produced by `process_glucose`, so they never reach this
        // function today; giving each its own slot keeps that true if one
        // ever does.
        AlertType::EcgAnomaly => 4,
        AlertType::HeartRateAbnormal => 5,
        AlertType::BloodPressureAbnormal => 6,
        AlertType::OxygenLow => 7,
    }
}

/// How many glucose condition slots exist.
const GLUCOSE_SLOTS: usize = 8;

/// The condition index for an ECG alert.
fn ecg_slot(kind: EcgAnomalyType) -> usize {
    match kind {
        EcgAnomalyType::IrregularRhythm => 0,
        EcgAnomalyType::ProlongedQt => 1,
        EcgAnomalyType::ShortPr => 2,
        EcgAnomalyType::Tachycardia => 3,
        EcgAnomalyType::Bradycardia => 4,
        EcgAnomalyType::StSegmentChange => 5,
        EcgAnomalyType::TWaveInversion => 6,
        EcgAnomalyType::MissedBeats => 7,
    }
}

/// How many ECG condition slots exist.
const ECG_SLOTS: usize = 8;

/// The primary anomaly an ECG analysis is about, for its cooldown slot.
///
/// `process_ecg` reports one alert per analysis, and its `risk_level` is
/// derived from the anomaly set. A second reading with the *same* severity
/// but a different primary anomaly (severe bradycardia, then severe
/// tachycardia) is a different event with its own cooldown slot.
///
/// The most severe anomaly wins. `Iterator::max_by_key` returns the **last**
/// maximal element, so on a tie the later anomaly is reported — the opposite
/// of what this comment used to claim.
fn ecg_alert_condition(analysis: &EcgAnalysis) -> EcgAnomalyType {
    analysis
        .anomalies
        .iter()
        .max_by_key(|a| a.severity)
        .map(|a| a.anomaly_type)
        .unwrap_or(EcgAnomalyType::IrregularRhythm)
}

/// Which side of the healthy band a heart-rate alert came from.
///
/// `AlertType::HeartRateAbnormal` covers both, so the condition has to be
/// carried separately so the cooldown can tell a Critical-high from a
/// Critical-low.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrDirection {
    /// Above the healthy range.
    High,
    /// Below the healthy range.
    Low,
}

/// The rate limit an ECG anomaly crossed, in BPM.
///
/// `process_ecg` applies fixed bands — bradycardia below 40, tachycardia
/// above 180, an irregular rhythm with no numeric limit of its own. The alert
/// has to report the band it was derived from so the reading and the limit can
/// be compared; the value used to be a blanket 100.0, so a 35 BPM bradycardia
/// alert claimed a threshold of 100.
fn ecg_threshold_bpm(analysis: &EcgAnalysis) -> f32 {
    // Highest-severity anomaly wins, matching `assess_risk`.
    let anomaly = analysis.anomalies.iter().max_by_key(|a| a.severity);
    match anomaly.map(|a| a.anomaly_type) {
        Some(EcgAnomalyType::Bradycardia) => {
            // Bradycardia is *any* rate below the normal band; the 40 BPM
            // figure only decides the severity. A 55 BPM reading at Medium
            // severity has crossed 60, not 40, so reporting 40 claimed a
            // limit the reading never passed.
            bradycardia_threshold(anomaly.map(|a| a.severity))
        }
        Some(EcgAnomalyType::Tachycardia) => tachycardic_threshold(anomaly.map(|a| a.severity)),
        // An irregular rhythm has no single rate bound; report the nominal
        // normal-rate limit rather than pretending one was crossed.
        _ => 100.0,
    }
}

/// The low-rate limit a bradycardia alert actually crossed, in BPM.
///
/// `Critical` is the `< 40` band; lower severities only established "below
/// normal", which `HeartRhythm::Bradycardia` defines as 60.
fn bradycardia_threshold(severity: Option<AlertSeverity>) -> f32 {
    match severity {
        Some(AlertSeverity::Critical) => 40.0,
        _ => 60.0,
    }
}

/// The high-rate limit a tachycardia alert actually crossed, in BPM.
///
/// `Critical` is the `> 180` band; lower severities only established "above
/// normal", which `HeartRhythm::Tachycardia` defines as 100.
fn tachycardic_threshold(severity: Option<AlertSeverity>) -> f32 {
    match severity {
        Some(AlertSeverity::Critical) => 180.0,
        _ => 100.0,
    }
}

/// Alert severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertSeverity {
    /// Informational; do not bother the user.
    Low = 0,
    /// Worth surfacing in the next notification batch.
    Medium = 1,
    /// Loud notification + haptic.
    High = 2,
    /// Triggers the emergency-protocol path (call contacts, etc.).
    Critical = 3,
}

impl AlertSeverity {
    /// Get severity name
    pub fn name(&self) -> &'static str {
        match self {
            AlertSeverity::Low => "Low",
            AlertSeverity::Medium => "Medium",
            AlertSeverity::High => "High",
            AlertSeverity::Critical => "Critical",
        }
    }

    /// Whether this severity requires immediate notification
    pub fn requires_emergency(&self) -> bool {
        matches!(self, AlertSeverity::Critical)
    }
}

/// Alert type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertType {
    /// Hypoglycaemia — blood glucose below safe range.
    GlucoseLow,
    /// Hyperglycaemia — blood glucose above safe range.
    GlucoseHigh,
    /// Trend shows glucose climbing fast — warning before an alert.
    GlucoseTrendRising,
    /// Trend shows glucose dropping fast.
    GlucoseTrendFalling,
    /// ECG classification flagged an abnormal rhythm.
    EcgAnomaly,
    /// Heart rate outside the expected resting / active band.
    HeartRateAbnormal,
    /// Systolic / diastolic outside the configured healthy range.
    BloodPressureAbnormal,
    /// SpO₂ below the safe threshold.
    OxygenLow,
}

/// Alert information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthAlert {
    /// Alert ID
    pub id: u32,
    /// Alert type
    pub alert_type: AlertType,
    /// Severity
    pub severity: AlertSeverity,
    /// Current value
    pub current_value: f32,
    /// Threshold that was crossed
    pub threshold: f32,
    /// Human-readable message
    pub message: String<256>,
    /// Recommendation
    pub recommendation: String<256>,
    /// Timestamp
    pub timestamp: u32,
    /// Whether emergency contact was notified
    pub emergency_contacted: bool,
    /// Whether user acknowledged
    pub acknowledged: bool,
}

impl HealthAlert {
    /// Create new alert
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u32,
        alert_type: AlertType,
        severity: AlertSeverity,
        current_value: f32,
        threshold: f32,
        message: &str,
        recommendation: &str,
        timestamp: u32,
    ) -> Self {
        // HARDENING (audit-2026-08 unwrap sweep): `message` and
        // `recommendation` come from medical rule evaluation. A rule that
        // produces a message longer than 255 chars would previously panic
        // the entire health monitoring loop — exactly the wrong time to
        // crash. `try_heapless` silently truncates at the UTF-8
        // boundary instead, preserving partial alerting capability.
        Self {
            id,
            alert_type,
            severity,
            current_value,
            threshold,
            message: try_heapless::<256>(message),
            recommendation: try_heapless::<256>(recommendation),
            timestamp,
            emergency_contacted: false,
            acknowledged: false,
        }
    }

    /// Mark as acknowledged
    pub fn acknowledge(&mut self) {
        self.acknowledged = true;
    }

    /// Mark emergency contact notified
    pub fn mark_emergency_notified(&mut self) {
        self.emergency_contacted = true;
    }
}

/// A raised emergency notification, waiting for a host to deliver it.
///
/// `magent-core` has no radio and no SMS stack, so the emergency protocol
/// cannot put this on the wire itself. What it *can* do — and previously did
/// not — is decide what to send and to whom, and hand that to the layer that
/// has the transport: the ESP32's AT surface, the host web admin, or an app
/// through the Lua bridge. Before this existed, a Critical alert ran a
/// cooldown check and returned, so the alert reached no one and the caller
/// could not tell a handled emergency from an ignored one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmergencyNotification {
    /// ID of the alert that raised this notification.
    pub alert_id: u32,
    /// Message body, ready to send (see
    /// [`EarlyWarningSystem::generate_emergency_message`]).
    pub message: String<512>,
    /// Name of the highest-priority SMS-capable contact, if one is
    /// configured.
    pub contact_name: String<64>,
    /// Phone number of that contact.
    pub contact_phone: String<32>,
    /// Timestamp the notification was raised at.
    pub timestamp: u32,
}

/// Emergency contact
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmergencyContact {
    /// Contact name
    pub name: String<64>,
    /// Contact phone
    pub phone: String<32>,
    /// Relationship
    pub relationship: String<32>,
    /// Priority (lower = higher priority)
    pub priority: u8,
    /// Whether this contact can receive SMS alerts
    pub sms_enabled: bool,
}

impl EmergencyContact {
    /// Create new emergency contact
    pub fn new(name: &str, phone: &str, relationship: &str, priority: u8) -> Self {
        // HARDENING (audit-2026-08 unwrap sweep): user-provided contact data
        // is not bounded by compile-time constants. `try_heapless` keeps
        // the constructor panic-free even for long names.
        Self {
            name: try_heapless::<64>(name),
            phone: try_heapless::<32>(phone),
            relationship: try_heapless::<32>(relationship),
            priority,
            sms_enabled: true,
        }
    }
}

/// Hospital information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hospital {
    /// Hospital name
    pub name: String<64>,
    /// Address
    pub address: String<128>,
    /// Phone
    pub phone: String<32>,
    /// Distance in meters (if available)
    pub distance_m: Option<u32>,
    /// Emergency room available
    pub has_er: bool,
    /// 24 hours
    pub is_24h: bool,
    /// Has cardiology
    pub has_cardiology: bool,
    /// Has endocrinology
    pub has_endocrinology: bool,
}

impl Hospital {
    /// Create new hospital
    pub fn new(name: &str, address: &str, phone: &str) -> Self {
        // HARDENING (audit-2026-08 unwrap sweep): `name`, `address`, and
        // `phone` come from external database / API responses which are
        // not bounded by the field sizes. `try_heapless` prevents panic.
        Self {
            name: try_heapless::<64>(name),
            address: try_heapless::<128>(address),
            phone: try_heapless::<32>(phone),
            distance_m: None,
            has_er: true,
            is_24h: true,
            has_cardiology: false,
            has_endocrinology: false,
        }
    }

    /// Get distance string
    pub fn distance_string(&self) -> String<32> {
        if let Some(dist) = self.distance_m {
            if dist < 1000 {
                let mut s: String<32> = String::new();
                let _ = write!(s, "{}m", dist);
                s
            } else {
                let mut s: String<32> = String::new();
                let _ = write!(s, "{:.1}km", dist as f32 / 1000.0);
                s
            }
        } else {
            let mut s: String<32> = String::new();
            let _ = write!(s, "Distance unknown");
            s
        }
    }
}

/// Glucose trend prediction result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlucoseTrendPrediction {
    /// Predicted glucose in 30 minutes
    pub predicted_30min: f32,
    /// Predicted glucose in 60 minutes
    pub predicted_60min: f32,
    /// Trend direction (-1 falling, 0 stable, 1 rising)
    pub trend: i8,
    /// Rate of change (mg/dL per minute)
    pub rate_of_change: f32,
    /// Risk level (0-100)
    pub risk_level: u8,
    /// Prediction confidence (0-100)
    pub confidence: u8,
}

impl GlucoseTrendPrediction {
    /// Get risk description
    pub fn risk_description(&self) -> &'static str {
        match self.risk_level {
            0..=20 => "Low risk",
            21..=50 => "Moderate risk",
            51..=75 => "High risk",
            _ => "Critical risk",
        }
    }
}

/// ECG analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcgAnalysis {
    /// Heart rate
    pub heart_rate: u16,
    /// Detected rhythm
    pub rhythm: HeartRhythm,
    /// QT interval (ms)
    pub qt_interval: f32,
    /// PR interval (ms)
    pub pr_interval: f32,
    /// Signal quality
    pub quality: u8,
    /// Anomalies detected
    pub anomalies: Vec<EcgAnomaly, 4>,
    /// Risk assessment
    pub risk_level: AlertSeverity,
    /// Recommendation
    pub recommendation: String<128>,
}

impl EcgAnalysis {
    /// Create new analysis
    pub fn new(heart_rate: u16, rhythm: HeartRhythm, quality: u8) -> Self {
        let mut analysis = Self {
            heart_rate,
            rhythm,
            qt_interval: 400.0, // Normal QT
            pr_interval: 150.0, // Normal PR
            quality,
            anomalies: Vec::new(),
            risk_level: AlertSeverity::Low,
            recommendation: String::try_from("Normal rhythm observed").unwrap(),
        };

        analysis.assess_risk();
        analysis
    }

    /// Assess overall risk
    fn assess_risk(&mut self) {
        // Check for anomalies
        let anomaly_count = self.anomalies.len();
        let has_serious_anomaly = self
            .anomalies
            .iter()
            .any(|a| a.severity >= AlertSeverity::High);

        if has_serious_anomaly || anomaly_count >= 3 {
            self.risk_level = AlertSeverity::Critical;
            self.recommendation = String::try_from(
                "Serious cardiac anomaly detected. Seek immediate medical attention.",
            )
            .unwrap();
        } else if anomaly_count >= 2 {
            self.risk_level = AlertSeverity::High;
            self.recommendation = String::try_from(
                "Multiple cardiac irregularities detected. Consult a cardiologist.",
            )
            .unwrap();
        } else if anomaly_count >= 1 {
            self.risk_level = AlertSeverity::Medium;
            self.recommendation = String::try_from(
                "Minor cardiac irregularity detected. Monitor and consider medical consultation.",
            )
            .unwrap();
        }
    }
}

/// ECG anomaly type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EcgAnomalyType {
    /// R-R intervals vary by more than the configured threshold.
    IrregularRhythm,
    /// QTc interval is longer than the safe upper bound.
    ProlongedQt,
    /// PR interval is shorter than the safe lower bound.
    ShortPr,
    /// Sustained rate above the high-rate threshold.
    Tachycardia,
    /// Sustained rate below the low-rate threshold.
    Bradycardia,
    /// ST segment elevation or depression.
    StSegmentChange,
    /// T wave flipped relative to the patient's baseline.
    TWaveInversion,
    /// One or more expected beats are missing.
    MissedBeats,
}

impl EcgAnomalyType {
    /// Get description
    pub fn description(&self) -> &'static str {
        match self {
            EcgAnomalyType::IrregularRhythm => "Irregular heart rhythm pattern",
            EcgAnomalyType::ProlongedQt => "Prolonged QT interval - risk of arrhythmia",
            EcgAnomalyType::ShortPr => "Short PR interval - possible pre-excitation",
            EcgAnomalyType::Tachycardia => "Heart rate too fast for current activity",
            EcgAnomalyType::Bradycardia => "Heart rate too slow",
            EcgAnomalyType::StSegmentChange => "ST segment change - possible ischemia",
            EcgAnomalyType::TWaveInversion => "T wave inversion - possible cardiac stress",
            EcgAnomalyType::MissedBeats => "Missed or extra heartbeats detected",
        }
    }
}

/// ECG anomaly
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcgAnomaly {
    /// Anomaly type
    pub anomaly_type: EcgAnomalyType,
    /// Severity
    pub severity: AlertSeverity,
    /// Value
    pub value: f32,
    /// Description
    pub description: String<64>,
}

impl EcgAnomaly {
    /// Create new anomaly
    pub fn new(anomaly_type: EcgAnomalyType, severity: AlertSeverity, value: f32) -> Self {
        Self {
            anomaly_type,
            severity,
            value,
            description: String::try_from(anomaly_type.description()).unwrap(),
        }
    }
}

/// Chronic Disease Early Warning System
pub struct EarlyWarningSystem {
    /// Alert history
    alert_history: Vec<HealthAlert, MAX_ALERT_HISTORY>,
    /// Emergency contacts
    emergency_contacts: Vec<EmergencyContact, MAX_EMERGENCY_CONTACTS>,
    /// Nearby hospitals
    hospitals: Vec<Hospital, 8>,
    /// Last alert ID
    next_alert_id: u32,
    /// Glucose reading count
    glucose_reading_count: u32,
    /// One cooldown per glucose condition (low / high / falling / rising).
    last_glucose_cooldown: [ConditionCooldown; GLUCOSE_SLOTS],
    /// Cooldown between glucose alerts (ms)
    glucose_alert_cooldown_ms: u32,
    /// One cooldown per ECG anomaly kind.
    last_ecg_cooldown: [ConditionCooldown; ECG_SLOTS],
    /// Cooldown between ECG alerts (ms)
    ecg_alert_cooldown_ms: u32,
    /// One cooldown per heart-rate direction (high / low). Heart-rate alerts
    /// had no cooldown at all, so a per-second feed produced one alert per
    /// sample and filled the 50-entry history in under a minute.
    last_hr_cooldown: [ConditionCooldown; HR_SLOTS],
    /// Cooldown between heart-rate alerts (ms).
    hr_alert_cooldown_ms: u32,
    /// ECG samples refused because the sensor graded them unusable.
    ///
    /// See [`Self::process_ecg`]: a low-quality sample raises no alert, and
    /// this counter is the only way for a caller to tell that apart from a
    /// normal reading. A rising count means the electrodes need attention.
    ecg_samples_rejected_low_quality: u32,
    /// Consecutive low glucose count
    consecutive_low_count: u8,
    /// Consecutive high glucose count
    consecutive_high_count: u8,
    /// Last notification time for emergency, or `None` if none has been
    /// raised.
    last_emergency_notification_ms: Option<u32>,
    /// Cooldown for emergency notifications (ms) - 30 minutes
    emergency_cooldown_ms: u32,
    /// Most recent emergency notification awaiting delivery by the host.
    pending_emergency: Option<EmergencyNotification>,
}

impl EarlyWarningSystem {
    /// Create new early warning system
    pub fn new() -> Self {
        Self {
            alert_history: Vec::new(),
            emergency_contacts: Vec::new(),
            hospitals: Vec::new(),
            next_alert_id: 1,
            glucose_reading_count: 0,
            last_glucose_cooldown: [ConditionCooldown::new(); GLUCOSE_SLOTS],
            glucose_alert_cooldown_ms: 300_000, // 5 minutes
            last_ecg_cooldown: [ConditionCooldown::new(); ECG_SLOTS],
            ecg_alert_cooldown_ms: 60_000, // 1 minute
            last_hr_cooldown: [ConditionCooldown::new(); HR_SLOTS],
            hr_alert_cooldown_ms: 60_000, // 1 minute
            ecg_samples_rejected_low_quality: 0,
            consecutive_low_count: 0,
            consecutive_high_count: 0,
            last_emergency_notification_ms: None,
            emergency_cooldown_ms: 1_800_000, // 30 minutes
            pending_emergency: None,
        }
    }

    /// Add emergency contact
    pub fn add_emergency_contact(&mut self, contact: EmergencyContact) -> Result<()> {
        if self.emergency_contacts.push(contact).is_err() {
            return Err(AgentError::MemoryAllocationFailed {
                requested: 1,
                available: 0,
            });
        }
        // Sort by priority
        self.emergency_contacts.sort_by_key(|c| c.priority);
        Ok(())
    }

    /// Get emergency contacts
    pub fn emergency_contacts(&self) -> &[EmergencyContact] {
        &self.emergency_contacts
    }

    /// Add hospital, keeping the **nearest** ones when the list is full.
    ///
    /// When all 8 slots were taken the old body evicted the farthest stored
    /// hospital and inserted the newcomer unconditionally — so adding a
    /// 100 km hospital evicted the 8 km one, and `nearest_er` (used by the
    /// emergency SMS) then pointed at a hospital 92 km further away than the
    /// one just discarded. The list must stay the 8 nearest: a newcomer that
    /// is farther than everything stored is rejected outright.
    pub fn add_hospital(&mut self, hospital: Hospital) -> Result<()> {
        let new_dist = hospital.distance_m.unwrap_or(u32::MAX);
        if self.hospitals.is_full() {
            let farthest = self
                .hospitals
                .iter()
                .map(|h| h.distance_m.unwrap_or(u32::MAX))
                .max()
                .unwrap_or(u32::MAX);
            if new_dist >= farthest {
                // Everything stored is at least as close: keep it.
                return Ok(());
            }
            if let Some(pos) = self
                .hospitals
                .iter()
                .enumerate()
                .max_by_key(|(_, h)| h.distance_m.unwrap_or(u32::MAX))
                .map(|(i, _)| i)
            {
                let _ = self.hospitals.remove(pos);
            }
        }
        // `is_full` was handled above, so this cannot fail in practice; if the
        // capacity were ever lowered, reporting the drop beats pretending.
        self.hospitals
            .push(hospital)
            .map_err(|_| AgentError::BufferOverflow {
                capacity: 8,
                attempted: 9,
            })?;
        // Sort by distance
        self.hospitals
            .sort_by_key(|h| h.distance_m.unwrap_or(u32::MAX));
        Ok(())
    }

    /// Get hospitals
    pub fn hospitals(&self) -> &[Hospital] {
        &self.hospitals
    }

    /// Get nearest hospital with ER
    pub fn nearest_er(&self) -> Option<&Hospital> {
        self.hospitals.iter().find(|h| h.has_er)
    }

    /// Get nearest hospital with specialty
    pub fn nearest_with_specialty(
        &self,
        has_cardiology: bool,
        has_endocrinology: bool,
    ) -> Option<&Hospital> {
        self.hospitals.iter().find(|h| {
            h.has_er
                && ((has_cardiology && h.has_cardiology)
                    || (has_endocrinology && h.has_endocrinology))
        })
    }

    /// Process glucose data and check for alerts
    pub fn process_glucose(
        &mut self,
        glucose: &GlucoseData,
        _profile: &UserProfile,
        current_time_ms: u32,
    ) -> Option<HealthAlert> {
        self.glucose_reading_count += 1;

        let status = glucose.status();

        // Track consecutive readings
        match status {
            GlucoseStatus::Low => {
                self.consecutive_low_count = self.consecutive_low_count.saturating_add(1);
                self.consecutive_high_count = 0;
            }
            // `Elevated` (100–126) is *not* a high-glucose reading, and
            // counting it here corrupted the "sustained >180" escalation:
            // 105, 110, then 190 made `consecutive_high_count == 3`, so a
            // single genuinely high sample was reported as sustained
            // hyperglycaemia. Only `High` (the status that covers >180)
            // increments the counter; `Elevated` is a normal-ish reading that
            // must not reset it either, so it neither counts nor clears.
            //
            // `High` itself spans 126..∞, but the escalation it feeds is the
            // *> 180* alert below. Counting a 130 mg/dL prediabetic reading
            // toward it made 130, 132, 190 read as "sustained hyperglycaemia"
            // when only the last sample had crossed 180 — the same defect the
            // `Elevated` fix addressed, one band higher. Count only readings
            // that would themselves raise the high-glucose alert.
            GlucoseStatus::High => {
                if glucose.glucose > 180.0 {
                    self.consecutive_high_count = self.consecutive_high_count.saturating_add(1);
                }
                self.consecutive_low_count = 0;
            }
            GlucoseStatus::Elevated => {
                self.consecutive_low_count = 0;
            }
            GlucoseStatus::Normal => {
                self.consecutive_low_count = 0;
                self.consecutive_high_count = 0;
            }
        }

        // Determine alert conditions
        let (alert_type, severity, threshold, message, recommendation) = if glucose.glucose < 54.0 {
            // Critical low (< 54 mg/dL)
            (
                AlertType::GlucoseLow,
                AlertSeverity::Critical,
                54.0,
                "严重低血糖警告！血糖过低，可能危及生命。",
                "立即摄入15-20克快速碳水化合物，如果意识不清请立即呼叫急救。",
            )
        } else if glucose.glucose < 70.0 {
            // Low (54-70 mg/dL)
            if self.consecutive_low_count >= 2 {
                (
                    AlertType::GlucoseLow,
                    AlertSeverity::High,
                    70.0,
                    "低血糖警告！血糖持续偏低。",
                    "请立即摄入碳水化合物，并检测血糖变化。",
                )
            } else {
                (
                    AlertType::GlucoseLow,
                    AlertSeverity::Medium,
                    70.0,
                    "血糖偏低提醒。注意监测并适当补充能量。",
                    "建议摄入少量碳水化合物。",
                )
            }
        } else if glucose.glucose > 250.0 {
            // Critical high (> 250 mg/dL)
            (
                AlertType::GlucoseHigh,
                AlertSeverity::Critical,
                250.0,
                "严重高血糖警告！血糖过高。",
                "检查是否有酮体，如有不适应立即就医。",
            )
        } else if glucose.glucose > 180.0 {
            // High (180-250 mg/dL)
            if self.consecutive_high_count >= 3 {
                (
                    AlertType::GlucoseHigh,
                    AlertSeverity::High,
                    180.0,
                    "高血糖警告！血糖持续偏高。",
                    "注意补充水分，考虑调整胰岛素或药物剂量。",
                )
            } else {
                (
                    AlertType::GlucoseHigh,
                    AlertSeverity::Medium,
                    180.0,
                    "血糖偏高提醒。",
                    "注意饮食和药物控制。",
                )
            }
        } else if glucose.trend == -1 && glucose.glucose < 100.0 {
            // Rapidly falling
            (
                AlertType::GlucoseTrendFalling,
                AlertSeverity::Medium,
                100.0,
                "血糖快速下降警告。",
                "注意低血糖风险，准备碳水化合物。",
            )
        } else if glucose.trend == 1 && glucose.glucose > 150.0 {
            // Rapidly rising
            (
                AlertType::GlucoseTrendRising,
                AlertSeverity::Medium,
                150.0,
                "血糖快速上升提醒。",
                "注意餐后血糖控制。",
            )
        } else {
            return None; // No alert needed
        };

        // Each condition has its own cooldown slot. A repeat of *this*
        // warning waits it out; a strictly worse reading of the same
        // condition breaks through (the cooldown collapses repeats, it must
        // not hide a Critical reading and skip the emergency protocol); and a
        // different condition — hypoglycaemia after hyperglycaemia — is a
        // separate episode with its own, already-elapsed budget.
        let slot = glucose_slot(alert_type);
        if !self.last_glucose_cooldown[slot].allows(
            current_time_ms,
            severity,
            self.glucose_alert_cooldown_ms,
        ) {
            return None;
        }

        // Create alert
        let mut alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            severity,
            glucose.glucose,
            threshold,
            message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id = self.next_alert_id.wrapping_add(1);
        self.last_glucose_cooldown[slot].record(current_time_ms, severity);

        // Raise the emergency protocol *before* recording the alert, so the
        // history copy carries the same `emergency_contacted` flag the caller
        // receives. The cloned-then-triggered order left the recorded alert
        // permanently false — the two views of the same alert disagreed.
        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        // Add to history
        self.push_alert(alert.clone());

        Some(alert)
    }

    /// Minimum ECG signal quality (0–100) for an alert to be raised.
    ///
    /// The `EcgData.quality` field is the sensor's own confidence in the
    /// sample. Below this the waveform cannot support a rhythm diagnosis, so
    /// treating it as one is a fabricated medical claim. `health_sensors`
    /// uses the same 70 % bar for `SensorData::is_quality_acceptable`.
    const MIN_TRUSTWORTHY_ECG_QUALITY: u8 = 70;

    /// Append an alert to the bounded history, evicting the oldest when full.
    ///
    /// A bare `push` returned an error once 50 alerts were stored, and the
    /// result was discarded — so the newest alert (the one the caller had
    /// just been handed) was absent from `alert_history`, `acknowledge_alert`
    /// could not find it, and `unacknowledged_alerts` never listed it.
    /// Every other bounded container in this crate evicts the oldest instead.
    fn push_alert(&mut self, alert: HealthAlert) {
        if self.alert_history.push(alert.clone()).is_err() {
            let _ = self.alert_history.remove(0);
            let _ = self.alert_history.push(alert);
        }
    }

    /// Process ECG data and check for alerts
    pub fn process_ecg(
        &mut self,
        ecg: &EcgData,
        current_time_ms: u32,
    ) -> Option<(HealthAlert, EcgAnalysis)> {
        // Perform analysis
        let mut analysis = EcgAnalysis::new(ecg.hr, ecg.rhythm, ecg.quality);

        // Check rhythm. Anomalies are pushed into a fixed-capacity
        // heapless Vec; if it's already full (more than 4 anomalies
        // for a single ECG sample) we just drop the additional ones —
        // assess_risk() looks at the highest-severity entry, so we
        // don't lose the most important signal.
        match ecg.rhythm {
            HeartRhythm::Irregular => {
                let _ = analysis.anomalies.push(EcgAnomaly::new(
                    EcgAnomalyType::IrregularRhythm,
                    AlertSeverity::High,
                    0.0,
                ));
            }
            HeartRhythm::Bradycardia if ecg.hr < 40 => {
                let _ = analysis.anomalies.push(EcgAnomaly::new(
                    EcgAnomalyType::Bradycardia,
                    AlertSeverity::Critical,
                    ecg.hr as f32,
                ));
            }
            HeartRhythm::Bradycardia => {
                let _ = analysis.anomalies.push(EcgAnomaly::new(
                    EcgAnomalyType::Bradycardia,
                    AlertSeverity::Medium,
                    ecg.hr as f32,
                ));
            }
            HeartRhythm::Tachycardia if ecg.hr > 180 => {
                let _ = analysis.anomalies.push(EcgAnomaly::new(
                    EcgAnomalyType::Tachycardia,
                    AlertSeverity::Critical,
                    ecg.hr as f32,
                ));
            }
            HeartRhythm::Tachycardia => {
                let _ = analysis.anomalies.push(EcgAnomaly::new(
                    EcgAnomalyType::Tachycardia,
                    AlertSeverity::Medium,
                    ecg.hr as f32,
                ));
            }
            _ => {}
        }

        analysis.assess_risk();

        // Only create alert if significant issue
        if analysis.risk_level < AlertSeverity::Medium {
            return None;
        }

        // A sample the sensor itself graded as unusable must not raise an
        // alert, let alone the emergency protocol. `quality` was recorded in
        // the analysis and then never consulted, so an ECG with 5 %
        // electrode contact and a noisy irregular rhythm produced a Critical
        // alert claiming "serious cardiac anomaly".
        //
        // This function returns `None` here rather than the analysis: its
        // return type only carries an analysis alongside an alert. The
        // rejection is instead counted, so "no alert" because the sample was
        // unreadable is distinguishable from "no alert" because the rhythm
        // was normal — a distinction the caller previously could not make,
        // and which a rising counter on a device means an electrode problem.
        if ecg.quality < Self::MIN_TRUSTWORTHY_ECG_QUALITY {
            self.ecg_samples_rejected_low_quality =
                self.ecg_samples_rejected_low_quality.saturating_add(1);
            return None;
        }

        // Each anomaly kind has its own cooldown slot, so a Medium
        // irregular-rhythm alert no longer suppresses a Critical bradycardia
        // 30 s later, and alternating between two rhythms cannot defeat the
        // cooldown on every sample.
        let condition = ecg_alert_condition(&analysis);
        let slot = ecg_slot(condition);
        if !self.last_ecg_cooldown[slot].allows(
            current_time_ms,
            analysis.risk_level,
            self.ecg_alert_cooldown_ms,
        ) {
            return None;
        }

        let (alert_type, message, recommendation) = match analysis.risk_level {
            AlertSeverity::Critical => (
                AlertType::EcgAnomaly,
                format!("严重心脏异常！心率{} BPM，检测到心律不齐。", ecg.hr),
                "立即停止活动并就医，必要时呼叫急救！",
            ),
            AlertSeverity::High => (
                AlertType::EcgAnomaly,
                format!("心脏异常警告。心率{} BPM，检测到心律问题。", ecg.hr),
                "建议尽快咨询心脏科医生。",
            ),
            _ => (
                AlertType::HeartRateAbnormal,
                format!("心率异常：{} BPM", ecg.hr),
                "建议进行进一步检查。",
            ),
        };

        // The threshold that was actually crossed. Every ECG alert reported a
        // hard-coded 100.0, so a 35 BPM bradycardia alert claimed a threshold
        // of 100 and the emergency message printed a value/limit pair that
        // made no sense against the reading.
        let threshold = ecg_threshold_bpm(&analysis);

        let mut alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            analysis.risk_level,
            ecg.hr as f32,
            threshold,
            &message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id = self.next_alert_id.wrapping_add(1);
        self.last_ecg_cooldown[slot].record(current_time_ms, analysis.risk_level);

        if analysis.risk_level == AlertSeverity::Critical {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        self.push_alert(alert.clone());

        Some((alert, analysis))
    }

    /// Process heart rate data
    pub fn process_heart_rate(
        &mut self,
        hr_data: &HeartRateData,
        current_time_ms: u32,
    ) -> Option<HealthAlert> {
        // Which side of the band the reading is on. `AlertType` cannot carry
        // this (both directions are `HeartRateAbnormal`) and the cooldown
        // needs it to tell a tachycardia crisis from a bradycardia one.
        // The threshold is the band boundary the reading actually crossed, so
        // the alert can be compared against its own limit. This was a literal
        // `0.0`, so a 205 BPM crisis was recorded as "205 crossed 0" — the same
        // claimed-limit defect the ECG path fixed (see `ecg_threshold_bpm`).
        let (alert_type, severity, threshold, message, recommendation, direction) =
            if hr_data.hr > 200 {
                (
                    AlertType::HeartRateAbnormal,
                    AlertSeverity::Critical,
                    200.0,
                    "危险！心率严重过高。",
                    "立即停止运动并就医！",
                    HrDirection::High,
                )
            } else if hr_data.hr > 180 {
                (
                    AlertType::HeartRateAbnormal,
                    AlertSeverity::High,
                    180.0,
                    "心率过高警告。",
                    "降低运动强度，观察心率变化。",
                    HrDirection::High,
                )
            } else if hr_data.hr < 40 {
                (
                    AlertType::HeartRateAbnormal,
                    AlertSeverity::Critical,
                    40.0,
                    "危险！心率过低。",
                    "立即就医检查！",
                    HrDirection::Low,
                )
            } else {
                return None;
            };

        // One cooldown per direction. Heart-rate alerts had no cooldown at
        // all once, so a per-second feed produced one alert per sample — 60 a
        // minute — and the 50-entry history held nothing but one episode.
        // Keying the cooldown on the *direction* gives both properties this
        // needs at once: a tachycardia crisis and a bradycardia crisis are
        // separate episodes (the second is never swallowed by the first's
        // window), while oscillating between them cannot re-alert every
        // sample, because each direction has its own timer.
        let slot = hr_slot(direction);
        if !self.last_hr_cooldown[slot].allows(
            current_time_ms,
            severity,
            self.hr_alert_cooldown_ms,
        ) {
            return None;
        }

        let mut alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            severity,
            hr_data.hr as f32,
            threshold,
            message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id = self.next_alert_id.wrapping_add(1);
        self.last_hr_cooldown[slot].record(current_time_ms, severity);

        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        self.push_alert(alert.clone());

        Some(alert)
    }

    /// Predict glucose trend
    pub fn predict_glucose_trend(
        &self,
        glucose_history: &[GlucoseData],
    ) -> Option<GlucoseTrendPrediction> {
        if glucose_history.len() < 3 {
            return None;
        }

        // Get recent readings. Use a heapless::Vec with a fixed capacity so
        // this works in both std and no_std builds; 6 entries matches the
        // historical `.take(6)` above.
        let mut recent: Vec<&GlucoseData, 6> = Vec::new();
        for r in glucose_history.iter().rev().take(6) {
            let _ = recent.push(r);
        }
        if recent.len() < 3 {
            return None;
        }

        // Calculate rate of change
        let first = recent.last()?;
        let last = recent.first()?;

        let time_diff = (last.timestamp as f32 - first.timestamp as f32) / 60000.0; // minutes
        if time_diff <= 0.0 {
            return None;
        }

        let glucose_change = last.glucose - first.glucose;
        let rate = glucose_change / time_diff; // mg/dL per minute

        // Project future values
        let predicted_30min = last.glucose + rate * 30.0;
        let predicted_60min = last.glucose + rate * 60.0;

        // Determine risk
        let risk_level = if predicted_30min < 70.0 || predicted_60min > 250.0 {
            80
        } else if predicted_30min < 80.0 || predicted_60min > 200.0 {
            50
        } else {
            20
        };

        Some(GlucoseTrendPrediction {
            predicted_30min,
            predicted_60min,
            trend: if rate > 0.5 {
                1
            } else if rate < -0.5 {
                -1
            } else {
                0
            },
            rate_of_change: rate,
            risk_level,
            confidence: (60 + recent.len() * 5).min(95) as u8,
        })
    }

    /// Raise the emergency protocol for a critical `alert`.
    ///
    /// Returns `true` when a notification was raised, `false` when the
    /// cooldown suppressed it. On `true` the alert is marked
    /// `emergency_contacted` and the notification is left in
    /// [`Self::pending_emergency_notification`] for the host to deliver.
    ///
    /// The protocol's job here is *deciding*: which contact, and what message.
    /// Delivery belongs to the layer that owns a transport — `magent-core`
    /// under `no_std` has neither radio nor SMS, so pretending to send would
    /// leave the caller unable to tell a handled emergency from an ignored
    /// one. What it must not do is return silently, which is what it did
    /// before: no message was built (`generate_emergency_message` was
    /// unreachable from here), no state changed, and the alert went out with
    /// `emergency_contacted: false`.
    fn trigger_emergency_protocol(
        &mut self,
        alert: &mut HealthAlert,
        current_time_ms: u32,
    ) -> bool {
        // Nothing has been raised yet → this is the first, and a cooldown that
        // exists to rate-limit repeats must not suppress it. `cooldown_elapsed`
        // also handles the backwards-timestamp case: the previous subtraction
        // panicked in debug and wrapped in release, which would have read a
        // wrapped value as "well past the cooldown" and fired.
        if !cooldown_elapsed(
            self.last_emergency_notification_ms,
            current_time_ms,
            self.emergency_cooldown_ms,
        ) {
            return false;
        }
        self.last_emergency_notification_ms = Some(current_time_ms);

        // Highest-priority contact that can actually receive an alert.
        // `add_emergency_contact` keeps the list sorted by priority, so the
        // first SMS-capable entry is the one to notify.
        let primary = self.emergency_contacts.iter().find(|c| c.sms_enabled);
        let contact_name = primary.map(|c| c.name.clone()).unwrap_or_default();
        let contact_phone = primary.map(|c| c.phone.clone()).unwrap_or_default();

        let notification = EmergencyNotification {
            alert_id: alert.id,
            message: self.generate_emergency_message(alert),
            contact_name,
            contact_phone,
            timestamp: current_time_ms,
        };

        alert.mark_emergency_notified();
        self.pending_emergency = Some(notification);
        true
    }

    /// Queue a notification the caller has already decided to send — a named
    /// contact and a message that did **not** come from a threshold crossing.
    ///
    /// It shares the awaiting-delivery slot with the alert-driven protocol
    /// (`trigger_emergency_protocol`), because "a message that must
    /// reach this contact" is the same thing to the layer that owns a
    /// transport. Two differences are deliberate:
    /// - **No cooldown.** The 30-minute window exists to rate-limit *repeated*
    ///   alerts from a sensor that keeps reading badly. A user asking to reach
    ///   their contact is a one-off decision, and suppressing it would be the
    ///   device silently ignoring a request.
    /// - **`alert_id` is 0**, meaning "not raised by an alert". Reusing
    ///   `next_alert_id` would invent an alert that never happened.
    ///
    /// `magent-core` has no radio or SMS stack, so this is a decision, not a
    /// send; the caller drains it with [`Self::take_emergency_notification`]
    /// once it has put the message on the wire.
    pub fn queue_notification(
        &mut self,
        contact: &EmergencyContact,
        message: &str,
        timestamp: u32,
    ) -> EmergencyNotification {
        let notification = EmergencyNotification {
            alert_id: 0,
            message: try_heapless::<512>(message),
            contact_name: contact.name.clone(),
            contact_phone: contact.phone.clone(),
            timestamp,
        };
        self.pending_emergency = Some(notification.clone());
        notification
    }

    /// The most recent emergency notification, if one is awaiting delivery.
    ///
    /// Paired with [`Self::take_emergency_notification`], which drains it.
    pub fn pending_emergency_notification(&self) -> Option<&EmergencyNotification> {
        self.pending_emergency.as_ref()
    }

    /// Take the pending emergency notification, clearing it.
    ///
    /// Call this once the host has put the notification on the wire, so the
    /// next critical alert produces a fresh one.
    pub fn take_emergency_notification(&mut self) -> Option<EmergencyNotification> {
        self.pending_emergency.take()
    }

    /// Acknowledge alert
    pub fn acknowledge_alert(&mut self, alert_id: u32) -> bool {
        if let Some(alert) = self.alert_history.iter_mut().find(|a| a.id == alert_id) {
            alert.acknowledge();
            return true;
        }
        false
    }

    /// Get unacknowledged alerts (at most the return buffer's 16).
    ///
    /// The history holds up to 50 alerts, so `collect()` into a `Vec<_, 16>`
    /// panicked (`Vec::from_iter overflow`) once 17 unacknowledged alerts
    /// existed — reachable with 17 abnormal heart-rate samples. Push
    /// explicitly and stop at capacity instead.
    pub fn unacknowledged_alerts(&self) -> Vec<&HealthAlert, 16> {
        let mut out = Vec::new();
        for alert in self.alert_history.iter().filter(|a| !a.acknowledged) {
            if out.push(alert).is_err() {
                break;
            }
        }
        out
    }

    /// Get alert history
    /// How many ECG samples have been refused as untrustworthy.
    ///
    /// Non-zero means the sensor reported a signal too poor to diagnose;
    /// `process_ecg` returns `None` for those without raising an alert.
    pub fn ecg_samples_rejected_low_quality(&self) -> u32 {
        self.ecg_samples_rejected_low_quality
    }

    /// The alert history, newest last, bounded by `MAX_ALERT_HISTORY`.
    pub fn alert_history(&self) -> &[HealthAlert] {
        &self.alert_history
    }

    /// Get critical alerts (at most the return buffer's 8).
    ///
    /// Same overflow hazard as [`Self::unacknowledged_alerts`]: the history
    /// can hold more than the return capacity.
    pub fn critical_alerts(&self) -> Vec<&HealthAlert, 8> {
        let mut out = Vec::new();
        for alert in self
            .alert_history
            .iter()
            .filter(|a| a.severity >= AlertSeverity::High && !a.acknowledged)
        {
            if out.push(alert).is_err() {
                break;
            }
        }
        out
    }

    /// Generate emergency notification message
    pub fn generate_emergency_message(&self, alert: &HealthAlert) -> String<512> {
        let mut message = String::new();

        let _ = writeln!(message, "【健康预警】紧急通知");
        let _ = writeln!(
            message,
            "类型：{}",
            match alert.alert_type {
                AlertType::GlucoseLow => "低血糖警告",
                AlertType::GlucoseHigh => "高血糖警告",
                AlertType::EcgAnomaly => "心脏异常",
                AlertType::HeartRateAbnormal => "心率异常",
                _ => "健康警告",
            }
        );
        let _ = writeln!(message, "数值：{:.1}", alert.current_value);
        let _ = writeln!(message, "详情：{}", alert.message);
        let _ = writeln!(message, "建议：{}", alert.recommendation);

        if let Some(hospital) = self.nearest_er() {
            let _ = writeln!(message);
            let _ = writeln!(message, "最近医院：{}", hospital.name);
            let _ = writeln!(message, "地址：{}", hospital.address);
            let _ = writeln!(message, "电话：{}", hospital.phone);
        }

        message
    }
}

impl Default for EarlyWarningSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> EarlyWarningSystem {
        EarlyWarningSystem::new()
    }

    fn ecg(hr: u16, rhythm: HeartRhythm, ts: u32) -> EcgData {
        EcgData {
            hr,
            rhythm,
            rr_interval: 800.0,
            quality: 90,
            timestamp: ts,
        }
    }

    /// An ECG alert must report the limit the reading actually crossed.
    ///
    /// `ecg_threshold_bpm` used to return the *band boundary* regardless of
    /// severity: a 55 BPM Medium bradycardia (crossed 60) reported
    /// `threshold: 40`, and a 170 BPM Medium tachycardia (crossed 100)
    /// reported `threshold: 180` — neither reading reached the number the
    /// alert claimed it had passed. `HealthAlert` serialises this field, so
    /// it was a fabricated limit in the emitted evidence.
    #[test]
    fn an_ecg_alert_reports_the_limit_it_actually_crossed() {
        let mut m = mgr();
        // Medium bradycardia: below 60, not below 40.
        let (alert, _) = m
            .process_ecg(&ecg(55, HeartRhythm::Bradycardia, 0), 0)
            .expect("a 55 BPM bradycardia is an alert");
        assert_eq!(alert.current_value, 55.0);
        assert_eq!(
            alert.threshold, 60.0,
            "a 55 BPM reading has crossed 60, not 40: {alert:?}"
        );

        // Medium tachycardia: above 100, not above 180.
        let mut m = mgr();
        let (alert, _) = m
            .process_ecg(&ecg(150, HeartRhythm::Tachycardia, 0), 0)
            .expect("a 150 BPM tachycardia is an alert");
        assert_eq!(
            alert.threshold, 100.0,
            "a 150 BPM reading has crossed 100, not 180: {alert:?}"
        );

        // Critical readings do cross the tighter bound.
        let mut m = mgr();
        let (alert, _) = m
            .process_ecg(&ecg(35, HeartRhythm::Bradycardia, 0), 0)
            .expect("a 35 BPM bradycardia is critical");
        assert_eq!(alert.threshold, 40.0, "{alert:?}");
        assert_eq!(alert.severity, AlertSeverity::Critical);
    }

    fn hr_data(hr: u16, ts: u32) -> HeartRateData {
        HeartRateData {
            hr,
            hrv: 50.0,
            spo2: Some(98),
            timestamp: ts,
        }
    }

    #[test]
    fn alert_severity_requires_emergency_only_for_critical() {
        assert!(AlertSeverity::Critical.requires_emergency());
        assert!(!AlertSeverity::High.requires_emergency());
        assert!(!AlertSeverity::Medium.requires_emergency());
        assert!(!AlertSeverity::Low.requires_emergency());
        assert!(!AlertSeverity::Critical.name().is_empty());
    }

    #[test]
    fn glucose_critical_low_triggers_critical() {
        let mut m = mgr();
        // 50 mg/dL < 54 → critical regardless of consecutive count.
        let alert = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("alert");
        assert_eq!(alert.alert_type, AlertType::GlucoseLow);
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(alert.current_value, 50.0);
    }

    #[test]
    fn glucose_low_escalates_to_high_after_two() {
        let mut m = mgr();
        // First low (60) → Medium (consecutive_low_count 1 < 2).
        let a1 = m
            .process_glucose(
                &GlucoseData::new(60.0, -1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("first");
        assert_eq!(a1.severity, AlertSeverity::Medium);
        // Second low (65) → High (consecutive_low_count 2 >= 2).
        let a2 = m
            .process_glucose(
                &GlucoseData::new(65.0, -1, 700_000),
                &UserProfile::default(),
                700_000,
            )
            .expect("second");
        assert_eq!(a2.severity, AlertSeverity::High);
    }

    #[test]
    fn glucose_critical_high_triggers_critical() {
        let mut m = mgr();
        let alert = m
            .process_glucose(
                &GlucoseData::new(260.0, 1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("alert");
        assert_eq!(alert.alert_type, AlertType::GlucoseHigh);
        assert_eq!(alert.severity, AlertSeverity::Critical);
    }

    #[test]
    fn glucose_high_escalates_after_three() {
        let mut m = mgr();
        // Three high readings (200) → Medium, Medium, then High.
        let a1 = m
            .process_glucose(
                &GlucoseData::new(200.0, 1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("1");
        let a2 = m
            .process_glucose(
                &GlucoseData::new(200.0, 1, 700_000),
                &UserProfile::default(),
                700_000,
            )
            .expect("2");
        let a3 = m
            .process_glucose(
                &GlucoseData::new(200.0, 1, 1_000_000),
                &UserProfile::default(),
                1_000_000,
            )
            .expect("3");
        assert_eq!(a1.severity, AlertSeverity::Medium);
        assert_eq!(a2.severity, AlertSeverity::Medium);
        assert_eq!(a3.severity, AlertSeverity::High);
    }

    /// A 126–180 mg/dL reading must not count toward the "> 180 sustained"
    /// escalation.
    ///
    /// `GlucoseStatus::High` starts at 126, but the escalation it feeds is the
    /// `> 180` alert. Counting the whole `High` band made 130, 132, 190 read as
    /// *sustained hyperglycaemia* — a High-severity, insulin-advice alert
    /// backed by one reading that had actually crossed 180. This is the same
    /// defect the `Elevated` (100–126) fix closed, one band higher.
    #[test]
    fn a_prediabetic_reading_does_not_count_toward_sustained_hyperglycaemia() {
        let mut m = mgr();
        // Two readings in the 126–180 band: `High` by status, but neither
        // crosses the > 180 alert boundary, so neither raises an alert at all.
        for (i, value) in [130.0f32, 132.0].iter().enumerate() {
            let ts = 400_000 + i as u32 * 300_000;
            assert!(
                m.process_glucose(
                    &GlucoseData::new(*value, 0, ts),
                    &UserProfile::default(),
                    ts,
                )
                .is_none(),
                "{value} mg/dL is in the prediabetic band and must not alert"
            );
        }
        // The first reading that really is > 180 must still be reported as a
        // single sample, not the third of a "sustained" streak.
        let high = m
            .process_glucose(
                &GlucoseData::new(190.0, 1, 1_000_000),
                &UserProfile::default(),
                1_000_000,
            )
            .expect("a >180 reading alerts");
        assert_eq!(
            high.severity,
            AlertSeverity::Medium,
            "only one reading was ever above 180, so this is not 'sustained': {:?}",
            high.message
        );
        assert!(
            !high.message.contains("持续"),
            "the message must not claim a sustained state: {:?}",
            high.message
        );
    }

    /// A more severe reading must break the cooldown; the cooldown exists to
    /// collapse repeats, not to suppress an emergency.
    ///
    /// Medium (65 mg/dL) at t=0 opened a 5-minute cooldown. A Critical
    /// reading (45 mg/dL) at t=120 s was returned as `None`, so no alert and
    /// — more importantly — no emergency protocol. That is the exact event
    /// the system is for.
    #[test]
    fn a_critical_glucose_reading_breaks_a_lower_severity_cooldown() {
        let mut m = mgr();
        let first = m
            .process_glucose(&GlucoseData::new(65.0, -1, 0), &UserProfile::default(), 0)
            .expect("first low alert");
        assert_eq!(first.severity, AlertSeverity::Medium);

        // 2 minutes later — well inside the 5-minute cooldown — but worse.
        let escalated = m
            .process_glucose(
                &GlucoseData::new(45.0, -1, 120_000),
                &UserProfile::default(),
                120_000,
            )
            .expect("a Critical reading must not be swallowed by the cooldown");
        assert_eq!(escalated.severity, AlertSeverity::Critical);
        assert!(
            escalated.emergency_contacted,
            "the emergency protocol must run for a Critical reading"
        );

        // A repeat at the same severity is still collapsed.
        let repeat = m.process_glucose(
            &GlucoseData::new(45.0, -1, 130_000),
            &UserProfile::default(),
            130_000,
        );
        assert!(
            repeat.is_none(),
            "an identical reading must respect the cooldown"
        );
    }

    /// The flood test above must be able to fail.
    ///
    /// Pins its sensitivity: with a zero-length cooldown (i.e. no
    /// rate-limiting at all, which is what the round-20c rule degenerated to
    /// for an alternating feed) the same loop raises one alert per sample.
    /// Without this, a future change that quietly disables the gate could
    /// leave both the fix and its test looking green.
    #[test]
    fn the_anti_flood_gate_is_what_suppresses_the_repeats() {
        let mut m = mgr();
        m.hr_alert_cooldown_ms = 0;
        let mut alerts = 0;
        for i in 0..10u32 {
            let hr = if i % 2 == 0 { 205 } else { 35 };
            let ts = i * 1_000;
            if m.process_heart_rate(&hr_data(hr, ts), ts).is_some() {
                alerts += 1;
            }
        }
        assert_eq!(
            alerts, 10,
            "with no cooldown every sample must alert — if this fails, the \
             anti-flood test above proves nothing"
        );
    }

    /// Oscillating between opposite conditions must not alert on every sample.
    ///
    /// Round 20c's "a different condition breaks the cooldown" made the gate
    /// trivially defeatable: HR 205 then 35 then 205 differ in condition every
    /// sample, so a 1 Hz feed raised **60 alerts a minute** and filled the
    /// 50-entry history in under 50 s. One cooldown per *condition* keeps the
    /// fix (a new episode is never swallowed) without the flood (each
    /// direction has its own timer).
    #[test]
    fn oscillating_conditions_do_not_alert_on_every_sample() {
        let mut m = mgr();
        let mut alerts = 0;
        // 1 Hz for a minute, alternating between the two crises.
        for i in 0..60u32 {
            let hr = if i % 2 == 0 { 205 } else { 35 };
            let ts = i * 1_000;
            if m.process_heart_rate(&hr_data(hr, ts), ts).is_some() {
                alerts += 1;
            }
        }
        assert!(
            alerts <= 4,
            "60 samples of alternating 205/35 raised {alerts} alerts; the \
             cooldown must collapse each direction to ~1 per minute"
        );
        assert!(alerts >= 2, "both crises must be reported at least once");
    }

    /// The same for glucose: alternating high and low must not flood.
    #[test]
    fn oscillating_glucose_does_not_alert_on_every_sample() {
        let mut m = mgr();
        let mut alerts = 0;
        for i in 0..60u32 {
            let (value, trend) = if i % 2 == 0 { (300.0, 0) } else { (40.0, -1) };
            let ts = i * 1_000;
            if m.process_glucose(
                &GlucoseData::new(value, trend, ts),
                &UserProfile::default(),
                ts,
            )
            .is_some()
            {
                alerts += 1;
            }
        }
        assert!(
            alerts <= 4,
            "60 samples of alternating 300/40 mg/dL raised {alerts} alerts"
        );
        assert!(alerts >= 2, "both conditions must be reported at least once");
    }

    /// A Critical reading in the *opposite* direction must break the cooldown.
    ///
    /// `escalates` compares severity only, so a Critical high glucose
    /// followed by a Critical low (or 205 BPM then 35 BPM, or severe
    /// bradycardia then severe tachycardia) compared equal and was returned
    /// as `None` — never recorded, never notified, no emergency protocol.
    /// That is a life-threatening reading silently discarded by a
    /// rate-limiter that was only ever meant to collapse repeats.
    #[test]
    fn a_critical_alert_in_the_opposite_direction_breaks_the_cooldown() {
        let mut m = mgr();
        let high = m
            .process_glucose(&GlucoseData::new(300.0, 0, 0), &UserProfile::default(), 0)
            .expect("first Critical high");
        assert_eq!(high.severity, AlertSeverity::Critical);

        // 60 s later — inside the cooldown, same severity, opposite condition.
        let low = m
            .process_glucose(&GlucoseData::new(40.0, -1, 60_000), &UserProfile::default(), 60_000)
            .expect("a Critical low must not be swallowed by the high's cooldown");
        assert_eq!(low.severity, AlertSeverity::Critical);
        assert_eq!(low.alert_type, AlertType::GlucoseLow);
        // The *alert* is raised and recorded. The emergency *contact* is
        // governed by its own 30-minute window (`emergency_cooldown_ms`),
        // which is a deliberate anti-spam limit on paging a human, not part
        // of the per-channel alert cooldown this test is about — see the
        // BACKLOG note on re-paging a different crisis inside that window.
        assert!(
            m.alert_history().len() >= 2,
            "both alerts must be recorded"
        );

        // A *repeat* of the first condition, still inside its own 5-minute
        // window, is collapsed. That is the property that keeps a noisy feed
        // from filling the history — the opposite condition above does not
        // reset it, because the two have separate slots.
        assert!(
            m.process_glucose(
                &GlucoseData::new(300.0, 0, 130_000),
                &UserProfile::default(),
                130_000
            )
            .is_none(),
            "a repeat of the first condition must respect its own cooldown"
        );
    }

    /// The same for heart rate: 205 BPM then 35 BPM are different emergencies.
    #[test]
    fn a_bradycardia_crisis_breaks_a_tachycardia_cooldown() {
        let mut m = mgr();
        let fast = m
            .process_heart_rate(&HeartRateData::new(205, 50.0, Some(98), 0), 0)
            .expect("critical tachycardia");
        assert_eq!(fast.severity, AlertSeverity::Critical);

        // 10 s later, inside the cooldown, on the other side of the band.
        let slow = m
            .process_heart_rate(&HeartRateData::new(35, 50.0, Some(98), 10_000), 10_000)
            .expect("a Critical bradycardia must not be swallowed by the cooldown");
        assert_eq!(slow.severity, AlertSeverity::Critical);
        assert!(slow.message.contains("过低"), "got {:?}", slow.message);

        // A repeat of the bradycardia is collapsed.
        let repeat = m.process_heart_rate(&HeartRateData::new(35, 50.0, Some(98), 20_000), 20_000);
        assert!(repeat.is_none(), "an identical reading must respect the cooldown");
    }

    /// `Elevated` (100–126) is not a high-glucose reading and must not count
    /// toward the "sustained >180" escalation.
    #[test]
    fn elevated_glucose_does_not_prescalate_a_single_high_reading() {
        let mut m = mgr();
        // Two Elevated readings, then one genuinely High one.
        for (i, value) in [105.0f32, 110.0].iter().enumerate() {
            let ts = 400_000 + i as u32 * 300_000;
            // Elevated may or may not produce an alert; either way it must
            // not increment the high counter.
            let _ = m.process_glucose(
                &GlucoseData::new(*value, 0, ts),
                &UserProfile::default(),
                ts,
            );
        }
        let first_high = m
            .process_glucose(
                &GlucoseData::new(190.0, 1, 1_000_000),
                &UserProfile::default(),
                1_000_000,
            )
            .expect("a >180 reading alerts");
        assert_eq!(
            first_high.severity,
            AlertSeverity::Medium,
            "one real high reading after Elevated samples is not 'sustained'"
        );
    }

    /// An ECG the sensor graded as unusable must not raise a cardiac alert or
    /// the emergency protocol.
    #[test]
    fn a_low_quality_ecg_does_not_raise_a_cardiac_alert() {
        let mut m = mgr();
        let noisy = EcgData {
            hr: 80,
            rhythm: HeartRhythm::Irregular,
            rr_interval: 800.0,
            quality: 5, // unusable electrodes
            timestamp: 0,
        };
        assert!(
            m.process_ecg(&noisy, 0).is_none(),
            "a 5%-quality sample cannot support a rhythm diagnosis"
        );
        assert!(m.alert_history().is_empty());
        // The rejection is observable: "no alert" because the sample was
        // unreadable must be distinguishable from "no alert" because the
        // rhythm was fine.
        assert_eq!(m.ecg_samples_rejected_low_quality(), 1);

        // A good sample still alerts.
        assert!(m
            .process_ecg(&ecg(80, HeartRhythm::Irregular, 1000), 1000)
            .is_some());
    }

    /// Heart-rate alerts honour a cooldown instead of firing once per sample.
    #[test]
    fn repeated_high_heart_rate_alerts_are_rate_limited() {
        let mut m = mgr();
        let first = m.process_heart_rate(&hr_data(185, 0), 0).expect("first");
        assert_eq!(first.severity, AlertSeverity::High);
        // 10 s later, same severity — collapsed.
        assert!(m
            .process_heart_rate(&hr_data(185, 10_000), 10_000)
            .is_none());
        // Still collapsed just before the 60 s cooldown ends.
        assert!(m
            .process_heart_rate(&hr_data(185, 59_000), 59_000)
            .is_none());
        // But a Critical reading goes through immediately.
        let critical = m
            .process_heart_rate(&hr_data(205, 61_000), 61_000)
            .expect("an escalation must not wait out the cooldown");
        assert_eq!(critical.severity, AlertSeverity::Critical);
    }

    /// A full alert history must keep the newest alert, not silently drop it.
    #[test]
    fn the_newest_alert_survives_a_full_history() {
        let mut m = mgr();
        for i in 0..(MAX_ALERT_HISTORY as u32 + 5) {
            let ts = i * 400_000;
            let _ = m.process_glucose(&GlucoseData::new(200.0, 1, ts), &UserProfile::default(), ts);
        }
        assert_eq!(m.alert_history().len(), MAX_ALERT_HISTORY);
        let newest = m.alert_history().last().expect("non-empty");
        assert_eq!(
            newest.id,
            m.next_alert_id - 1,
            "the most recently raised alert must be in the history"
        );
        assert!(
            m.acknowledge_alert(newest.id),
            "the newest alert must be acknowledgeable"
        );
    }

    #[test]
    fn glucose_trend_falling_and_rising_alert() {
        // Falling (trend -1) below 100 → GlucoseTrendFalling.
        let mut m = mgr();
        let a = m
            .process_glucose(
                &GlucoseData::new(90.0, -1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("falling");
        assert_eq!(a.alert_type, AlertType::GlucoseTrendFalling);
        assert_eq!(a.severity, AlertSeverity::Medium);

        // Rising (trend 1) above 150 → GlucoseTrendRising.
        let mut m2 = mgr();
        let a2 = m2
            .process_glucose(
                &GlucoseData::new(160.0, 1, 400_000),
                &UserProfile::default(),
                400_000,
            )
            .expect("rising");
        assert_eq!(a2.alert_type, AlertType::GlucoseTrendRising);
    }

    #[test]
    fn glucose_normal_produces_no_alert() {
        let mut m = mgr();
        assert!(m
            .process_glucose(
                &GlucoseData::new(100.0, 0, 400_000),
                &UserProfile::default(),
                400_000
            )
            .is_none());
    }

    #[test]
    fn glucose_alerts_respect_cooldown() {
        let mut m = mgr();
        // First critical-low alert at t=400000.
        assert!(m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 400_000),
                &UserProfile::default(),
                400_000
            )
            .is_some());
        // A second alert 30s later (within the 5-min cooldown) is suppressed.
        assert!(m
            .process_glucose(
                &GlucoseData::new(52.0, -1, 430_000),
                &UserProfile::default(),
                430_000
            )
            .is_none());
    }

    #[test]
    fn ecg_bradycardia_and_tachycardia_are_critical() {
        let mut m = mgr();
        // Bradycardia with hr < 40 → Critical.
        let (alert, _analysis) = m
            .process_ecg(&ecg(35, HeartRhythm::Bradycardia, 120_000), 120_000)
            .expect("brady");
        assert_eq!(alert.severity, AlertSeverity::Critical);

        let mut m2 = mgr();
        // Tachycardia with hr > 180 → Critical.
        let (alert2, _) = m2
            .process_ecg(&ecg(190, HeartRhythm::Tachycardia, 120_000), 120_000)
            .expect("tachy");
        assert_eq!(alert2.severity, AlertSeverity::Critical);

        // Each alert reports the bound it actually crossed. Every ECG alert
        // used to say `threshold: 100.0`, so a 35 BPM bradycardia alert
        // claimed a limit of 100 and the emergency message's value/limit pair
        // contradicted itself.
        assert_eq!(alert.threshold, 40.0, "bradycardia's limit is 40 BPM");
        assert_eq!(alert2.threshold, 180.0, "tachycardia's limit is 180 BPM");
    }

    #[test]
    fn ecg_irregular_rhythm_escalates_to_critical() {
        // A single High-severity anomaly (irregular rhythm) is treated as
        // "serious" by `assess_risk`, which escalates the overall risk to
        // Critical (conservative over-alerting).
        let mut m = mgr();
        let (alert, _) = m
            .process_ecg(&ecg(80, HeartRhythm::Irregular, 120_000), 120_000)
            .expect("alert");
        assert_eq!(alert.alert_type, AlertType::EcgAnomaly);
        assert_eq!(alert.severity, AlertSeverity::Critical);
    }

    #[test]
    fn ecg_normal_produces_no_alert() {
        let mut m = mgr();
        assert!(m
            .process_ecg(&ecg(70, HeartRhythm::Normal, 120_000), 120_000)
            .is_none());
    }

    #[test]
    fn heart_rate_detects_abnormal_bands() {
        // > 200 → Critical.
        let mut m = mgr();
        let a = m.process_heart_rate(&hr_data(210, 0), 0).expect("high");
        assert_eq!(a.severity, AlertSeverity::Critical);

        // 180 < hr <= 200 → High.
        let mut m2 = mgr();
        let a2 = m2.process_heart_rate(&hr_data(185, 0), 0).expect("high2");
        assert_eq!(a2.severity, AlertSeverity::High);

        // < 40 → Critical.
        let mut m3 = mgr();
        let a3 = m3.process_heart_rate(&hr_data(35, 0), 0).expect("low");
        assert_eq!(a3.severity, AlertSeverity::Critical);

        // Normal → none.
        let mut m4 = mgr();
        assert!(m4.process_heart_rate(&hr_data(70, 0), 0).is_none());
    }

    /// Every heart-rate alert must report the limit its reading crossed.
    ///
    /// The threshold was a literal `0.0`, so a 205 BPM crisis was recorded as
    /// `current_value: 205.0, threshold: 0.0` — a record that reads as "this
    /// reading crossed zero". The ECG path had the identical bug fixed
    /// (`ecg_threshold_bpm`); this pins the same property for heart rate, in
    /// both directions and at both severities.
    #[test]
    fn a_heart_rate_alert_reports_the_limit_it_actually_crossed() {
        for (hr, expected_threshold, severity) in [
            (210u16, 200.0f32, AlertSeverity::Critical), // > 200
            (185, 180.0, AlertSeverity::High),           // 180 < hr <= 200
            (35, 40.0, AlertSeverity::Critical),         // < 40
        ] {
            let mut m = mgr();
            let alert = m
                .process_heart_rate(&hr_data(hr, 0), 1_800_000)
                .unwrap_or_else(|| panic!("{hr} BPM must alert"));
            assert_eq!(alert.severity, severity, "{hr} BPM");
            assert_eq!(
                alert.threshold, expected_threshold,
                "{hr} BPM alert claims a limit of {} — the reading crossed \
                 {expected_threshold}",
                alert.threshold
            );
            // The reported pair has to be comparable: the reading is on the
            // far side of the limit it names.
            if hr > 180 {
                assert!(alert.current_value > alert.threshold);
            } else {
                assert!(alert.current_value < alert.threshold);
            }
        }
    }

    #[test]
    fn predict_glucose_trend_requires_three_readings() {
        let m = mgr();
        let two = vec![
            GlucoseData::new(100.0, 0, 0),
            GlucoseData::new(105.0, 0, 60_000),
        ];
        assert!(m.predict_glucose_trend(&two).is_none());
        assert!(m.predict_glucose_trend(&[]).is_none());
    }

    #[test]
    fn predict_glucose_trend_rising() {
        let m = mgr();
        let history = vec![
            GlucoseData::new(100.0, 0, 0),
            GlucoseData::new(110.0, 0, 60_000),
            GlucoseData::new(120.0, 0, 120_000),
        ];
        let p = m.predict_glucose_trend(&history).expect("prediction");
        assert_eq!(p.trend, 1);
        // rate = 20 mg/dL / 2 min = 10; predicted_60min = 120 + 600 > 250.
        assert_eq!(p.risk_level, 80);
        assert!(p.predicted_30min > 120.0);
    }

    #[test]
    fn predict_glucose_trend_falling() {
        let m = mgr();
        let history = vec![
            GlucoseData::new(120.0, 0, 0),
            GlucoseData::new(110.0, 0, 60_000),
            GlucoseData::new(100.0, 0, 120_000),
        ];
        let p = m.predict_glucose_trend(&history).expect("prediction");
        assert_eq!(p.trend, -1);
    }

    #[test]
    fn hospitals_nearest_and_distance_string() {
        let mut m = mgr();
        let mut far_er = Hospital::new("FarER", "addr1", "111");
        far_er.distance_m = Some(5000);
        far_er.has_er = true;
        let mut near_no_er = Hospital::new("NearNoER", "addr2", "222");
        near_no_er.distance_m = Some(300);
        near_no_er.has_er = false;
        m.add_hospital(far_er.clone()).unwrap();
        m.add_hospital(near_no_er.clone()).unwrap();

        // nearest_er returns the first hospital with an ER (order-dependent).
        assert_eq!(m.nearest_er().unwrap().name.as_str(), "FarER");
        assert_eq!(m.hospitals().len(), 2);

        // distance_string formatting.
        assert_eq!(far_er.distance_string().as_str(), "5.0km");
        assert_eq!(near_no_er.distance_string().as_str(), "300m");
        let unknown = Hospital::new("U", "a", "0"); // distance_m = None
        assert_eq!(unknown.distance_string().as_str(), "Distance unknown");
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;
    use crate::health_sensors::{EcgData, GlucoseData, HeartRateData, UserProfile};

    fn mgr() -> EarlyWarningSystem {
        EarlyWarningSystem::new()
    }

    fn ecg(hr: u16, rhythm: HeartRhythm, ts: u32) -> EcgData {
        EcgData {
            hr,
            rhythm,
            rr_interval: 800.0,
            quality: 90,
            timestamp: ts,
        }
    }

    #[test]
    fn alert_severity_names_and_emergency() {
        assert_eq!(AlertSeverity::Low.name(), "Low");
        assert_eq!(AlertSeverity::Medium.name(), "Medium");
        assert_eq!(AlertSeverity::High.name(), "High");
        assert_eq!(AlertSeverity::Critical.name(), "Critical");
        assert!(!AlertSeverity::Low.requires_emergency());
        assert!(!AlertSeverity::High.requires_emergency());
        assert!(AlertSeverity::Critical.requires_emergency());
    }

    #[test]
    fn alert_acknowledge_and_emergency_flags() {
        let mut a = HealthAlert::new(
            1,
            AlertType::GlucoseLow,
            AlertSeverity::Critical,
            50.0,
            70.0,
            "low",
            "eat",
            0,
        );
        assert!(!a.acknowledged);
        assert!(!a.emergency_contacted);
        a.acknowledge();
        a.mark_emergency_notified();
        assert!(a.acknowledged);
        assert!(a.emergency_contacted);
    }

    #[test]
    fn contacts_capacity_and_sort() {
        let mut s = EarlyWarningSystem::new();
        for i in 0..MAX_EMERGENCY_CONTACTS {
            s.add_emergency_contact(EmergencyContact::new(&format!("c{i}"), "1", "f", i as u8))
                .unwrap();
        }
        let err = s
            .add_emergency_contact(EmergencyContact::new("extra", "1", "f", 0))
            .unwrap_err();
        assert!(matches!(err, AgentError::MemoryAllocationFailed { .. }));
        assert_eq!(s.emergency_contacts().len(), MAX_EMERGENCY_CONTACTS);
        assert_eq!(s.emergency_contacts()[0].priority, 0);
    }

    #[test]
    fn hospitals_capacity_replaces_farthest() {
        let mut s = EarlyWarningSystem::new();
        for i in 0..8u32 {
            let mut h = Hospital::new(&format!("h{i}"), "addr", "tel");
            h.distance_m = Some(i * 1000);
            s.add_hospital(h).unwrap();
        }
        assert_eq!(s.hospitals().len(), 8);
        let mut closer = Hospital::new("closer", "addr", "tel");
        closer.distance_m = Some(100);
        s.add_hospital(closer).unwrap();
        assert_eq!(s.hospitals().len(), 8);
        assert!(s.hospitals().iter().any(|h| h.name.as_str() == "closer"));
        assert!(!s.hospitals().iter().any(|h| h.name.as_str() == "h7"));
    }

    /// A full list must keep the nearest hospitals: a newcomer that is
    /// farther than everything already stored is rejected, not admitted at
    /// the cost of a closer one.
    ///
    /// The old body always evicted the farthest stored hospital and inserted
    /// the newcomer, so adding a 100 km hospital dropped the 8 km one and
    /// `nearest_er` — the address in the emergency SMS — moved 92 km away.
    #[test]
    fn a_full_hospital_list_rejects_a_farther_newcomer() {
        let mut s = EarlyWarningSystem::new();
        for i in 0..8u32 {
            let mut h = Hospital::new(&format!("h{i}"), "addr", "tel");
            h.has_er = true;
            h.distance_m = Some((i + 1) * 1000);
            s.add_hospital(h).unwrap();
        }
        assert_eq!(s.hospitals().len(), 8);
        let nearest_before = s.nearest_er().unwrap().name.clone();

        let mut far = Hospital::new("far", "addr", "tel");
        far.has_er = true;
        far.distance_m = Some(100_000);
        s.add_hospital(far).unwrap();

        assert_eq!(s.hospitals().len(), 8);
        assert!(
            !s.hospitals().iter().any(|h| h.name.as_str() == "far"),
            "a 100 km hospital must not displace an 8 km one"
        );
        assert_eq!(
            s.nearest_er().unwrap().name,
            nearest_before,
            "the emergency target must not change"
        );
        // The 8 km hospital is still there.
        assert!(s.hospitals().iter().any(|h| h.name.as_str() == "h7"));
    }

    /// A closer newcomer still displaces the farthest, and an unknown-distance
    /// newcomer is kept out lane: it cannot be shown to be nearer than
    /// anything already stored.
    #[test]
    fn a_full_hospital_list_admits_a_closer_newcomer_only() {
        let mut s = EarlyWarningSystem::new();
        for i in 0..8u32 {
            let mut h = Hospital::new(&format!("h{i}"), "addr", "tel");
            h.has_er = true;
            h.distance_m = Some((i + 1) * 1000);
            s.add_hospital(h).unwrap();
        }

        // Closer than the 8 km farthest → admitted, farthest evicted.
        let mut closer = Hospital::new("closer", "addr", "tel");
        closer.has_er = true;
        closer.distance_m = Some(500);
        s.add_hospital(closer).unwrap();
        assert!(s.hospitals().iter().any(|h| h.name.as_str() == "closer"));
        assert!(!s.hospitals().iter().any(|h| h.name.as_str() == "h7"));

        // Unknown distance cannot be shown to be nearer → not admitted, and
        // nothing is evicted for it.
        let before: [bool; 8] = core::array::from_fn(|i| {
            s.hospitals()
                .iter()
                .any(|h| h.name.as_str() == alloc::format!("h{i}"))
        });
        let unknown = Hospital::new("unknown", "addr", "tel");
        s.add_hospital(unknown).unwrap();
        assert_eq!(s.hospitals().len(), 8);
        assert!(
            !s.hospitals().iter().any(|h| h.name.as_str() == "unknown"),
            "an unknown-distance hospital must not be admitted on the strength of an unknown distance"
        );
        for (i, present) in before.iter().enumerate() {
            assert_eq!(
                s.hospitals()
                    .iter()
                    .any(|h| h.name.as_str() == alloc::format!("h{i}")),
                *present,
                "h{i} must still be present: an unknown-distance newcomer evicts nobody"
            );
        }
    }

    #[test]
    fn nearest_er_and_specialty() {
        let mut s = EarlyWarningSystem::new();
        let mut cardio = Hospital::new("Cardio", "a", "t");
        cardio.has_er = true;
        cardio.has_cardiology = true;
        cardio.distance_m = Some(2000);
        let mut er_only = Hospital::new("ER", "a", "t");
        er_only.has_er = true;
        er_only.distance_m = Some(1000);
        let mut no_er = Hospital::new("Clinic", "a", "t");
        no_er.has_er = false;
        no_er.distance_m = Some(100);
        s.add_hospital(er_only).unwrap();
        s.add_hospital(cardio).unwrap();
        s.add_hospital(no_er).unwrap();
        assert_eq!(s.nearest_er().unwrap().name.as_str(), "ER");
        assert_eq!(
            s.nearest_with_specialty(true, false).unwrap().name.as_str(),
            "Cardio"
        );
        assert!(s.nearest_with_specialty(false, false).is_none());
    }

    #[test]
    fn hospital_distance_string_variants() {
        let mut h = Hospital::new("h", "a", "t");
        h.distance_m = Some(500);
        assert_eq!(h.distance_string().as_str(), "500m");
        h.distance_m = Some(2500);
        assert_eq!(h.distance_string().as_str(), "2.5km");
        h.distance_m = None;
        assert_eq!(h.distance_string().as_str(), "Distance unknown");
    }

    #[test]
    fn glucose_trend_risk_description() {
        let mut p = GlucoseTrendPrediction {
            predicted_30min: 0.0,
            predicted_60min: 0.0,
            trend: 0,
            rate_of_change: 0.0,
            risk_level: 0,
            confidence: 0,
        };
        assert_eq!(p.risk_description(), "Low risk");
        p.risk_level = 50;
        assert_eq!(p.risk_description(), "Moderate risk");
        p.risk_level = 75;
        assert_eq!(p.risk_description(), "High risk");
        p.risk_level = 100;
        assert_eq!(p.risk_description(), "Critical risk");
    }

    #[test]
    fn ecg_anomaly_descriptions() {
        assert!(EcgAnomalyType::IrregularRhythm
            .description()
            .contains("Irregular"));
        assert!(EcgAnomalyType::ProlongedQt.description().contains("QT"));
        assert!(EcgAnomalyType::Bradycardia.description().contains("slow"));
        assert!(EcgAnomalyType::TWaveInversion
            .description()
            .contains("inversion"));
    }

    #[test]
    fn alert_ack_queries_and_emergency_message() {
        let mut s = EarlyWarningSystem::new();
        s.add_emergency_contact(EmergencyContact::new("c", "1", "f", 0))
            .unwrap();
        let mut er = Hospital::new("St Mary", "Main St", "555");
        er.has_er = true;
        er.distance_m = Some(500);
        s.add_hospital(er).unwrap();

        let alert = s
            .process_glucose(
                &GlucoseData::new(50.0, 0, 1),
                &UserProfile::default(),
                300_001,
            )
            .unwrap();
        assert_eq!(alert.alert_type, AlertType::GlucoseLow);
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(s.alert_history().len(), 1);
        assert_eq!(s.unacknowledged_alerts().len(), 1);
        assert_eq!(s.critical_alerts().len(), 1);

        assert!(s.acknowledge_alert(alert.id));
        assert!(!s.acknowledge_alert(999), "unknown id must return false");
        assert_eq!(s.unacknowledged_alerts().len(), 0);
        assert_eq!(s.critical_alerts().len(), 0);
        assert!(s.alert_history()[0].acknowledged);

        let msg = s.generate_emergency_message(&s.alert_history()[0]);
        assert!(msg.as_str().contains("最近医院"));
        assert!(msg.as_str().contains("St Mary"));
        assert!(msg.as_str().contains("低血糖警告"));
    }

    #[test]
    fn emergency_message_without_er_omits_hospital() {
        let mut s = EarlyWarningSystem::new();
        let alert = s
            .process_glucose(
                &GlucoseData::new(50.0, 0, 1),
                &UserProfile::default(),
                300_001,
            )
            .unwrap();
        let msg = s.generate_emergency_message(&alert);
        assert!(!msg.as_str().contains("最近医院"));
    }

    /// The emergency protocol was a no-op: it checked a cooldown, stored the
    /// timestamp and returned. `generate_emergency_message` was never reached
    /// from it, nothing was recorded, and the alert went out with
    /// `emergency_contacted: false` — so a Critical reading reached nobody and
    /// the caller could not tell a handled emergency from an ignored one.
    #[test]
    fn emergency_protocol_raises_a_notification_and_marks_the_alert() {
        let mut m = EarlyWarningSystem::new();
        m.add_emergency_contact(EmergencyContact::new("Son", "111", "family", 1))
            .unwrap();
        // Priority 0 outranks the above, so this is the one to notify.
        m.add_emergency_contact(EmergencyContact::new("Doctor", "222", "physician", 0))
            .unwrap();
        m.add_hospital(Hospital::new("St Mary", "1 Road", "120"))
            .unwrap();

        // 50 mg/dL < 54 → Critical, and past the 30-minute emergency cooldown
        // (which is measured from boot, so a small timestamp is suppressed).
        let alert = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .expect("alert");

        assert!(
            alert.emergency_contacted,
            "the caller's alert must record the emergency"
        );
        // The recorded copy has to agree with the returned one — the old
        // clone-then-trigger order left the history entry permanently false.
        let recorded = m.alert_history().last().expect("recorded");
        assert_eq!(recorded.id, alert.id);
        assert!(recorded.emergency_contacted, "history must agree");

        let n = m.pending_emergency_notification().expect("a notification");
        assert_eq!(n.alert_id, alert.id);
        assert_eq!(n.timestamp, 1_800_000);
        assert_eq!(n.contact_name.as_str(), "Doctor");
        assert_eq!(n.contact_phone.as_str(), "222");
        // The message the module already knew how to build, hospital included.
        assert!(n.message.contains("低血糖警告"), "got {}", n.message);
        assert!(n.message.contains("St Mary"), "got {}", n.message);
    }

    #[test]
    fn a_second_critical_alert_inside_the_cooldown_is_not_re_raised() {
        let mut m = EarlyWarningSystem::new();
        m.add_emergency_contact(EmergencyContact::new("Son", "111", "family", 1))
            .unwrap();
        let first = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .unwrap();
        assert!(first.emergency_contacted);
        let (id0, ts0) = {
            let n = m.pending_emergency_notification().unwrap();
            (n.alert_id, n.timestamp)
        };

        // Ten minutes later: past the 5-minute *glucose* cooldown but well
        // inside the 30-minute emergency one.
        let second = m
            .process_glucose(
                &GlucoseData::new(45.0, -1, 2_400_000),
                &UserProfile::default(),
                2_400_000,
            )
            .unwrap();
        assert!(
            !second.emergency_contacted,
            "inside the emergency cooldown, nobody was contacted"
        );
        let n = m.pending_emergency_notification().expect("still pending");
        assert_eq!(n.alert_id, id0, "the cooldown must not re-raise");
        assert_eq!(n.timestamp, ts0);
    }

    /// A clock that steps backwards (an NTP correction, a 32-bit `u32` ms
    /// counter wrapping after ~49 days) used to underflow the cooldown
    /// subtraction — a debug panic, and in release a wrapped value that read
    /// as "well past the cooldown" and fired a duplicate alert. Either way the
    /// reading is now suppressed, which is the safe outcome for a duplicate.
    #[test]
    fn a_backwards_timestamp_neither_panics_nor_re_raises() {
        let mut m = EarlyWarningSystem::new();
        m.add_emergency_contact(EmergencyContact::new("Son", "111", "family", 1))
            .unwrap();
        let first = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .unwrap();
        assert!(first.emergency_contacted);
        let (id0, ts0) = {
            let n = m.pending_emergency_notification().unwrap();
            (n.alert_id, n.timestamp)
        };

        // The clock jumps back to the start of the epoch. No panic, no second
        // alert, and the notification already pending is left alone.
        let second = m.process_glucose(
            &GlucoseData::new(50.0, -1, 500),
            &UserProfile::default(),
            500,
        );
        assert!(second.is_none(), "a backwards clock must not re-alert");
        let n = m.pending_emergency_notification().expect("unchanged");
        assert_eq!(n.alert_id, id0);
        assert_eq!(n.timestamp, ts0);
    }

    #[test]
    fn a_non_critical_alert_raises_no_emergency() {
        let mut m = EarlyWarningSystem::new();
        // 65 mg/dL → Medium, not Critical.
        let alert = m
            .process_glucose(
                &GlucoseData::new(65.0, 0, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .expect("alert");
        assert_eq!(alert.severity, AlertSeverity::Medium);
        assert!(!alert.emergency_contacted);
        assert!(m.pending_emergency_notification().is_none());
    }

    #[test]
    fn taking_the_notification_drains_it() {
        let mut m = EarlyWarningSystem::new();
        let alert = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .unwrap();
        let taken = m.take_emergency_notification().expect("one to take");
        assert_eq!(taken.alert_id, alert.id);
        assert!(m.pending_emergency_notification().is_none());
        assert!(m.take_emergency_notification().is_none(), "only once");
    }

    /// With no contacts configured the protocol still records *what* happened
    /// — the message and the hospital — rather than dropping the alert.
    #[test]
    fn an_emergency_with_no_contacts_still_records_the_message() {
        let mut m = EarlyWarningSystem::new();
        assert!(m.emergency_contacts().is_empty());
        let alert = m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_800_000),
                &UserProfile::default(),
                1_800_000,
            )
            .unwrap();
        assert!(alert.emergency_contacted);
        let n = m.pending_emergency_notification().expect("a notification");
        assert!(n.contact_name.is_empty());
        assert!(n.contact_phone.is_empty());
        assert!(n.message.contains("低血糖警告"));
    }

    /// A device's **first** alert must not be suppressed by a cooldown that
    /// exists to rate-limit the second one. The `last_*_ms` fields start at
    /// `0`, and the cooldown test read that as "last raised at t=0" rather than
    /// "never raised" — so on a freshly booted device no glucose alert could
    /// fire in its first 5 minutes of uptime, no ECG alert in the first minute,
    /// and no emergency in the first *30*. A health monitor that cannot report
    /// the emergency at the moment it matters is the whole feature missing.
    ///
    /// The pre-existing tests all used timestamps past the cooldown
    /// (`300_001`, `400_000`, `1_800_000`) — one comment even records the
    /// workaround — so nothing ever exercised the boot window.
    #[test]
    fn the_first_glucose_alert_is_not_swallowed_by_the_cooldown() {
        let mut m = EarlyWarningSystem::new();
        // One second of uptime, and a critically low reading.
        let alert = m.process_glucose(
            &GlucoseData::new(50.0, -1, 1_000),
            &UserProfile::default(),
            1_000,
        );
        let alert = alert.expect("a critical-low reading 1 s after boot must alert");
        assert_eq!(alert.alert_type, AlertType::GlucoseLow);
        assert_eq!(alert.severity, AlertSeverity::Critical);
    }

    /// The same boot window, on the emergency path: the alert is Critical, so
    /// the protocol has to run on the first reading rather than wait out a
    /// 30-minute cooldown measured from power-on.
    #[test]
    fn the_first_emergency_is_raised_immediately_after_boot() {
        let mut m = EarlyWarningSystem::new();
        m.add_emergency_contact(EmergencyContact::new("Son", "111", "family", 1))
            .unwrap();

        let alert = m
            .process_heart_rate(&HeartRateData::new(220, 20.0, None, 1_000), 1_000)
            .expect("a 220 BPM reading 1 s after boot must alert");
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(
            alert.emergency_contacted,
            "the first emergency must reach a contact, not wait out a cooldown"
        );
        let n = m.pending_emergency_notification().expect("a notification");
        assert_eq!(n.contact_name.as_str(), "Son");
    }

    /// The cooldown still does its job: a second alert inside the window is
    /// suppressed. Without this the previous two tests would pass just as well
    /// against a system that had no cooldown at all.
    #[test]
    fn a_second_alert_inside_the_cooldown_is_still_suppressed() {
        let mut m = EarlyWarningSystem::new();
        m.add_emergency_contact(EmergencyContact::new("Son", "111", "family", 1))
            .unwrap();
        assert!(m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 1_000),
                &UserProfile::default(),
                1_000,
            )
            .is_some());
        // 10 s later, inside the 5-minute glucose cooldown.
        assert!(
            m.process_glucose(
                &GlucoseData::new(50.0, -1, 11_000),
                &UserProfile::default(),
                11_000,
            )
            .is_none(),
            "the second reading inside the cooldown must be suppressed"
        );
        // Past it, the alert returns.
        assert!(m
            .process_glucose(
                &GlucoseData::new(50.0, -1, 301_000),
                &UserProfile::default(),
                301_000,
            )
            .is_some());
    }

    #[test]
    fn ecg_assess_risk_escalates_by_anomaly_count() {
        // Two non-serious anomalies → High, *not* Critical (Critical is
        // reserved for a serious anomaly or three-or-more).
        let mut a = EcgAnalysis::new(80, HeartRhythm::Normal, 90);
        let _ = a.anomalies.push(EcgAnomaly::new(
            EcgAnomalyType::ProlongedQt,
            AlertSeverity::Medium,
            410.0,
        ));
        let _ = a.anomalies.push(EcgAnomaly::new(
            EcgAnomalyType::TWaveInversion,
            AlertSeverity::Medium,
            0.0,
        ));
        a.assess_risk();
        assert_eq!(a.risk_level, AlertSeverity::High);
        assert!(a.recommendation.as_str().contains("cardiologist"));

        // A single anomaly → Medium.
        let mut b = EcgAnalysis::new(80, HeartRhythm::Normal, 90);
        let _ = b.anomalies.push(EcgAnomaly::new(
            EcgAnomalyType::ShortPr,
            AlertSeverity::Medium,
            100.0,
        ));
        b.assess_risk();
        assert_eq!(b.risk_level, AlertSeverity::Medium);
        assert!(b.recommendation.as_str().contains("Minor"));

        // The default no-anomaly analysis stays Low.
        let c = EcgAnalysis::new(70, HeartRhythm::Normal, 95);
        assert_eq!(c.risk_level, AlertSeverity::Low);
    }

    #[test]
    fn ecg_bradycardia_and_tachycardia_below_critical_are_medium() {
        // Bradycardia with hr >= 40 → a single Medium anomaly → an overall
        // Medium risk, which the alert path classifies as a plain heart-rate
        // abnormality rather than an ECG anomaly.
        let mut m = mgr();
        let (alert, analysis) = m
            .process_ecg(&ecg(45, HeartRhythm::Bradycardia, 120_000), 120_000)
            .expect("alert");
        assert_eq!(analysis.risk_level, AlertSeverity::Medium);
        assert_eq!(analysis.rhythm, HeartRhythm::Bradycardia);
        assert_eq!(alert.alert_type, AlertType::HeartRateAbnormal);

        // Tachycardia with hr <= 180 → Medium as well.
        let mut m2 = mgr();
        let (alert2, analysis2) = m2
            .process_ecg(&ecg(150, HeartRhythm::Tachycardia, 120_000), 120_000)
            .expect("alert");
        assert_eq!(analysis2.risk_level, AlertSeverity::Medium);
        assert_eq!(alert2.alert_type, AlertType::HeartRateAbnormal);
    }

    #[test]
    fn ecg_anomaly_descriptions_cover_the_remaining_variants() {
        assert!(EcgAnomalyType::ShortPr.description().contains("PR"));
        assert!(EcgAnomalyType::StSegmentChange.description().contains("ST"));
        assert!(EcgAnomalyType::MissedBeats.description().contains("beat"));
        assert!(EcgAnomalyType::Tachycardia.description().contains("fast"));
    }

    #[test]
    fn predict_glucose_trend_selects_every_risk_band() {
        let m = mgr();
        // Mild sustained rise: predicted_60min ≈ 212 → in (200, 250] → 50.
        let moderate = vec![
            GlucoseData::new(150.0, 0, 0),
            GlucoseData::new(151.0, 0, 60_000),
            GlucoseData::new(152.0, 0, 120_000),
        ];
        let p = m.predict_glucose_trend(&moderate).expect("prediction");
        assert_eq!(p.risk_level, 50);
        assert_eq!(p.trend, 1);

        // Gentle rise kept well inside range → 20, and a flat series → trend 0.
        let calm = vec![
            GlucoseData::new(120.0, 0, 0),
            GlucoseData::new(121.0, 0, 60_000),
            GlucoseData::new(122.0, 0, 120_000),
        ];
        assert_eq!(
            m.predict_glucose_trend(&calm)
                .expect("prediction")
                .risk_level,
            20
        );

        let flat = vec![
            GlucoseData::new(110.0, 0, 0),
            GlucoseData::new(110.0, 0, 60_000),
            GlucoseData::new(110.0, 0, 120_000),
        ];
        assert_eq!(m.predict_glucose_trend(&flat).expect("prediction").trend, 0);
    }

    #[test]
    fn emergency_message_labels_a_generic_alert() {
        let m = EarlyWarningSystem::new();
        // An alert type the message formatter has no dedicated label for falls
        // back to the generic "健康警告" heading.
        let alert = HealthAlert::new(
            1,
            AlertType::BloodPressureAbnormal,
            AlertSeverity::High,
            150.0,
            130.0,
            "bp high",
            "rest",
            0,
        );
        let msg = m.generate_emergency_message(&alert);
        assert!(msg.as_str().contains("健康警告"), "got {}", msg);
        assert!(msg.as_str().contains("150.0"));
    }

    #[test]
    fn default_early_warning_system_matches_new() {
        let a = EarlyWarningSystem::default();
        assert!(a.alert_history().is_empty());
        assert!(a.emergency_contacts().is_empty());
        assert!(a.hospitals().is_empty());
    }
}
