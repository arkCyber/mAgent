//! Intelligent Sports Coach Module for mAgent
//!
//! Provides AI-powered exercise coaching with:
//! - Adaptive daily exercise goal adjustment
//! - Real-time breathing rhythm correction via voice
//! - Environmental factor consideration (temperature, humidity)
//! - Historical fitness data analysis
//!
//! The coach monitors exercise state and provides real-time guidance
//! to optimize workout effectiveness and safety.

use crate::error::try_heapless;
use crate::health_sensors::{HealthSensorManager, HeartRateData, HeartRateZone, TemperatureData};
use core::fmt::Write;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Exercise type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExerciseType {
    /// Steady-state outdoor or treadmill running.
    Running,
    /// Casual walking — lowest intensity activity class.
    Walking,
    /// Indoor or outdoor cycling.
    Cycling,
    /// Lap swimming in a pool.
    Swimming,
    /// Resistance / weights workout.
    Strength,
    /// Yoga — primarily flexibility and breath work.
    Yoga,
    /// High-intensity interval training (alternating bursts and rest).
    Hiit,
    /// Active recovery or rest day — no structured workout.
    Rest,
}

impl ExerciseType {
    /// Get default target duration in minutes
    pub fn default_duration(&self) -> u16 {
        match self {
            ExerciseType::Running => 30,
            ExerciseType::Walking => 45,
            ExerciseType::Cycling => 40,
            ExerciseType::Swimming => 30,
            ExerciseType::Strength => 45,
            ExerciseType::Yoga => 30,
            ExerciseType::Hiit => 20,
            ExerciseType::Rest => 0,
        }
    }

    /// Get default target intensity (percentage of max HR)
    pub fn default_intensity(&self) -> f32 {
        match self {
            ExerciseType::Running => 0.70,
            ExerciseType::Walking => 0.50,
            ExerciseType::Cycling => 0.65,
            ExerciseType::Swimming => 0.60,
            ExerciseType::Strength => 0.50,
            ExerciseType::Yoga => 0.30,
            ExerciseType::Hiit => 0.85,
            ExerciseType::Rest => 0.0,
        }
    }
}

/// Exercise goal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseGoal {
    /// Exercise type
    pub exercise_type: ExerciseType,
    /// Target duration in minutes
    pub duration_min: u16,
    /// Target distance in meters (for running/cycling)
    pub distance_m: Option<u32>,
    /// Target calories
    pub calories: Option<u16>,
    /// Target average heart rate zone
    pub target_zone: HeartRateZone,
    /// Target intensity (percentage of max HR)
    pub target_intensity: f32,
    /// Smart adjustments applied
    pub adjustments: Vec<String<64>, 8>,
}

impl Default for ExerciseGoal {
    fn default() -> Self {
        Self {
            exercise_type: ExerciseType::Running,
            duration_min: 30,
            distance_m: Some(3000),
            calories: Some(250),
            target_zone: HeartRateZone::FatBurn,
            target_intensity: 0.65,
            adjustments: Vec::new(),
        }
    }
}

impl ExerciseGoal {
    /// Create a new exercise goal
    pub fn new(exercise_type: ExerciseType) -> Self {
        Self {
            exercise_type,
            duration_min: exercise_type.default_duration(),
            distance_m: None,
            calories: None,
            target_zone: HeartRateZone::FatBurn,
            target_intensity: exercise_type.default_intensity(),
            adjustments: Vec::new(),
        }
    }

    /// Add an adjustment reason
    // HARDENING (audit-2026-08 unwrap sweep): use `try_heapless` to
    // prevent panic when a reason string exceeds the capacity.
    pub fn add_adjustment(&mut self, reason: &str) {
        let truncated = try_heapless::<64>(reason);
        if self.adjustments.push(truncated).is_err() {
            let _ = self.adjustments.remove(0);
            let _ = self.adjustments.push(try_heapless::<64>(reason));
        }
    }

    /// Format goal as readable string
    pub fn format_summary(&self) -> String<256> {
        let mut result = String::new();
        let _ = write!(
            result,
            "{} for {} minutes",
            self.exercise_type.name(),
            self.duration_min
        );
        if let Some(dist) = self.distance_m {
            let _ = write!(result, ", distance {}m", dist);
        }
        if let Some(cal) = self.calories {
            let _ = write!(result, ", burn {} cal", cal);
        }
        let _ = write!(result, ", zone: {}", self.target_zone.name());
        if !self.adjustments.is_empty() {
            let _ = write!(result, " (adjusted)");
        }
        result
    }
}

