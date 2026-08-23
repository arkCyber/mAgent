//! Sleep and Circadian Rhythm Manager Module for mAgent
//!
//! Provides intelligent sleep and stress management with:
//! - HRV-based stress detection (integrating with StressWatch-style data)
//! - Circadian rhythm analysis and optimization
//! - Proactive mindfulness meditation guidance when stress is high
//! - Sleep quality assessment and recommendations
//!
//! This module monitors the user's autonomic nervous system state
//! and provides timely interventions to reduce stress and improve sleep quality.

use crate::error::Result;
use crate::health_sensors::StressLevel;
use core::fmt::Write;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum entries in sleep history
pub const MAX_SLEEP_HISTORY: usize = 30; // 30 nights

/// Maximum entries in stress log
pub const MAX_STRESS_LOG: usize = 100;

/// Time of day category for circadian rhythm
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeOfDay {
    /// 5:00 – 7:00 — wake-up window.
    EarlyMorning,
    /// 7:00 – 12:00 — morning block.
    Morning,
    /// 12:00 – 17:00 — afternoon block.
    Afternoon,
    /// 17:00 – 21:00 — evening block (pre-sleep wind-down begins).
    Evening,
    /// 21:00 – 5:00 — overnight block.
    Night,
}

impl TimeOfDay {
    /// Get time of day from hour (24h format)
    pub fn from_hour(hour: u8) -> Self {
        match hour {
            5..=6 => TimeOfDay::EarlyMorning,
            7..=11 => TimeOfDay::Morning,
            12..=16 => TimeOfDay::Afternoon,
            17..=20 => TimeOfDay::Evening,
            _ => TimeOfDay::Night,
        }
    }

    /// Get name
    pub fn name(&self) -> &'static str {
        match self {
            TimeOfDay::EarlyMorning => "Early Morning",
            TimeOfDay::Morning => "Morning",
            TimeOfDay::Afternoon => "Afternoon",
            TimeOfDay::Evening => "Evening",
            TimeOfDay::Night => "Night",
        }
    }

    /// Get ideal activity level for this time
    pub fn ideal_activity(&self) -> &'static str {
        match self {
            TimeOfDay::EarlyMorning => "Light stretching, meditation",
            TimeOfDay::Morning => "High-intensity work, exercise",
            TimeOfDay::Afternoon => "Meetings, creative tasks",
            TimeOfDay::Evening => "Light activities, social",
            TimeOfDay::Night => "Relaxation, sleep preparation",
        }
    }
}

/// Circadian phase
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircadianPhase {
    /// User is asleep — lowest cortisol, melatonin peak.
    SleepPhase,
    /// First hour after waking — groggy, caffeine-sensitive.
    WakeTransition,
    /// Normal daytime alertness window.
    AlertPhase,
    /// Mid-day peak cognitive performance.
    PeakAlertness,
    /// Evening wind-down before sleep.
    WindDown,
}

impl CircadianPhase {
    /// Get phase name
    pub fn name(&self) -> &'static str {
        match self {
            CircadianPhase::SleepPhase => "Sleep Phase",
            CircadianPhase::WakeTransition => "Wake Transition",
            CircadianPhase::AlertPhase => "Alert Phase",
            CircadianPhase::PeakAlertness => "Peak Alertness",
            CircadianPhase::WindDown => "Wind Down",
        }
    }

    /// Get recommended light exposure
    pub fn light_recommendation(&self) -> &'static str {
        match self {
            CircadianPhase::SleepPhase => "Avoid light, keep environment dark",
            CircadianPhase::WakeTransition => "Get bright light exposure (10-15 min)",
            CircadianPhase::AlertPhase => "Normal indoor lighting",
            CircadianPhase::PeakAlertness => "Normal lighting, can work efficiently",
            CircadianPhase::WindDown => "Dim lights, warm tones",
        }
    }
}

/// Sleep quality metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepQuality {
    /// Total sleep duration in minutes
    pub duration_min: u32,
    /// Sleep efficiency (percentage in bed actually sleeping)
    pub efficiency: f32,
    /// Deep sleep minutes (estimated)
    pub deep_sleep_min: u16,
    /// REM sleep minutes (estimated)
    pub rem_sleep_min: u16,
    /// Wake episodes count
    pub wake_count: u8,
    /// Sleep onset latency in minutes
    pub onset_latency_min: u8,
    /// Overall score (0-100)
    pub score: u8,
}

