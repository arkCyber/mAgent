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

use crate::error::{AgentError, Result};
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
        Self {
            id,
            alert_type,
            severity,
            current_value,
            threshold,
            message: String::try_from(message).unwrap(),
            recommendation: String::try_from(recommendation).unwrap(),
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
        Self {
            name: String::try_from(name).unwrap(),
            phone: String::try_from(phone).unwrap(),
            relationship: String::try_from(relationship).unwrap(),
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
        Self {
            name: String::try_from(name).unwrap(),
            address: String::try_from(address).unwrap(),
            phone: String::try_from(phone).unwrap(),
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
                write!(s, "{}m", dist).unwrap();
                s
            } else {
                let mut s: String<32> = String::new();
                write!(s, "{:.1}km", dist as f32 / 1000.0).unwrap();
                s
            }
        } else {
            let mut s: String<32> = String::new();
            write!(s, "Distance unknown").unwrap();
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
            ).unwrap();
        } else if anomaly_count >= 2 {
            self.risk_level = AlertSeverity::High;
            self.recommendation = String::try_from(
                "Multiple cardiac irregularities detected. Consult a cardiologist.",
            ).unwrap();
        } else if anomaly_count >= 1 {
            self.risk_level = AlertSeverity::Medium;
            self.recommendation = String::try_from(
                "Minor cardiac irregularity detected. Monitor and consider medical consultation.",
            ).unwrap();
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
    /// Last glucose trend alert
    last_glucose_alert_ms: u32,
    /// Cooldown between glucose alerts (ms)
    glucose_alert_cooldown_ms: u32,
    /// Last ECG alert
    last_ecg_alert_ms: u32,
    /// Cooldown between ECG alerts (ms)
    ecg_alert_cooldown_ms: u32,
    /// Consecutive low glucose count
    consecutive_low_count: u8,
    /// Consecutive high glucose count
    consecutive_high_count: u8,
    /// Last notification time for emergency
    last_emergency_notification_ms: u32,
    /// Cooldown for emergency notifications (ms) - 30 minutes
    emergency_cooldown_ms: u32,
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
            last_glucose_alert_ms: 0,
            glucose_alert_cooldown_ms: 300_000, // 5 minutes
            last_ecg_alert_ms: 0,
            ecg_alert_cooldown_ms: 60_000, // 1 minute
            consecutive_low_count: 0,
            consecutive_high_count: 0,
            last_emergency_notification_ms: 0,
            emergency_cooldown_ms: 1_800_000, // 30 minutes
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
            // Remove farthest if full
            if let Some(pos) = self.hospitals.iter().position(|h| {
                h.distance_m.unwrap_or(u32::MAX) > hospital.distance_m.unwrap_or(u32::MAX)
            }) {
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
                self.consecutive_low_count += 1;
                self.consecutive_high_count = 0;
            }
            GlucoseStatus::High | GlucoseStatus::Elevated => {
                self.consecutive_high_count += 1;
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
        if current_time_ms - self.last_glucose_alert_ms < self.glucose_alert_cooldown_ms {
            return None;
        }

        // Create alert
        let alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            severity,
            glucose.glucose,
            threshold,
            message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id += 1;
        self.last_glucose_alert_ms = current_time_ms;

        // Add to history
        let _ = self.alert_history.push(alert.clone());

        // Trigger emergency protocol for critical alerts
        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&alert, current_time_ms);
        }

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
        if current_time_ms - self.last_ecg_alert_ms < self.ecg_alert_cooldown_ms {
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

        let alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            analysis.risk_level,
            ecg.hr as f32,
            100.0,
            &message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id += 1;
        self.last_ecg_alert_ms = current_time_ms;

        let _ = self.alert_history.push(alert.clone());

        if analysis.risk_level == AlertSeverity::Critical {
            self.trigger_emergency_protocol(&alert, current_time_ms);
        }

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

        let alert = HealthAlert::new(
            self.next_alert_id,
            alert_type,
            severity,
            hr_data.hr as f32,
            0.0,
            message,
            recommendation,
            current_time_ms,
        );

        self.next_alert_id += 1;

        let _ = self.alert_history.push(alert.clone());

        if severity.requires_emergency() {
            self.trigger_emergency_protocol(&alert, current_time_ms);
        }

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

    /// Trigger emergency protocol
    fn trigger_emergency_protocol(&mut self, _alert: &HealthAlert, current_time_ms: u32) {
        // Check cooldown
        if current_time_ms - self.last_emergency_notification_ms < self.emergency_cooldown_ms {
            return;
        }

        self.last_emergency_notification_ms = current_time_ms;

        // In real implementation, this would:
        // 1. Send SMS to emergency contacts
        // 2. Call emergency services if needed
        // 3. Provide location to emergency responders
        // 4. Send alert to cloud monitoring service

        // Mark alert as emergency contacted
        // (In real implementation, this would be updated after actual contact)
    }

    /// Acknowledge alert
    pub fn acknowledge_alert(&mut self, alert_id: u32) -> bool {
        if let Some(alert) = self.alert_history.iter_mut().find(|a| a.id == alert_id) {
            alert.acknowledge();
            return true;
        }
        false
    }

    /// Get unacknowledged alerts
    pub fn unacknowledged_alerts(&self) -> Vec<&HealthAlert, 16> {
        self.alert_history
            .iter()
            .filter(|a| !a.acknowledged)
            .collect()
    }

    /// Get alert history
    pub fn alert_history(&self) -> &[HealthAlert] {
        &self.alert_history
    }

    /// Get critical alerts
    pub fn critical_alerts(&self) -> Vec<&HealthAlert, 8> {
        self.alert_history
            .iter()
            .filter(|a| a.severity >= AlertSeverity::High && !a.acknowledged)
            .collect()
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

// Implement Write for String
impl core::fmt::Write for EarlyWarningSystem {
    fn write_str(&mut self, _s: &str) -> core::fmt::Result {
        Ok(())
    }
}