impl core::fmt::Write for ExerciseGoal {
    fn write_str(&mut self, _s: &str) -> core::fmt::Result {
        Ok(())
    }
}

impl ExerciseType {
    /// Get exercise name
    pub fn name(&self) -> &'static str {
        match self {
            ExerciseType::Running => "Running",
            ExerciseType::Walking => "Walking",
            ExerciseType::Cycling => "Cycling",
            ExerciseType::Swimming => "Swimming",
            ExerciseType::Strength => "Strength Training",
            ExerciseType::Yoga => "Yoga",
            ExerciseType::Hiit => "HIIT",
            ExerciseType::Rest => "Rest Day",
        }
    }
}

/// Breathing pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreathingPattern {
    /// 2:2 pattern (2s inhale, 2s exhale) - for calm
    BoxBreathing,
    /// 3:3 pattern - moderate
    Relaxed,
    /// 4:4 pattern - aerobic warm-up
    AerobicInhale,
    /// 4:6 pattern - endurance
    Endurance,
    /// 2:4 pattern - recovery
    Recovery,
    /// Synchronized with running cadence
    RunningSync,
}

impl BreathingPattern {
    /// Get pattern description
    pub fn description(&self) -> &'static str {
        match self {
            BreathingPattern::BoxBreathing => "4-4-4-4 Box Breathing",
            BreathingPattern::Relaxed => "3-3 Relaxed Breathing",
            BreathingPattern::AerobicInhale => "4-4 Aerobic Breathing",
            BreathingPattern::Endurance => "4-6 Extended Exhale",
            BreathingPattern::Recovery => "2-4 Recovery Breathing",
            BreathingPattern::RunningSync => "2-2 Running Sync (2in-2out)",
        }
    }

    /// Get recommended pattern for heart rate zone
    pub fn for_zone(zone: HeartRateZone) -> Self {
        match zone {
            HeartRateZone::Rest => BreathingPattern::BoxBreathing,
            HeartRateZone::WarmUp => BreathingPattern::Relaxed,
            HeartRateZone::FatBurn => BreathingPattern::AerobicInhale,
            HeartRateZone::Cardio => BreathingPattern::RunningSync,
            HeartRateZone::Peak => BreathingPattern::Recovery,
            HeartRateZone::Danger => BreathingPattern::Recovery,
        }
    }
}

/// Voice coaching message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoachingMessage {
    /// Message type
    pub msg_type: CoachingMessageType,
    /// Voice text to speak
    pub voice_text: String<128>,
    /// Priority level
    pub priority: u8,
}

impl CoachingMessage {
    /// Create new coaching message
    pub fn new(msg_type: CoachingMessageType, voice_text: &str, priority: u8) -> Self {
        // HARDENING (audit-2026-08 unwrap sweep): coaching messages from LLM
        // can be arbitrarily long. Truncate to 512 bytes to keep the
        // coaching pipeline panic-free.
        Self {
            msg_type,
            voice_text: try_heapless::<128>(voice_text),
            priority,
        }
    }
}

/// Coaching message category — what kind of cue the agent is delivering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoachingMessageType {
    /// Reminder to slow the breath / match inhale-exhale cadence.
    BreathingCorrection,
    /// Suggestion to speed up or slow down the current pace.
    PaceAdjustment,
    /// Positive reinforcement ("you're crushing it!").
    Encouragement,
    /// Safety or form warning — read out immediately regardless of priority order.
    Warning,
    /// Goal for the current session was just hit.
    GoalComplete,
    /// Beginning of a workout — boot the session in the listener.
    StartExercise,
    /// End of a workout — close out the session.
    EndExercise,
}

/// Exercise session state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExerciseState {
    /// No session is active.
    Idle,
    /// Warm-up phase — easy effort to raise heart rate.
    WarmUp,
    /// Main work interval(s).
    Active,
    /// Cool-down phase — gradually reducing effort.
    CoolDown,
    /// Session completed normally.
    Finished,
}