impl SleepQuality {
    /// Create new sleep quality
    pub fn new(
        duration_min: u32,
        efficiency: f32,
        deep_sleep_min: u16,
        rem_sleep_min: u16,
        wake_count: u8,
        onset_latency_min: u8,
    ) -> Self {
        // Calculate overall score
        let duration_score = (duration_min as f32 / 480.0 * 30.0).min(30.0);
        let efficiency_score = efficiency * 0.25;
        let structure_score = ((deep_sleep_min + rem_sleep_min) as f32 / 180.0 * 25.0).min(25.0);
        let latency_score = if onset_latency_min < 15 {
            10.0
        } else {
            10.0 - onset_latency_min as f32 / 3.0
        };
        let wake_penalty = (wake_count as f32 * 2.0).min(10.0);

        let score = (duration_score + efficiency_score + structure_score + latency_score
            - wake_penalty)
            .clamp(0.0, 100.0) as u8;

        Self {
            duration_min,
            efficiency,
            deep_sleep_min,
            rem_sleep_min,
            wake_count,
            onset_latency_min,
            score,
        }
    }

    /// Get quality rating
    pub fn rating(&self) -> &'static str {
        match self.score {
            0..=40 => "Poor",
            41..=60 => "Fair",
            61..=80 => "Good",
            _ => "Excellent",
        }
    }
}

/// Sleep record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepRecord {
    /// Date (simplified as day index)
    pub day_index: u32,
    /// Sleep start time (minutes from midnight)
    pub sleep_start_min: u16,
    /// Wake time (minutes from midnight)
    pub wake_time_min: u16,
    /// Sleep quality metrics
    pub quality: SleepQuality,
    /// Average HRV during sleep
    pub sleep_hrv: f32,
    /// Resting HR during sleep
    pub sleep_hr: u16,
    /// HRV trend overnight
    pub hrv_trend: i8, // -1 decreasing, 0 stable, 1 increasing
}

impl SleepRecord {
    /// Get sleep duration in minutes
    pub fn duration_minutes(&self) -> u32 {
        if self.wake_time_min > self.sleep_start_min {
            self.wake_time_min as u32 - self.sleep_start_min as u32
        } else {
            (1440 - self.sleep_start_min as u32) + self.wake_time_min as u32
        }
    }
}

/// Mindfulness meditation session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeditationSession {
    /// Session type
    pub session_type: MeditationType,
    /// Duration in minutes
    pub duration_min: u8,
    /// Completion status
    pub completed: bool,
    /// Stress level before (0-100)
    pub stress_before: u8,
    /// Stress level after (0-100)
    pub stress_after: u8,
}

impl MeditationSession {
    /// Create new session
    pub fn new(session_type: MeditationType, duration_min: u8, stress_level: u8) -> Self {
        Self {
            session_type,
            duration_min,
            completed: false,
            stress_before: stress_level,
            stress_after: stress_level,
        }
    }

    /// Complete session
    pub fn complete(&mut self, new_stress_level: u8) {
        self.completed = true;
        self.stress_after = new_stress_level;
    }

    /// Get stress reduction
    pub fn stress_reduction(&self) -> i16 {
        self.stress_before as i16 - self.stress_after as i16
    }
}

/// Meditation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeditationType {
    /// 2-minute quick breathing (for stress popup)
    QuickBreath,
    /// 5-minute body scan
    BodyScan,
    /// 10-minute guided relaxation
    GuidedRelaxation,
    /// 15-minute deep meditation
    DeepMeditation,
    /// 20-minute sleep preparation
    SleepPrep,
    /// 2-minute emergency calm (HRV spike detection)
    EmergencyCalm,
}

