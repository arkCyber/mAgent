//! Voice and Notification System for mAgent
//!
//! Provides voice output (TTS) and notification capabilities:
//! - Text-to-Speech synthesis for real-time coaching
//! - Voice message queue management
//! - Notification prioritization and delivery
//! - Alert escalation for critical health events
//!
//! This module handles all audio output and user notifications,
//! including voice coaching during exercise and health alerts.

use crate::error::Result;
use core::fmt::Write;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum queued voice messages
pub const MAX_VOICE_QUEUE: usize = 32;

/// Maximum notification history
pub const MAX_NOTIFICATION_HISTORY: usize = 100;

/// Voice message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceMessage {
    /// Message ID
    pub id: u32,
    /// Text to speak
    pub text: String<256>,
    /// Priority (higher = more urgent)
    pub priority: u8,
    /// Message category
    pub category: VoiceCategory,
    /// Timestamp
    pub timestamp: u32,
    /// Whether message was spoken
    pub spoken: bool,
}

impl VoiceMessage {
    /// Create new voice message
    pub fn new(id: u32, text: &str, priority: u8, category: VoiceCategory, timestamp: u32) -> Self {
        Self {
            id,
            text: String::try_from(text).unwrap(),
            priority,
            category,
            timestamp,
            spoken: false,
        }
    }
}

/// Voice message category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceCategory {
    /// Exercise coaching
    Coaching,
    /// Breathing correction
    Breathing,
    /// Health alert
    Alert,
    /// Meditation guidance
    Meditation,
    /// System notification
    System,
    /// Encouragement
    Encouragement,
    /// Warning
    Warning,
}

impl VoiceCategory {
    /// Get category priority multiplier
    pub fn priority_multiplier(&self) -> u8 {
        match self {
            VoiceCategory::Alert => 10,
            VoiceCategory::Warning => 9,
            VoiceCategory::Breathing => 8,
            VoiceCategory::Meditation => 7,
            VoiceCategory::Coaching => 5,
            VoiceCategory::Encouragement => 3,
            VoiceCategory::System => 2,
        }
    }

    /// Get Chinese name
    pub fn name(&self) -> &'static str {
        match self {
            VoiceCategory::Coaching => "运动指导",
            VoiceCategory::Breathing => "呼吸纠正",
            VoiceCategory::Alert => "健康警报",
            VoiceCategory::Meditation => "冥想引导",
            VoiceCategory::System => "系统通知",
            VoiceCategory::Encouragement => "鼓励",
            VoiceCategory::Warning => "警告",
        }
    }
}

/// Notification type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationType {
    /// Spoken cue via the on-device TTS / speaker.
    Voice,
    /// Haptic motor burst.
    Vibration,
    /// LED indicator (single blink / colour).
    Led,
    /// On-screen banner / modal.
    Screen,
    /// SMS forwarded over BLE to a paired phone.
    Sms,
}

/// Notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Notification ID
    pub id: u32,
    /// Title
    pub title: String<64>,
    /// Body
    pub body: String<256>,
    /// Notification type
    pub notification_type: NotificationType,
    /// Priority (1-10)
    pub priority: u8,
    /// Category
    pub category: VoiceCategory,
    /// Timestamp
    pub timestamp: u32,
    /// Delivered
    pub delivered: bool,
    /// Read/acknowledged
    pub acknowledged: bool,
}

impl Notification {
    /// Create new notification
    pub fn new(
        id: u32,
        title: &str,
        body: &str,
        notification_type: NotificationType,
        priority: u8,
        category: VoiceCategory,
        timestamp: u32,
    ) -> Self {
        Self {
            id,
            title: String::try_from(title).unwrap(),
            body: String::try_from(body).unwrap(),
            notification_type,
            priority,
            category,
            timestamp,
            delivered: false,
            acknowledged: false,
        }
    }
}

/// TTS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// Speech rate (0.5 - 2.0)
    pub speech_rate: f32,
    /// Pitch (0.5 - 2.0)
    pub pitch: f32,
    /// Volume (0.0 - 1.0)
    pub volume: f32,
    /// Language
    pub language: String<8>,
    /// Voice selection (if multiple available)
    pub voice_id: Option<String<32>>,
    /// Enable_ssml (text-to-speech markup)
    pub enable_ssml: bool,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            speech_rate: 1.0,
            pitch: 1.0,
            volume: 0.8,
            language: String::try_from("zh-CN").unwrap(),
            voice_id: None,
            enable_ssml: false,
        }
    }
}