/// Fitness history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitnessHistoryEntry {
    /// Exercise type performed
    pub exercise_type: ExerciseType,
    /// Duration in minutes
    pub duration_min: u16,
    /// Average heart rate
    pub avg_hr: u16,
    /// Calories burned
    pub calories: u16,
    /// User rating (1-5)
    pub rating: u8,
    /// Felt exertion (RPE 1-10)
    pub rpe: u8,
    /// Timestamp
    pub timestamp: u32,
}

impl FitnessHistoryEntry {
    /// Calculate fitness score from this entry
    pub fn fitness_score(&self) -> f32 {
        // Simple scoring: duration + effort
        let duration_score = self.duration_min as f32 * 2.0;
        let effort_score = self.rpe as f32 * 10.0;
        let hr_score = if self.avg_hr > 100 { 20.0 } else { 0.0 };
        (duration_score + effort_score + hr_score) / 10.0
    }
}

/// Intelligent Sports Coach
pub struct SportsCoach {
    /// Current exercise goal
    goal: ExerciseGoal,
    /// Current exercise state
    state: ExerciseState,
    /// Current exercise session duration in seconds
    session_duration_s: u32,
    /// Breathing pattern
    breathing_pattern: BreathingPattern,
    /// Pending coaching messages
    messages: Vec<CoachingMessage, 16>,
    /// Last breathing correction time
    last_breathing_correction_s: u32,
    /// Last encouragement time
    last_encouragement_s: u32,
    /// Consecutive high HR corrections
    high_hr_streak: u8,
}

impl SportsCoach {
    /// Create new sports coach
    pub fn new() -> Self {
        Self {
            goal: ExerciseGoal::default(),
            state: ExerciseState::Idle,
            session_duration_s: 0,
            breathing_pattern: BreathingPattern::Relaxed,
            messages: Vec::new(),
            last_breathing_correction_s: 0,
            last_encouragement_s: 0,
            high_hr_streak: 0,
        }
    }

    /// Get current goal
    pub fn goal(&self) -> &ExerciseGoal {
        &self.goal
    }

    /// Get current state
    pub fn state(&self) -> ExerciseState {
        self.state
    }

    /// Get breathing pattern
    pub fn breathing_pattern(&self) -> BreathingPattern {
        self.breathing_pattern
    }

    /// Set exercise goal
    pub fn set_goal(&mut self, goal: ExerciseGoal) {
        self.goal = goal;
    }

    /// Start exercise session
    pub fn start_session(&mut self) {
        self.state = ExerciseState::WarmUp;
        self.session_duration_s = 0;
        self.high_hr_streak = 0;

        let msg = CoachingMessage::new(
            CoachingMessageType::StartExercise,
            "运动开始。保持均匀呼吸，专注于今天的训练目标。",
            10,
        );
        let _ = self.messages.push(msg);
    }

    /// End exercise session
    pub fn end_session(&mut self) {
        self.state = ExerciseState::Finished;

        let msg = CoachingMessage::new(
            CoachingMessageType::EndExercise,
            "运动结束。辛苦了，做一下拉伸放松。",
            10,
        );
        let _ = self.messages.push(msg);
    }

    /// Adjust goal based on environment and health data
    pub fn adjust_goal_for_conditions(
        &mut self,
        health_mgr: &HealthSensorManager,
        env_temp_c: f32,
    ) {
        let mut adjusted = false;

        // Temperature adjustment
        if env_temp_c > 30.0 {
            // Reduce intensity in hot weather
            if self.goal.target_intensity > 0.5 {
                self.goal.target_intensity *= 0.85;
                self.goal.add_adjustment("Hot weather - reduced intensity");
                adjusted = true;
            }
        } else if env_temp_c < 5.0 {
            // Extend warm-up in cold weather
            self.goal.add_adjustment("Cold weather - extended warm-up");
            adjusted = true;
        }

        // Health-based adjustment
        if let Some(avg_hrv) = health_mgr.average_hrv(10) {
            if avg_hrv < 30.0 {
                // Low HRV indicates fatigue - reduce intensity
                self.goal.target_intensity *= 0.8;
                self.goal
                    .add_adjustment("Low HRV - reduced intensity for recovery");
                adjusted = true;
            }
        }

        // Recent exercise adjustment (prevent overtraining)
        let recent_hr = health_mgr.latest_heart_rate();
        if let Some(hr_data) = recent_hr {
            if hr_data.hr > 150 && hr_data.zone() == HeartRateZone::Cardio {
                self.high_hr_streak += 1;
                if self.high_hr_streak > 3 {
                    self.goal.target_intensity *= 0.9;
                    self.goal
                        .add_adjustment("High HR sustained - reduced intensity");
                    self.add_warning("心率持续偏高，建议降低运动强度。");
                    adjusted = true;
                }
            } else {
                self.high_hr_streak = 0;
            }
        }

        if adjusted {
            // Clamp intensity to valid range
            self.goal.target_intensity = self.goal.target_intensity.clamp(0.3, 0.95);
        }
    }