impl MeditationType {
    /// Get meditation name
    pub fn name(&self) -> &'static str {
        match self {
            MeditationType::QuickBreath => "2-Minute Quick Breathing",
            MeditationType::BodyScan => "5-Minute Body Scan",
            MeditationType::GuidedRelaxation => "10-Minute Guided Relaxation",
            MeditationType::DeepMeditation => "15-Minute Deep Meditation",
            MeditationType::SleepPrep => "20-Minute Sleep Preparation",
            MeditationType::EmergencyCalm => "2-Minute Emergency Calm",
        }
    }

    /// Get description
    pub fn description(&self) -> &'static str {
        match self {
            MeditationType::QuickBreath => "A quick breathing exercise to reduce immediate stress",
            MeditationType::BodyScan => "Progressive relaxation from head to toe",
            MeditationType::GuidedRelaxation => "Guided imagery for deep relaxation",
            MeditationType::DeepMeditation => "Extended mindfulness practice for stress relief",
            MeditationType::SleepPrep => "Gentle stretching and breathing for better sleep",
            MeditationType::EmergencyCalm => {
                "Urgent breathing when HRV spikes or stress is very high"
            }
        }
    }

    /// Get recommended for stress level
    pub fn recommended_for_stress(stress: StressLevel) -> Self {
        match stress {
            StressLevel::Low => MeditationType::QuickBreath,
            StressLevel::Moderate => MeditationType::BodyScan,
            StressLevel::High => MeditationType::GuidedRelaxation,
            StressLevel::VeryHigh => MeditationType::EmergencyCalm,
        }
    }
}

/// Stress event log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressEvent {
    /// Timestamp (ms since boot)
    pub timestamp: u32,
    /// Detected stress level
    pub stress: StressLevel,
    /// Stress score (0-100)
    pub stress_score: u8,
    /// Possible triggers
    pub possible_triggers: Vec<String<32>, 4>,
    /// Whether meditation was offered
    pub meditation_offered: bool,
    /// Whether user accepted
    pub meditation_accepted: bool,
    /// Session if completed
    pub session_completed: Option<MeditationSession>,
}

impl StressEvent {
    /// Create new stress event
    pub fn new(timestamp: u32, stress: StressLevel, stress_score: u8) -> Self {
        Self {
            timestamp,
            stress,
            stress_score,
            possible_triggers: Vec::new(),
            meditation_offered: false,
            meditation_accepted: false,
            session_completed: None,
        }
    }
}

/// Sleep and Circadian Manager
pub struct SleepManager {
    /// Sleep history
    sleep_history: Vec<SleepRecord, MAX_SLEEP_HISTORY>,
    /// Stress log
    stress_log: Vec<StressEvent, MAX_STRESS_LOG>,
    /// Current circadian phase
    circadian_phase: CircadianPhase,
    /// Last stress event timestamp
    last_stress_check: u32,
    /// Stress threshold for intervention
    stress_threshold: u8,
    /// Daily meditation goal in minutes
    meditation_goal_min: u8,
    /// Today's meditation minutes
    today_meditation_min: u8,
    /// Last meditation offer time
    last_meditation_offer: u32,
    /// Cooldown between offers (ms)
    meditation_cooldown_ms: u32,
}

impl SleepManager {
    /// Create new sleep manager
    pub fn new() -> Self {
        Self {
            sleep_history: Vec::new(),
            stress_log: Vec::new(),
            circadian_phase: CircadianPhase::AlertPhase,
            last_stress_check: 0,
            stress_threshold: 70,
            meditation_goal_min: 10,
            today_meditation_min: 0,
            last_meditation_offer: 0,
            meditation_cooldown_ms: 900_000, // 15 minutes
        }
    }

    /// Set stress threshold for intervention
    pub fn set_stress_threshold(&mut self, threshold: u8) {
        self.stress_threshold = threshold.min(100);
    }

    /// Set daily meditation goal
    pub fn set_meditation_goal(&mut self, minutes: u8) {
        self.meditation_goal_min = minutes;
    }

    /// Update circadian phase based on time
    pub fn update_circadian(&mut self, current_time_min: u32) {
        let hour = ((current_time_min / 60) % 24) as u8;
        self.circadian_phase = match hour {
            0..=5 => CircadianPhase::SleepPhase,
            6..=8 => CircadianPhase::WakeTransition,
            9..=16 => CircadianPhase::AlertPhase,
            17..=19 => CircadianPhase::PeakAlertness,
            _ => CircadianPhase::WindDown,
        };
    }

    /// Get current circadian phase
    pub fn circadian_phase(&self) -> CircadianPhase {
        self.circadian_phase
    }

    /// Add sleep record
    pub fn add_sleep_record(&mut self, record: SleepRecord) -> Result<()> {
        if self.sleep_history.push(record.clone()).is_err() {
            let _ = self.sleep_history.remove(0);
            let _ = self.sleep_history.push(record);
        }
        Ok(())
    }