impl TtsConfig {
    /// Set speech rate
    pub fn set_speech_rate(&mut self, rate: f32) {
        self.speech_rate = rate.clamp(0.5, 2.0);
    }

    /// Set volume
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Get volume as percentage
    pub fn volume_percent(&self) -> u8 {
        (self.volume * 100.0) as u8
    }
}

/// Voice and Notification Manager
pub struct VoiceNotificationManager {
    /// Voice message queue
    voice_queue: Vec<VoiceMessage, MAX_VOICE_QUEUE>,
    /// Notification history
    notifications: Vec<Notification, MAX_NOTIFICATION_HISTORY>,
    /// TTS configuration
    tts_config: TtsConfig,
    /// Current message ID
    next_message_id: u32,
    /// Voice enabled
    voice_enabled: bool,
    /// Vibration enabled
    vibration_enabled: bool,
    /// LED enabled
    led_enabled: bool,
    /// Screen enabled
    screen_enabled: bool,
    /// Do not disturb mode
    dnd_enabled: bool,
    /// DND start hour
    dnd_start_hour: u8,
    /// DND end hour
    dnd_end_hour: u8,
    /// Last spoken timestamp
    last_speech_ms: u32,
    /// Minimum interval between speech (ms)
    min_speech_interval_ms: u32,
    /// Current speaking state
    is_speaking: bool,
}

impl VoiceNotificationManager {
    /// Create new manager
    pub fn new() -> Self {
        Self {
            voice_queue: Vec::new(),
            notifications: Vec::new(),
            tts_config: TtsConfig::default(),
            next_message_id: 1,
            voice_enabled: true,
            vibration_enabled: true,
            led_enabled: true,
            screen_enabled: true,
            dnd_enabled: false,
            dnd_start_hour: 22,
            dnd_end_hour: 7,
            last_speech_ms: 0,
            min_speech_interval_ms: 2000, // 2 seconds minimum
            is_speaking: false,
        }
    }

    /// Enable/disable voice
    pub fn set_voice_enabled(&mut self, enabled: bool) {
        self.voice_enabled = enabled;
    }

    /// Enable/disable vibration
    pub fn set_vibration_enabled(&mut self, enabled: bool) {
        self.vibration_enabled = enabled;
    }

    /// Enable/disable LED
    pub fn set_led_enabled(&mut self, enabled: bool) {
        self.led_enabled = enabled;
    }

    /// Enable/disable screen
    pub fn set_screen_enabled(&mut self, enabled: bool) {
        self.screen_enabled = enabled;
    }

    /// Enable do not disturb
    pub fn set_dnd(&mut self, enabled: bool, start_hour: u8, end_hour: u8) {
        self.dnd_enabled = enabled;
        self.dnd_start_hour = start_hour;
        self.dnd_end_hour = end_hour;
    }

    /// Check if in DND period
    pub fn is_in_dnd_period(&self, current_hour: u8) -> bool {
        if !self.dnd_enabled {
            return false;
        }

        if self.dnd_start_hour < self.dnd_end_hour {
            // DND spans midnight (e.g., 22:00 - 07:00)
            current_hour >= self.dnd_start_hour || current_hour < self.dnd_end_hour
        } else {
            // DND within same day
            current_hour >= self.dnd_start_hour && current_hour < self.dnd_end_hour
        }
    }

    /// Set TTS configuration
    pub fn set_tts_config(&mut self, config: TtsConfig) {
        self.tts_config = config;
    }

    /// Get TTS configuration
    pub fn tts_config(&self) -> &TtsConfig {
        &self.tts_config
    }

    /// Queue a voice message
    pub fn queue_voice(
        &mut self,
        text: &str,
        priority: u8,
        category: VoiceCategory,
        timestamp: u32,
    ) -> Result<()> {
        if !self.voice_enabled {
            return Ok(());
        }

        // Check DND for non-critical messages
        let current_hour = ((timestamp / 3600000) % 24) as u8;
        if self.is_in_dnd_period(current_hour) && priority < 8 {
            // Queue as notification instead
            return self.send_notification(
                "语音消息(勿扰模式)",
                text,
                NotificationType::Screen,
                priority,
                category,
                timestamp,
            );
        }

        let message = VoiceMessage::new(self.next_message_id, text, priority, category, timestamp);
        self.next_message_id += 1;

        // Insert by priority (highest first)
        let insert_pos = self.voice_queue.iter().position(|m| m.priority < priority);
        match insert_pos {
            Some(pos) => {
                if self.voice_queue.insert(pos, message.clone()).is_err() {
                    // Queue full, replace lowest priority
                    self.voice_queue.pop();
                    let _ = self.voice_queue.insert(pos, message);
                }
            }
            None => {
                if self.voice_queue.push(message.clone()).is_err() {
                    // Queue full, remove oldest low-priority message
                    if let Some(pos) = self.voice_queue.iter().position(|m| m.priority < priority) {
                        let _ = self.voice_queue.remove(pos);
                        let _ = self.voice_queue.push(message);
                    }
                }
            }
        }

        Ok(())
    }