    /// Calculate personalized goal based on fitness history
    pub fn calculate_adaptive_goal(
        &self,
        fitness_history: &[FitnessHistoryEntry],
        health_mgr: &HealthSensorManager,
        days_active: u8,
    ) -> ExerciseGoal {
        let mut goal = ExerciseGoal::new(self.goal.exercise_type);

        if fitness_history.is_empty() {
            return goal; // Use defaults for new users
        }

        // Calculate average performance from recent history. Use a heapless::Vec
        // with a fixed capacity so this works in both std and no_std; 7
        // entries matches the `.take(7)` below.
        let mut recent_entries: Vec<&FitnessHistoryEntry, 7> = Vec::new();
        for e in fitness_history.iter().rev().take(7) {
            let _ = recent_entries.push(e);
        }
        if recent_entries.is_empty() {
            return goal;
        }

        let avg_duration: f32 = recent_entries
            .iter()
            .map(|e| e.duration_min as f32)
            .sum::<f32>()
            / recent_entries.len() as f32;
        let avg_hr: f32 = recent_entries.iter().map(|e| e.avg_hr as f32).sum::<f32>()
            / recent_entries.len() as f32;
        let avg_rpe: f32 =
            recent_entries.iter().map(|e| e.rpe as f32).sum::<f32>() / recent_entries.len() as f32;

        // Adjust based on recent average performance
        if avg_rpe < 6.0 && days_active > 3 {
            // User is recovering well - can increase slightly
            goal.duration_min = (avg_duration * 1.1) as u16;
            goal.add_adjustment("Good recovery - slight increase");
        } else if avg_rpe > 8.0 {
            // User is fatigued - reduce
            goal.duration_min = (avg_duration * 0.85) as u16;
            goal.add_adjustment("High fatigue - reduced duration");
        }

        // Set target zone based on average HR
        goal.target_zone = HeartRateZone::from_hr_and_age(avg_hr as u16, health_mgr.profile().age);

        // Set target intensity
        goal.target_intensity = self.goal.target_intensity;

        goal
    }

    /// Update coach with current sensor data
    pub fn update(
        &mut self,
        hr_data: &HeartRateData,
        temp_data: Option<&TemperatureData>,
        delta_s: u32,
    ) {
        self.session_duration_s += delta_s;

        let zone = hr_data.zone();

        // Update state based on session progress
        match self.state {
            ExerciseState::WarmUp => {
                if self.session_duration_s > 300 {
                    // 5 minutes warm-up
                    self.state = ExerciseState::Active;
                }
            }
            ExerciseState::Active => {
                let total_duration_min = self.goal.duration_min as u32;
                if self.session_duration_s > (total_duration_min * 60) {
                    self.state = ExerciseState::CoolDown;
                }
            }
            ExerciseState::CoolDown => {
                // Keep the explicit `if` block for clarity despite clippy's
                // collapsible_match suggestion; the guard reads better this way.
                #[allow(clippy::collapsible_match)]
                if self.session_duration_s > (self.goal.duration_min as u32 + 5) * 60 {
                    self.end_session();
                }
            }
            _ => {}
        }

        // Breathing pattern updates
        let target_pattern = BreathingPattern::for_zone(zone);
        if target_pattern != self.breathing_pattern && self.state == ExerciseState::Active {
            self.breathing_pattern = target_pattern;
            let msg = CoachingMessage::new(
                CoachingMessageType::BreathingCorrection,
                self.breathing_pattern.description(),
                5,
            );
            let _ = self.messages.push(msg);
        }

        // Real-time breathing correction when running too fast
        if (zone == HeartRateZone::Peak || zone == HeartRateZone::Danger)
            && self.session_duration_s - self.last_breathing_correction_s > 30
        {
            self.last_breathing_correction_s = self.session_duration_s;
            let correction = match self.breathing_pattern {
                BreathingPattern::RunningSync => "放慢脚步，深呼吸，用鼻子吸气，嘴巴呼气。",
                BreathingPattern::Recovery => "立即降低强度，深呼吸放松。",
                _ => "放松呼吸，放慢速度。",
            };
            let msg = CoachingMessage::new(CoachingMessageType::BreathingCorrection, correction, 8);
            let _ = self.messages.push(msg);
        }

        // Encouragement at regular intervals
        if self.session_duration_s - self.last_encouragement_s > 180 {
            self.last_encouragement_s = self.session_duration_s;
            let encouragement = self.generate_encouragement(zone);
            let msg = CoachingMessage::new(CoachingMessageType::Encouragement, encouragement, 3);
            let _ = self.messages.push(msg);
        }

        // Warning for danger zone
        if zone == HeartRateZone::Danger {
            let msg = CoachingMessage::new(
                CoachingMessageType::Warning,
                "警告！心率过高，请立即停止运动并休息。",
                10,
            );
            let _ = self.messages.push(msg);
        }

        // Environment-based warning
        if let Some(temp) = temp_data {
            if temp.has_fever() {
                let msg = CoachingMessage::new(
                    CoachingMessageType::Warning,
                    "体温偏高，建议降低运动强度或暂停运动。",
                    9,
                );
                let _ = self.messages.push(msg);
            }
        }
    }

