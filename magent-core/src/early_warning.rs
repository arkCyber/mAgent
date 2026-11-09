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
    /// Last glucose trend alert, or `None` if none has been raised. `None` is
    /// *never raised*, not *raised at t=0* — see [`cooldown_elapsed`].
    last_glucose_alert_ms: Option<u32>,
    /// Cooldown between glucose alerts (ms)
    glucose_alert_cooldown_ms: u32,
    /// Last ECG alert, or `None` if none has been raised.
    last_ecg_alert_ms: Option<u32>,
    /// Cooldown between ECG alerts (ms)
    ecg_alert_cooldown_ms: u32,
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
            last_glucose_alert_ms: None,
            glucose_alert_cooldown_ms: 300_000, // 5 minutes
            last_ecg_alert_ms: None,
            ecg_alert_cooldown_ms: 60_000, // 1 minute
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

    /// Add hospital
    pub fn add_hospital(&mut self, hospital: Hospital) -> Result<()> {
        if self.hospitals.push(hospital.clone()).is_err() {
            // Remove the farthest hospital to make room (they're kept sorted by
            // distance, so the farthest is the max-distance element). Evicting
            // the farthest (not the first-farther-than-new) matches the stated
            // "keep the nearest hospitals" policy.
            if let Some(pos) = self
                .hospitals
                .iter()
                .enumerate()
                .max_by_key(|(_, h)| h.distance_m.unwrap_or(u32::MAX))
                .map(|(i, _)| i)
            {
                let _ = self.hospitals.remove(pos);
                let _ = self.hospitals.push(hospital);
            }
        }
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
            GlucoseStatus::High | GlucoseStatus::Elevated => {
                self.consecutive_high_count = self.consecutive_high_count.saturating_add(1);
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

        // Check cooldown
        if !cooldown_elapsed(
            self.last_glucose_alert_ms,
            current_time_ms,
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
        self.last_glucose_alert_ms = Some(current_time_ms);

        // Raise the emergency protocol *before* recording the alert, so the
        // history copy carries the same `emergency_contacted` flag the caller
        // receives. The cloned-then-triggered order left the recorded alert
        // permanently false — the two views of the same alert disagreed.
        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        // Add to history
        let _ = self.alert_history.push(alert.clone());

        Some(alert)
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

        // Check cooldown
        if !cooldown_elapsed(
            self.last_ecg_alert_ms,
            current_time_ms,
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

        let mut alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            analysis.risk_level,
            ecg.hr as f32,
            100.0,
            &message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id = self.next_alert_id.wrapping_add(1);
        self.last_ecg_alert_ms = Some(current_time_ms);

        if analysis.risk_level == AlertSeverity::Critical {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        let _ = self.alert_history.push(alert.clone());

        Some((alert, analysis))
    }

    /// Process heart rate data
    pub fn process_heart_rate(
        &mut self,
        hr_data: &HeartRateData,
        current_time_ms: u32,
    ) -> Option<HealthAlert> {
        // Check for dangerous heart rate
        let (alert_type, severity, message, recommendation) = if hr_data.hr > 200 {
            (
                AlertType::HeartRateAbnormal,
                AlertSeverity::Critical,
                "危险！心率严重过高。",
                "立即停止运动并就医！",
            )
        } else if hr_data.hr > 180 {
            (
                AlertType::HeartRateAbnormal,
                AlertSeverity::High,
                "心率过高警告。",
                "降低运动强度，观察心率变化。",
            )
        } else if hr_data.hr < 40 {
            (
                AlertType::HeartRateAbnormal,
                AlertSeverity::Critical,
                "危险！心率过低。",
                "立即就医检查！",
            )
        } else {
            return None;
        };

        let mut alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            severity,
            hr_data.hr as f32,
            0.0,
            message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id = self.next_alert_id.wrapping_add(1);

        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&mut alert, current_time_ms);
        }

        let _ = self.alert_history.push(alert.clone());

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
    /// ([`Self::trigger_emergency_protocol`]), because "a message that must
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
        assert_eq!(m.predict_glucose_trend(&calm).expect("prediction").risk_level, 20);

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