    /// Get sleep history
    pub fn sleep_history(&self) -> &[SleepRecord] {
        &self.sleep_history
    }

    /// Get latest sleep record
    pub fn latest_sleep(&self) -> Option<&SleepRecord> {
        self.sleep_history.last()
    }

    /// Calculate average sleep quality over last N nights
    pub fn average_sleep_score(&self, nights: usize) -> Option<f32> {
        let count = nights.min(self.sleep_history.len());
        if count == 0 {
            return None;
        }
        let sum: u32 = self
            .sleep_history
            .iter()
            .rev()
            .take(count)
            .map(|r| r.quality.score as u32)
            .sum();
        Some(sum as f32 / count as f32)
    }

    /// Calculate sleep debt (hours below 8h target)
    pub fn calculate_sleep_debt(&self, days: usize) -> f32 {
        let target_hours = 8.0 * days as f32;
        let actual_hours: f32 = self
            .sleep_history
            .iter()
            .rev()
            .take(days)
            .map(|r| r.duration_minutes() as f32 / 60.0)
            .sum();
        (target_hours - actual_hours).max(0.0)
    }

    /// Check stress and determine if intervention needed
    pub fn check_stress_intervention(
        &mut self,
        hrv: f32,
        hr: u16,
        current_time_ms: u32,
    ) -> Option<InterventionRecommendation> {
        let stress = StressLevel::from_hrv(hrv);
        let stress_score = self.calculate_stress_score(hrv, hr);

        // Log stress event
        let mut event = StressEvent::new(current_time_ms, stress, stress_score);
        self.last_stress_check = current_time_ms;

        // Check if intervention needed
        if stress_score >= self.stress_threshold
            && (current_time_ms - self.last_meditation_offer) > self.meditation_cooldown_ms
        {
            self.last_meditation_offer = current_time_ms;
            event.meditation_offered = true;

            let recommended_session = MeditationType::recommended_for_stress(stress);

            let recommendation = InterventionRecommendation {
                intervention_type: InterventionType::MeditationOffer,
                session_type: recommended_session,
                message: self.generate_stress_message(stress, stress_score),
                priority: self.calculate_priority(stress_score),
                stress_score,
            };

            let _ = self.stress_log.push(event);

            return Some(recommendation);
        }

        let _ = self.stress_log.push(event);
        None
    }

    /// Calculate stress score (0-100)
    fn calculate_stress_score(&self, hrv: f32, hr: u16) -> u8 {
        // Normalize HRV to 0-100 (lower HRV = higher stress)
        let hrv_score = ((80.0 - hrv) / 80.0 * 50.0).clamp(0.0, 50.0) as u8;

        // Add HR contribution (higher HR = higher stress)
        let hr_contribution = if hr > 90 {
            ((hr - 90) as f32 * 0.5) as u8
        } else {
            0
        };

        (hrv_score + hr_contribution).min(100)
    }