    /// Get next voice message to speak
    pub fn get_next_voice(&mut self, current_time_ms: u32) -> Option<VoiceMessage> {
        // Check minimum interval
        if current_time_ms - self.last_speech_ms < self.min_speech_interval_ms
            && !self.voice_queue.is_empty()
        {
            return None;
        }

        // Get highest priority message
        if let Some(mut msg) = self.voice_queue.pop() {
            // Mark as spoken
            msg.spoken = true;
            self.last_speech_ms = current_time_ms;
            self.is_speaking = true;

            // Queue notification for history
            let _ = self.send_notification(
                &format!("[语音] {}", msg.category.name()),
                &msg.text,
                NotificationType::Voice,
                msg.priority,
                msg.category,
                msg.timestamp,
            );

            return Some(msg);
        }

        self.is_speaking = false;
        None
    }

    /// Send a notification
    pub fn send_notification(
        &mut self,
        title: &str,
        body: &str,
        notification_type: NotificationType,
        priority: u8,
        category: VoiceCategory,
        timestamp: u32,
    ) -> Result<()> {
        // Check if notification should be delivered based on DND
        let current_hour = ((timestamp / 3600000) % 24) as u8;
        if self.is_in_dnd_period(current_hour) && priority < 7 {
            // Silently drop low-priority notifications during DND
            return Ok(());
        }

        let notification = Notification {
            id: self.next_message_id,
            title: String::try_from(title).unwrap(),
            body: String::try_from(body).unwrap(),
            notification_type,
            priority,
            category,
            timestamp,
            delivered: false,
            acknowledged: false,
        };

        self.next_message_id += 1;

        if self.notifications.push(notification.clone()).is_err() {
            // Remove oldest acknowledged notification
            if let Some(pos) = self.notifications.iter().position(|n| n.acknowledged) {
                let _ = self.notifications.remove(pos);
                let _ = self.notifications.push(notification);
            }
        }

        Ok(())
    }

    /// Mark notification as delivered
    pub fn mark_delivered(&mut self, notification_id: u32) -> bool {
        if let Some(n) = self
            .notifications
            .iter_mut()
            .find(|n| n.id == notification_id)
        {
            n.delivered = true;
            return true;
        }
        false
    }

    /// Acknowledge notification
    pub fn acknowledge(&mut self, notification_id: u32) -> bool {
        if let Some(n) = self
            .notifications
            .iter_mut()
            .find(|n| n.id == notification_id)
        {
            n.acknowledged = true;
            return true;
        }
        false
    }

    /// Get unacknowledged notifications
    pub fn unacknowledged(&self) -> Vec<&Notification, 16> {
        self.notifications
            .iter()
            .filter(|n| !n.acknowledged)
            .collect()
    }

    /// Get recent notifications
    pub fn recent_notifications(&self, count: usize) -> Vec<&Notification, 16> {
        self.notifications.iter().rev().take(count).collect()
    }

    /// Get notification history
    pub fn notification_history(&self) -> &[Notification] {
        &self.notifications
    }

    /// Get voice queue size
    pub fn voice_queue_size(&self) -> usize {
        self.voice_queue.len()
    }

    /// Clear voice queue
    pub fn clear_voice_queue(&mut self) {
        self.voice_queue.clear();
    }

    /// Clear notification history
    pub fn clear_notifications(&mut self) {
        self.notifications.clear();
    }

    /// Check if voice is enabled
    pub fn is_voice_enabled(&self) -> bool {
        self.voice_enabled
    }

    /// Check if currently speaking
    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    /// Get pending voice messages
    pub fn pending_voices(&self) -> &[VoiceMessage] {
        &self.voice_queue
    }