    /// Generate encouragement message
    fn generate_encouragement(&self, zone: HeartRateZone) -> &'static str {
        match zone {
            HeartRateZone::WarmUp => "状态不错，继续保持。",
            HeartRateZone::FatBurn => "脂肪燃烧区，加油！",
            HeartRateZone::Cardio => "心肺训练中，你很棒！",
            HeartRateZone::Peak => "接近目标了，保持节奏！",
            _ => "坚持就是胜利！",
        }
    }

    /// Add a warning message
    pub fn add_warning(&mut self, message: &str) {
        let msg = CoachingMessage::new(CoachingMessageType::Warning, message, 8);
        let _ = self.messages.push(msg);
    }

    /// Get pending coaching messages
    pub fn get_messages(&mut self) -> Vec<CoachingMessage, 16> {
        let msgs = self.messages.clone();
        self.messages.clear();
        msgs
    }

    /// Get session duration in seconds
    pub fn session_duration(&self) -> u32 {
        self.session_duration_s
    }

    /// Get session progress as percentage
    pub fn session_progress(&self) -> f32 {
        let total = (self.goal.duration_min as u32) * 60;
        if total == 0 {
            return 0.0;
        }
        (self.session_duration_s as f32 / total as f32 * 100.0).min(100.0)
    }
}

impl Default for SportsCoach {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_sensors::{HealthSensorManager, TemperatureData};

    fn hr_data(hr: u16, ts: u32) -> HeartRateData {
        HeartRateData {
            hr,
            hrv: 50.0,
            spo2: Some(98),
            timestamp: ts,
        }
    }

    #[test]
    fn exercise_type_defaults() {
        assert_eq!(ExerciseType::Running.default_duration(), 30);
        assert_eq!(ExerciseType::Walking.default_duration(), 45);
        assert_eq!(ExerciseType::Rest.default_duration(), 0);
        assert_eq!(ExerciseType::Hiit.default_intensity(), 0.85);
        assert_eq!(ExerciseType::Yoga.default_intensity(), 0.30);
        assert!(!ExerciseType::Strength.name().is_empty());
    }

    #[test]
    fn exercise_goal_new_and_format() {
        let goal = ExerciseGoal::new(ExerciseType::Running);
        assert_eq!(goal.duration_min, 30);
        assert_eq!(goal.target_intensity, 0.70);
        assert!(goal.distance_m.is_none());
        assert!(goal.adjustments.is_empty());
        assert!(goal
            .format_summary()
            .as_str()
            .contains("Running for 30 minutes"));
    }