    /// Generate appropriate stress message
    fn generate_stress_message(&self, stress: StressLevel, _score: u8) -> &'static str {
        match stress {
            StressLevel::VeryHigh => "检测到压力过高，建议进行2分钟正念冥想放松。",
            StressLevel::High => "当前压力较大，花几分钟做一下呼吸练习。",
            StressLevel::Moderate => "检测到轻微压力，可以考虑短暂休息。",
            StressLevel::Low => "压力水平正常，继续保持。",
        }
    }

    /// Calculate intervention priority
    fn calculate_priority(&self, stress_score: u8) -> u8 {
        match stress_score {
            0..=50 => 3,
            51..=70 => 5,
            71..=85 => 7,
            _ => 9,
        }
    }

    /// Record meditation completion
    pub fn record_meditation(&mut self, session: MeditationSession) -> Result<()> {
        self.today_meditation_min += session.duration_min;

        // Update the most recent stress event with meditation info
        if let Some(event) = self.stress_log.last_mut() {
            event.meditation_accepted = true;
            event.session_completed = Some(session);
        }

        Ok(())
    }

    /// Get stress log
    pub fn stress_log(&self) -> &[StressEvent] {
        &self.stress_log
    }

    /// Get today's meditation progress
    pub fn today_meditation_progress(&self) -> (u8, u8) {
        (self.today_meditation_min, self.meditation_goal_min)
    }

    /// Reset daily meditation counter (call at midnight)
    pub fn reset_daily(&mut self) {
        self.today_meditation_min = 0;
    }

    /// Get sleep recommendations
    pub fn get_sleep_recommendations(&self) -> Vec<String<128>, 8> {
        let mut recommendations = Vec::new();

        // Based on sleep debt
        let debt = self.calculate_sleep_debt(7);
        if debt > 2.0 {
            let mut s: String<128> = String::new();
            let _ = write!(s, "本周睡眠债务约{:.1}小时，建议增加睡眠时间", debt);
            let _ = recommendations.push(s);
        }

        // Based on recent sleep quality
        if let Some(avg_score) = self.average_sleep_score(5) {
            if avg_score < 60.0 {
                let _ = recommendations
                    .push(String::try_from("最近睡眠质量较差，建议睡前减少屏幕使用").unwrap());
            }
        }

        // Based on circadian phase
        match self.circadian_phase {
            CircadianPhase::WindDown => {
                let _ = recommendations.push(
                    String::try_from("现在是睡眠准备阶段，建议调暗灯光，进行放松活动").unwrap(),
                );
            }
            CircadianPhase::AlertPhase => {
                let _ = recommendations
                    .push(String::try_from("现在是警觉期，适合处理复杂任务").unwrap());
            }
            _ => {}
        }

        // Default recommendations
        if recommendations.is_empty() {
            let _ = recommendations
                .push(String::try_from("保持良好的睡眠习惯：固定作息，适度运动").unwrap());
        }

        recommendations
    }

    /// Get mindfulness meditation script for session type
    pub fn get_meditation_script(session_type: MeditationType) -> &'static str {
        match session_type {
            MeditationType::EmergencyCalm => {
                r#"请找一个舒适的位置坐好。深深地吸一口气，屏住呼吸，然后缓缓呼出。重复三次。现在，开始4-7-8呼吸：吸气4秒，屏气7秒，呼气8秒。专注于呼气时的放松感。继续这个节奏..."#
            }
            MeditationType::QuickBreath => {
                r#"请坐直，放松肩膀。深吸一口气，慢慢数到4。屏住呼吸，数到4。然后缓缓呼气，数到4。重复5次。每次呼气时，放松身体的紧张感。继续保持深呼吸..."#
            }
            MeditationType::BodyScan => {
                r#"找一个舒适的位置躺下。开始关注你的脚趾，感受它们的温度和触感。慢慢向上移动注意力，感受小腿的放松...膝盖...大腿...继续向上，感受腹部的起伏...胸部...肩膀...手臂...最后是面部和头顶。每次呼气时，让身体的一部分更加放松..."#
            }
            MeditationType::GuidedRelaxation => {
                r#"请找一个安静的地方坐下或躺下。闭上眼睛，开始深呼吸。想象自己站在温暖的海边，阳光洒在身上...感受脚下的细软沙滩...海风轻轻吹过...听到海浪的声音...每一次呼吸，都带走一点紧张和压力...现在，感觉自己充满平静和放松..."#
            }
            MeditationType::DeepMeditation => {
                r#"静坐，保持舒适的姿势。轻轻闭上眼睛。将注意力放在呼吸上，不需要控制它，只是观察。每次呼气，让杂念像云一样飘走。如果发现注意力分散，温柔地把注意力带回到呼吸上。继续这样练习..."#
            }
            MeditationType::SleepPrep => {
                r#"躺在床上，做几次深呼吸放松。开始从脚趾开始，轻轻拉伸并放松每一块肌肉。感受放松从脚趾向上蔓延...小腿...大腿...臀部...腹部...胸部...手臂...肩膀...颈部...面部。现在，感受呼吸缓慢而深沉，让睡意自然到来..."#
            }
        }
    }
}

impl Default for SleepManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Intervention recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionRecommendation {
    /// Type of intervention
    pub intervention_type: InterventionType,
    /// Specific session type if meditation
    pub session_type: MeditationType,
    /// Message to display
    pub message: &'static str,
    /// Priority (1-10)
    pub priority: u8,
    /// Current stress score
    pub stress_score: u8,
}

/// Intervention type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterventionType {
    /// Offer mindfulness meditation
    MeditationOffer,
    /// Suggest sleep
    SleepSuggestion,
    /// Light exposure reminder
    LightExposure,
    /// Emergency calm (HRV spike)
    EmergencyCalm,
}