    /// Send health alert notification
    pub fn send_health_alert(
        &mut self,
        alert_type: &str,
        severity: u8,
        message: &str,
        recommendation: &str,
        timestamp: u32,
    ) -> Result<()> {
        let title = format!(
            "健康{}：{}",
            match severity {
                9..=10 => "紧急警报",
                7..=8 => "严重警告",
                5..=6 => "警告",
                _ => "提醒",
            },
            alert_type
        );

        // High priority = trigger multiple channels
        if severity >= 8 {
            // Voice + vibration + LED + screen
            self.queue_voice(
                &format!("{}。{}", message, recommendation),
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
            self.queue_voice(
                "请注意查看您的健康数据。",
                9,
                VoiceCategory::Warning,
                timestamp,
            )?;
            self.send_notification(
                &title,
                &format!("{}\n\n建议：{}", message, recommendation),
                NotificationType::Vibration,
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
        } else if severity >= 5 {
            // Voice + screen
            self.queue_voice(
                &format!("{}。{}", message, recommendation),
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
            self.send_notification(
                &title,
                &format!("{}\n\n建议：{}", message, recommendation),
                NotificationType::Screen,
                severity,
                VoiceCategory::Warning,
                timestamp,
            )?;
        } else {
            // Screen only
            self.send_notification(
                &title,
                message,
                NotificationType::Screen,
                severity,
                VoiceCategory::System,
                timestamp,
            )?;
        }

        Ok(())
    }

    /// Send coaching message
    pub fn send_coaching(&mut self, message: &str, breathing: bool, timestamp: u32) -> Result<()> {
        let category = if breathing {
            VoiceCategory::Breathing
        } else {
            VoiceCategory::Coaching
        };
        let priority = if breathing { 6 } else { 4 };

        self.queue_voice(message, priority, category, timestamp)
    }

    /// Send encouragement
    pub fn send_encouragement(&mut self, message: &str, timestamp: u32) -> Result<()> {
        self.queue_voice(message, 3, VoiceCategory::Encouragement, timestamp)
    }

    /// Send meditation guidance
    pub fn send_meditation(&mut self, script: &str, timestamp: u32) -> Result<()> {
        self.queue_voice(script, 7, VoiceCategory::Meditation, timestamp)
    }
}

impl Default for VoiceNotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to convert text to speech-ready format
pub fn prepare_for_tts(text: &str, config: &TtsConfig) -> String<512> {
    let mut result = String::new();

    // Apply speech rate by adding pauses between clauses.
    let pause_duration = if config.speech_rate < 0.8 {
        ", " // Longer pause for slow speech
    } else if config.speech_rate > 1.2 {
        "" // Shorter (no) pause for fast speech
    } else {
        ". " // Normal pause
    };

    // Split into sentences and add pauses. We use a heapless::Vec so this
// code path works in both std and no_std builds; the sentence count is
// bounded by the text length so 64 entries is more than enough for any
// realistic TTS input.
    let mut sentences: Vec<&str, 64> = Vec::new();
    for s in text.split(['。', '！', '？', ',', '.']) {
        if !s.is_empty() {
            let _ = sentences.push(s);
        }
    }
    for (i, sentence) in sentences.iter().enumerate() {
        let trimmed = sentence.trim();
        if !trimmed.is_empty() {
            if i > 0 {
                let _ = result.push_str(pause_duration);
            }
            let _ = result.push_str(trimmed);
        }
    }

    result
}

/// Emergency alert that bypasses all filters
pub struct EmergencyAlert {
    /// Alert message
    pub message: String<256>,
    /// Contact emergency services
    pub call_emergency: bool,
    /// Notify all emergency contacts
    pub notify_all_contacts: bool,
    /// Timestamp
    pub timestamp: u32,
}

impl EmergencyAlert {
    /// Create new emergency alert
    pub fn new(
        message: &str,
        call_emergency: bool,
        notify_all_contacts: bool,
        timestamp: u32,
    ) -> Self {
        Self {
            message: String::try_from(message).unwrap(),
            call_emergency,
            notify_all_contacts,
            timestamp,
        }
    }

    /// Generate SMS body
    pub fn sms_body(&self) -> String<512> {
        let mut body = String::new();
        let _ = writeln!(body, "【紧急健康预警】");
        let _ = writeln!(body, "{}", self.message);
        let _ = writeln!(body, "时间：{}", self.timestamp);
        let _ = write!(body, "请立即联系用户或呼叫急救。");
        body
    }
}