    #[test]
    fn exercise_goal_add_adjustment_evicts_oldest() {
        let mut goal = ExerciseGoal::new(ExerciseType::Running);
        for i in 0..10 {
            goal.add_adjustment(&format!("adj {}", i));
        }
        // Capacity is 8; the two oldest are evicted.
        assert_eq!(goal.adjustments.len(), 8);
        assert!(goal.adjustments[7].contains("adj 9"));
        assert!(!goal
            .adjustments
            .iter()
            .any(|a| a.contains("adj 0") || a.contains("adj 1")));
        // Long reasons are truncated, never panic.
        goal.add_adjustment(&"x".repeat(200));
    }

    #[test]
    fn breathing_pattern_for_zone() {
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::Rest),
            BreathingPattern::BoxBreathing
        );
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::WarmUp),
            BreathingPattern::Relaxed
        );
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::FatBurn),
            BreathingPattern::AerobicInhale
        );
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::Cardio),
            BreathingPattern::RunningSync
        );
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::Peak),
            BreathingPattern::Recovery
        );
        assert_eq!(
            BreathingPattern::for_zone(HeartRateZone::Danger),
            BreathingPattern::Recovery
        );
        for p in [
            BreathingPattern::BoxBreathing,
            BreathingPattern::Relaxed,
            BreathingPattern::AerobicInhale,
            BreathingPattern::Endurance,
            BreathingPattern::Recovery,
            BreathingPattern::RunningSync,
        ] {
            assert!(!p.description().is_empty());
        }
    }

    #[test]
    fn fitness_history_fitness_score() {
        let e = FitnessHistoryEntry {
            exercise_type: ExerciseType::Running,
            duration_min: 30,
            avg_hr: 140,
            calories: 250,
            rating: 4,
            rpe: 7,
            timestamp: 0,
        };
        // (30*2 + 7*10 + 20) / 10 = (60 + 70 + 20)/10 = 15.0
        assert_eq!(e.fitness_score(), 15.0);
        // Low HR (<= 100) contributes no HR bonus.
        let low = FitnessHistoryEntry {
            exercise_type: ExerciseType::Walking,
            duration_min: 20,
            avg_hr: 90,
            calories: 80,
            rating: 3,
            rpe: 4,
            timestamp: 0,
        };
        assert_eq!(low.fitness_score(), (20.0 * 2.0 + 4.0 * 10.0 + 0.0) / 10.0);
    }

    #[test]
    fn sports_coach_session_lifecycle() {
        let mut coach = SportsCoach::new();
        assert_eq!(coach.state(), ExerciseState::Idle);
        coach.start_session();
        assert_eq!(coach.state(), ExerciseState::WarmUp);
        assert_eq!(coach.session_duration(), 0);
        coach.end_session();
        assert_eq!(coach.state(), ExerciseState::Finished);
    }

    #[test]
    fn session_progress_percentage() {
        let mut coach = SportsCoach::new(); // default goal = 30 min
        coach.start_session();
        assert_eq!(coach.session_progress(), 0.0);
        // 900 s = 15 min of a 30-min goal → 50%.
        coach.update(&hr_data(120, 0), None, 900);
        let p = coach.session_progress();
        assert!((p - 50.0).abs() < 0.01, "got {}", p);
        // Zero-duration goal never divides by zero.
        let mut rest = SportsCoach::new();
        rest.set_goal(ExerciseGoal::new(ExerciseType::Rest)); // duration 0
        assert_eq!(rest.session_progress(), 0.0);
    }

    #[test]
    fn adjust_goal_reduces_intensity_in_hot_weather() {
        let mut coach = SportsCoach::new();
        let health = HealthSensorManager::default();
        let before = coach.goal().target_intensity;
        coach.adjust_goal_for_conditions(&health, 35.0);
        assert!(
            coach.goal().target_intensity < before,
            "hot weather must cut intensity"
        );
        assert!(coach.goal().adjustments.iter().any(|a| a.contains("Hot")));
    }

    #[test]
    fn adjust_goal_extends_warmup_in_cold_weather() {
        let mut coach = SportsCoach::new();
        let health = HealthSensorManager::default();
        coach.adjust_goal_for_conditions(&health, 0.0);
        assert!(coach.goal().adjustments.iter().any(|a| a.contains("Cold")));
        // Cold weather does not change intensity, but intensity stays in-band.
        assert!(coach.goal().target_intensity >= 0.3 && coach.goal().target_intensity <= 0.95);
    }

    #[test]
    fn calculate_adaptive_goal_increases_when_recovering_well() {
        let coach = SportsCoach::new();
        let health = HealthSensorManager::default();
        // avg_rpe = 5 < 6 and days_active = 5 > 3 → +10% duration.
        let history = vec![
            FitnessHistoryEntry {
                exercise_type: ExerciseType::Running,
                duration_min: 30,
                avg_hr: 140,
                calories: 250,
                rating: 4,
                rpe: 5,
                timestamp: 0,
            },
            FitnessHistoryEntry {
                exercise_type: ExerciseType::Running,
                duration_min: 40,
                avg_hr: 145,
                calories: 300,
                rating: 4,
                rpe: 5,
                timestamp: 1,
            },
            FitnessHistoryEntry {
                exercise_type: ExerciseType::Running,
                duration_min: 35,
                avg_hr: 138,
                calories: 270,
                rating: 4,
                rpe: 5,
                timestamp: 2,
            },
        ];
        let goal = coach.calculate_adaptive_goal(&history, &health, 5);
        // avg_duration = 35 → 35 * 1.1 = 38.5 → 38.
        assert_eq!(goal.duration_min, 38);
        assert!(goal.adjustments.iter().any(|a| a.contains("increase")));
    }

    #[test]
    fn calculate_adaptive_goal_reduces_when_fatigued() {
        let coach = SportsCoach::new();
        let health = HealthSensorManager::default();
        // avg_rpe = 9 > 8 → -15% duration.
        let history = vec![
            FitnessHistoryEntry {
                exercise_type: ExerciseType::Running,
                duration_min: 30,
                avg_hr: 170,
                calories: 300,
                rating: 3,
                rpe: 9,
                timestamp: 0,
            },
            FitnessHistoryEntry {
                exercise_type: ExerciseType::Running,
                duration_min: 30,
                avg_hr: 172,
                calories: 300,
                rating: 3,
                rpe: 9,
                timestamp: 1,
            },
        ];
        let goal = coach.calculate_adaptive_goal(&history, &health, 1);
        // avg_duration = 30 → 30 * 0.85 = 25.5 → 25.
        assert_eq!(goal.duration_min, 25);
        assert!(goal.adjustments.iter().any(|a| a.contains("fatigue")));
    }

    #[test]
    fn calculate_adaptive_goal_defaults_for_new_user() {
        let coach = SportsCoach::new();
        let health = HealthSensorManager::default();
        let goal = coach.calculate_adaptive_goal(&[], &health, 0);
        // Empty history → defaults (Running, 30 min).
        assert_eq!(goal.duration_min, 30);
        assert!(goal.adjustments.is_empty());
    }

    #[test]
    fn update_transitions_through_session_states() {
        let mut coach = SportsCoach::new();
        coach.start_session(); // WarmUp
                               // 400 s > 300 s warm-up → Active.
        coach.update(&hr_data(120, 0), None, 400);
        assert_eq!(coach.state(), ExerciseState::Active);
        // Goal is 30 min = 1800 s; push past it → CoolDown.
        coach.update(&hr_data(120, 0), None, 2000);
        assert_eq!(coach.state(), ExerciseState::CoolDown);
        // Cool-down ends at (30+5) min = 2100 s.
        coach.update(&hr_data(120, 0), None, 1000);
        assert_eq!(coach.state(), ExerciseState::Finished);
    }

    #[test]
    fn update_emits_danger_zone_warning() {
        let mut coach = SportsCoach::new();
        // hr 190 → Danger zone (age 30).
        coach.update(&hr_data(190, 0), None, 10);
        let msgs = coach.get_messages();
        assert!(msgs
            .iter()
            .any(|m| m.msg_type == CoachingMessageType::Warning));
    }

    #[test]
    fn update_emits_fever_warning() {
        let mut coach = SportsCoach::new();
        coach.start_session();
        let temp = TemperatureData::new(38.5, 0); // > 37.5 → fever
        coach.update(&hr_data(120, 0), Some(&temp), 10);
        let msgs = coach.get_messages();
        assert!(msgs
            .iter()
            .any(|m| m.msg_type == CoachingMessageType::Warning));
    }

    #[test]
    fn coaching_message_truncates_long_text() {
        let long = "x".repeat(200);
        let msg = CoachingMessage::new(CoachingMessageType::Warning, &long, 8);
        assert_eq!(msg.voice_text.len(), 128); // bounded, never panics
        assert_eq!(msg.priority, 8);
    }
}
