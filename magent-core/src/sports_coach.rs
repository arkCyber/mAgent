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
    pub fn add_adjustment(&mut self, reason: &str) {
        if self
            .adjustments
            .push(String::try_from(reason).unwrap())
            .is_err()
        {
            let _ = self.adjustments.remove(0);
            let _ = self.adjustments.push(String::try_from(reason).unwrap());
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
        Self {
            msg_type,
            voice_text: String::try_from(voice_text).unwrap(),
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
            let msg =
                CoachingMessage::new(CoachingMessageType::BreathingCorrection, correction, 8);
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
