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

use crate::error::{try_heapless, Result};
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
            // HARDENING (audit-2026-08 unwrap sweep): TTS input from the
            // agent or sensor rules can be arbitrarily long. Truncating at
            // the 512-char boundary preserves partial speech rather than
            // crashing the notification pipeline during a health alert.
            text: try_heapless::<256>(text),
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
            // HARDENING (audit-2026-08 unwrap sweep): notification title/body
            // are typically short, but external sources could provide long
            // content. `try_heapless` keeps the notification pipeline alive.
            title: try_heapless::<64>(title),
            body: try_heapless::<256>(body),
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
            language: try_heapless::<8>("zh-CN"),
            voice_id: None,
            enable_ssml: false,
        }
    }
}

impl TtsConfig {
    /// Set speech rate
    pub fn set_speech_rate(&mut self, rate: f32) {
        // NaN would propagate silently through clamp (NaN.clamp returns NaN).
        // Treat NaN as the default rate (1.0) so a sensor/LLM that sends NaN
        // doesn't leave the TTS config in an undefined state.
        if rate.is_nan() {
            self.speech_rate = 1.0;
        } else {
            self.speech_rate = rate.clamp(0.5, 2.0);
        }
    }

    /// Set volume
    pub fn set_volume(&mut self, volume: f32) {
        if volume.is_nan() {
            self.volume = 0.8;
        } else {
            self.volume = volume.clamp(0.0, 1.0);
        }
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

        if self.dnd_start_hour <= self.dnd_end_hour {
            // Same-day window (e.g. 09:00 – 17:00): inside iff
            // `start <= hour < end`.
            current_hour >= self.dnd_start_hour && current_hour < self.dnd_end_hour
        } else {
            // Spans midnight (e.g. 22:00 – 07:00): inside iff
            // `hour >= start` (late night) or `hour < end` (early morning).
            current_hour >= self.dnd_start_hour || current_hour < self.dnd_end_hour
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
    ) -> crate::error::Result<QueueOutcome> {
        if !self.voice_enabled {
            return Ok(QueueOutcome::VoiceDisabled);
        }

        // Check DND for non-critical messages
        let current_hour = ((timestamp / 3600000) % 24) as u8;
        if self.is_in_dnd_period(current_hour) && priority < 8 {
            // Queue as notification instead.
            //
            // `send_notification` applies its *own* do-not-disturb rule,
            // which drops `priority < 7`. The two thresholds disagree (this
            // one routes < 8), so a priority-7 cue was routed and then
            // silently discarded by the notification layer while the caller
            // reported "sent as a screen notification instead" — a
            // fabricated delivery, and the exact bug this round set out to
            // remove. Ask the notification path what it did and report that.
            let delivered = self.push_notification(
                "语音消息(勿扰模式)",
                text,
                NotificationType::Screen,
                priority,
                category,
                timestamp,
            )?;
            return Ok(if delivered {
                QueueOutcome::RoutedToNotification
            } else {
                // Neither spoken nor shown: dropped by do-not-disturb.
                QueueOutcome::DroppedInDnd
            });
        }

        let message = VoiceMessage::new(self.next_message_id, text, priority, category, timestamp);
        self.next_message_id += 1;

        // Insert by priority (highest first). Both branches report what
        // actually happened: a full queue used to silently drop the new
        // message when nothing in it had a lower priority, and the *caller*
        // then reported "queued" because the depth had not shrunk.
        let insert_pos = self.voice_queue.iter().position(|m| m.priority < priority);
        match insert_pos {
            Some(pos) => {
                if self.voice_queue.insert(pos, message.clone()).is_err() {
                    // Queue full: evict the lowest-priority entry to make
                    // room. `Vec::insert` cannot fail after `pop`.
                    self.voice_queue.pop();
                    self.voice_queue.insert(pos, message).map_err(|_| {
                        crate::error::AgentError::BufferOverflow {
                            capacity: MAX_VOICE_QUEUE,
                            attempted: 1,
                        }
                    })?;
                    Ok(QueueOutcome::EvictedLowerPriority)
                } else {
                    Ok(QueueOutcome::Queued)
                }
            }
            None => {
                if self.voice_queue.push(message.clone()).is_err() {
                    // Full and ordered: only an entry *below* this priority
                    // can give way.
                    if let Some(pos) = self.voice_queue.iter().position(|m| m.priority < priority) {
                        self.voice_queue.remove(pos);
                        self.voice_queue.push(message).map_err(|_| {
                            crate::error::AgentError::BufferOverflow {
                                capacity: MAX_VOICE_QUEUE,
                                attempted: 1,
                            }
                        })?;
                        Ok(QueueOutcome::EvictedLowerPriority)
                    } else {
                        // Every queued message is at least as important as
                        // this one. Dropping one of them would lose a more
                        // urgent cue, so the *new* message is the one that
                        // does not fit — and the caller has to say so.
                        Ok(QueueOutcome::Dropped)
                    }
                } else {
                    Ok(QueueOutcome::Queued)
                }
            }
        }
    }

    /// Get next voice message to speak
    pub fn get_next_voice(&mut self, current_time_ms: u32) -> Option<VoiceMessage> {
        // Check minimum interval. `saturating_sub` so a clock that has gone
        // backwards — a u32 millisecond counter wrapping every ~49.7 days, or a
        // caller switching time sources — reads as "no time has passed" and
        // holds the rate limit, instead of wrapping to a huge elapsed value
        // that bypasses it (and panics under debug overflow checks).
        if current_time_ms.saturating_sub(self.last_speech_ms) < self.min_speech_interval_ms
            && !self.voice_queue.is_empty()
        {
            return None;
        }

        // Get highest priority message. The queue is ordered highest-first
        // (index 0 = highest), so we remove from the front — `pop()` would
        // wrongly serve the *lowest*-priority message.
        if !self.voice_queue.is_empty() {
            let mut msg = self.voice_queue.remove(0);
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
        self.push_notification(
            title,
            body,
            notification_type,
            priority,
            category,
            timestamp,
        )
        .map(|_| ())
    }

    /// The body of [`Self::send_notification`], reporting whether the
    /// notification was actually stored.
    ///
    /// `false` means one of two silent drops: do-not-disturb refused it
    /// (`priority < 7` while DND is active), or the history was full and
    /// held nothing acknowledged to evict. Callers that tell a user "this
    /// was shown on screen" must use this, not the `.map(|_| ())` wrapper —
    /// otherwise a refused notification is reported as delivered.
    fn push_notification(
        &mut self,
        title: &str,
        body: &str,
        notification_type: NotificationType,
        priority: u8,
        category: VoiceCategory,
        timestamp: u32,
    ) -> Result<bool> {
        // Check if notification should be delivered based on DND
        let current_hour = ((timestamp / 3600000) % 24) as u8;
        if self.is_in_dnd_period(current_hour) && priority < 7 {
            // Silently drop low-priority notifications during DND
            return Ok(false);
        }

        let notification = Notification {
            id: self.next_message_id,
            title: try_heapless::<64>(title),
            body: try_heapless::<256>(body),
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
                if self.notifications.push(notification).is_err() {
                    return Ok(false);
                }
            } else {
                // History full with everything unacknowledged: the newest
                // notification does not fit. Say so instead of returning
                // success for an entry that was never stored.
                return Ok(false);
            }
        }

        Ok(true)
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

    /// Send health alert notification.
    ///
    /// Returns what each channel actually did. The caller (the simulator's
    /// `send_health_alert` tool) used to infer this by comparing queue and
    /// history lengths before and after, which cannot distinguish "the
    /// notification was appended" from "an older entry was evicted to make
    /// room" — both leave the length unchanged — so a dropped alert could
    /// be reported as raised.
    pub fn send_health_alert(
        &mut self,
        alert_type: &str,
        severity: u8,
        message: &str,
        recommendation: &str,
        timestamp: u32,
    ) -> Result<AlertOutcome> {
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
            let voice = self.queue_voice(
                &format!("{}。{}", message, recommendation),
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
            let follow_up = self.queue_voice(
                "请注意查看您的健康数据。",
                9,
                VoiceCategory::Warning,
                timestamp,
            )?;
            let screen = self.push_notification(
                &title,
                &format!("{}\n\n建议：{}", message, recommendation),
                NotificationType::Vibration,
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
            Ok(AlertOutcome {
                voice: voice.was_queued(),
                notification: screen,
                follow_up: follow_up.was_queued(),
            })
        } else if severity >= 5 {
            // Voice + screen
            let voice = self.queue_voice(
                &format!("{}。{}", message, recommendation),
                severity,
                VoiceCategory::Alert,
                timestamp,
            )?;
            let screen = self.push_notification(
                &title,
                &format!("{}\n\n建议：{}", message, recommendation),
                NotificationType::Screen,
                severity,
                VoiceCategory::Warning,
                timestamp,
            )?;
            Ok(AlertOutcome {
                voice: voice.was_queued(),
                notification: screen,
                follow_up: false,
            })
        } else {
            // Screen only
            let screen = self.push_notification(
                &title,
                message,
                NotificationType::Screen,
                severity,
                VoiceCategory::System,
                timestamp,
            )?;
            Ok(AlertOutcome {
                voice: false,
                notification: screen,
                follow_up: false,
            })
        }
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
            .map(|_| ())
    }

    /// Send encouragement
    pub fn send_encouragement(&mut self, message: &str, timestamp: u32) -> Result<()> {
        self.queue_voice(message, 3, VoiceCategory::Encouragement, timestamp)
            .map(|_| ())
    }

    /// Send meditation guidance
    pub fn send_meditation(&mut self, script: &str, timestamp: u32) -> Result<()> {
        self.queue_voice(script, 7, VoiceCategory::Meditation, timestamp)
            .map(|_| ())
    }
}

impl Default for VoiceNotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`VoiceNotificationManager::queue_voice`] actually did with a cue.
///
/// Returned so a caller never reports "queued" for a message that was
/// routed elsewhere or dropped. `queue_voice` used to return only
/// `Result<()>`, so the one case where it silently lost a message (a full
/// queue holding nothing less urgent) was indistinguishable from success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOutcome {
    /// Added to the voice queue.
    Queued,
    /// Added, after evicting a lower-priority entry to make room.
    EvictedLowerPriority,
    /// Not queued: do-not-disturb is active, so it went out as a
    /// screen notification instead.
    RoutedToNotification,
    /// Not queued, and the do-not-disturb substitution was itself
    /// refused (`send_notification` drops `priority < 7`, while voice
    /// routing uses `< 8`). The user was told nothing.
    DroppedInDnd,
    /// Not queued and not routed: the queue is full of cues at least as
    /// urgent as this one.
    Dropped,
    /// Not queued: voice output is disabled on this device.
    VoiceDisabled,
}

impl QueueOutcome {
    /// Whether the message will actually be spoken.
    pub fn was_queued(self) -> bool {
        matches!(
            self,
            QueueOutcome::Queued | QueueOutcome::EvictedLowerPriority
        )
    }
}

/// What [`VoiceNotificationManager::send_health_alert`] delivered.
///
/// Both channels are independent: under do-not-disturb either can be
/// refused, and a full queue can drop a cue without changing its depth.
/// Reporting the outcome lets the caller say what happened instead of
/// guessing from before/after counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertOutcome {
    /// The **alert's own** voice cue was actually queued.
    ///
    /// This deliberately excludes the generic follow-up cue. Reporting
    /// `voice || follow_up` meant a full queue that dropped the real
    /// "严重警告：心跳过速…" while the lower-priority "请注意查看您的健康数据"
    /// still got in was reported as the alert having been spoken — the user
    /// heard the filler and missed the alarm, and the caller was told
    /// otherwise.
    pub voice: bool,
    /// The screen/vibration notification was actually stored.
    pub notification: bool,
    /// The generic follow-up cue was queued. Informational: it is not the
    /// alert, and must never be read as one.
    pub follow_up: bool,
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
        // HARDENING (audit-2026-08 unwrap sweep): emergency alert messages
        // come from LLM responses or sensor rules — not bounded at compile
        // time. `try_heapless` prevents panic in the critical alert path.
        Self {
            message: try_heapless::<256>(message),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_category_priority_and_names() {
        assert_eq!(VoiceCategory::Alert.priority_multiplier(), 10);
        assert_eq!(VoiceCategory::Warning.priority_multiplier(), 9);
        assert_eq!(VoiceCategory::Breathing.priority_multiplier(), 8);
        assert_eq!(VoiceCategory::Meditation.priority_multiplier(), 7);
        assert_eq!(VoiceCategory::Coaching.priority_multiplier(), 5);
        assert_eq!(VoiceCategory::Encouragement.priority_multiplier(), 3);
        assert_eq!(VoiceCategory::System.priority_multiplier(), 2);
        // All have a non-empty Chinese display name.
        for c in [
            VoiceCategory::Alert,
            VoiceCategory::Warning,
            VoiceCategory::Breathing,
            VoiceCategory::Meditation,
            VoiceCategory::Coaching,
            VoiceCategory::Encouragement,
            VoiceCategory::System,
        ] {
            assert!(!c.name().is_empty());
        }
    }

    #[test]
    fn notification_new_truncates_long_fields() {
        let long_body = "x".repeat(400);
        let n = Notification::new(
            1,
            "t",
            &long_body,
            NotificationType::Screen,
            5,
            VoiceCategory::System,
            0,
        );
        assert_eq!(n.title.as_str(), "t");
        assert_eq!(n.body.len(), 256); // truncated to the bounded buffer
        assert!(!n.delivered && !n.acknowledged);
    }

    #[test]
    fn tts_config_defaults_and_clamps() {
        let cfg = TtsConfig::default();
        assert_eq!(cfg.speech_rate, 1.0);
        assert_eq!(cfg.volume, 0.8);
        assert_eq!(cfg.volume_percent(), 80);

        let mut c = cfg;
        c.set_speech_rate(5.0); // clamp to 2.0
        assert_eq!(c.speech_rate, 2.0);
        c.set_speech_rate(0.1); // clamp to 0.5
        assert_eq!(c.speech_rate, 0.5);
        c.set_volume(3.0); // clamp to 1.0
        assert_eq!(c.volume, 1.0);
        assert_eq!(c.volume_percent(), 100);
    }

    #[test]
    fn dnd_period_logic_same_day_and_midnight() {
        let mut mgr = VoiceNotificationManager::new();
        // Default DND 22:00 → 07:00 spans midnight.
        mgr.set_dnd(true, 22, 7);
        assert!(!mgr.is_in_dnd_period(12)); // noon, outside
        assert!(mgr.is_in_dnd_period(23)); // 23:00, inside
        assert!(mgr.is_in_dnd_period(2)); // 02:00, inside
        assert!(mgr.is_in_dnd_period(6)); // 06:00, inside
        assert!(!mgr.is_in_dnd_period(7)); // 07:00 boundary, outside

        // Same-day DND 9:00 → 17:00.
        mgr.set_dnd(true, 9, 17);
        assert!(!mgr.is_in_dnd_period(8));
        assert!(mgr.is_in_dnd_period(10));
        assert!(!mgr.is_in_dnd_period(17));

        // Disabled → always false.
        mgr.set_dnd(false, 22, 7);
        assert!(!mgr.is_in_dnd_period(2));
    }

    #[test]
    fn queue_voice_orders_highest_priority_first() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.queue_voice("low", 3, VoiceCategory::Encouragement, 0)
            .unwrap();
        mgr.queue_voice("high", 9, VoiceCategory::Warning, 0)
            .unwrap();
        mgr.queue_voice("mid", 5, VoiceCategory::Coaching, 0)
            .unwrap();
        let q = mgr.pending_voices();
        // Highest priority should be at the front (index 0).
        assert_eq!(
            q[0].priority, 9,
            "highest priority must be first, got {}",
            q[0].priority
        );
        assert_eq!(q[1].priority, 5);
        assert_eq!(q[2].priority, 3);
    }

    #[test]
    fn get_next_voice_speaks_highest_priority_first() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.queue_voice("low", 3, VoiceCategory::Encouragement, 0)
            .unwrap();
        mgr.queue_voice("high", 9, VoiceCategory::Warning, 0)
            .unwrap();
        // Bypass the 2s min-interval by using a large timestamp.
        let first = mgr.get_next_voice(100_000).expect("first message");
        assert_eq!(first.text.as_str(), "high");
        assert!(first.spoken);
        // The second one is the next-highest.
        let second = mgr.get_next_voice(102_000).expect("second message");
        assert_eq!(second.text.as_str(), "low");
    }

    #[test]
    fn get_next_voice_respects_min_interval() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.queue_voice("a", 5, VoiceCategory::Coaching, 0).unwrap();
        // First call at t=100000 speaks it and records last_speech_ms.
        let first = mgr.get_next_voice(100_000);
        assert!(first.is_some());
        // A call too soon after (1s < 2s) returns None (cooldown).
        assert!(mgr.get_next_voice(101_000).is_none());
        // After the interval elapses, the next message is served.
        mgr.queue_voice("b", 4, VoiceCategory::Coaching, 0).unwrap();
        let next = mgr.get_next_voice(103_000);
        assert_eq!(next.unwrap().text.as_str(), "b");
    }

    #[test]
    fn get_next_voice_holds_the_rate_limit_when_the_clock_goes_backwards() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.queue_voice("a", 5, VoiceCategory::Coaching, 0).unwrap();
        mgr.queue_voice("b", 4, VoiceCategory::Coaching, 0).unwrap();
        assert!(mgr.get_next_voice(100_000).is_some());
        // The clock jumps back (wrap or a switched time source). The elapsed
        // time must read as zero — holding the 2 s cooldown — not as a wrapped
        // ~4e9 ms that serves a message immediately.
        assert!(mgr.get_next_voice(50_000).is_none());
    }

    #[test]
    fn voice_disabled_queues_nothing() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.set_voice_enabled(false);
        mgr.queue_voice("nope", 5, VoiceCategory::Coaching, 0)
            .unwrap();
        assert_eq!(mgr.voice_queue_size(), 0);
        assert!(mgr.get_next_voice(100_000).is_none());
    }

    #[test]
    fn send_notification_drops_during_dnd_low_priority() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.set_dnd(true, 22, 7);
        // Hour 2 (inside DND) + priority 3 < 7 → silently dropped.
        mgr.send_notification(
            "t",
            "b",
            NotificationType::Screen,
            3,
            VoiceCategory::System,
            2 * 3_600_000,
        )
        .unwrap();
        assert_eq!(
            mgr.notification_history().len(),
            0,
            "low-priority DND message must be dropped"
        );
        // High priority (>= 7) is delivered even in DND.
        mgr.send_notification(
            "t",
            "b",
            NotificationType::Screen,
            9,
            VoiceCategory::Alert,
            2 * 3_600_000,
        )
        .unwrap();
        assert_eq!(mgr.notification_history().len(), 1);
    }

    #[test]
    fn send_health_alert_channels_by_severity() {
        let mut mgr = VoiceNotificationManager::new();
        // Severity 8+ → voice queue + notification (multiple channels).
        mgr.send_health_alert("心率异常", 8, "心动过速", "请休息", 12 * 3_600_000)
            .unwrap();
        assert!(mgr.voice_queue_size() >= 1);
        assert!(!mgr.notification_history().is_empty());

        let mut mgr2 = VoiceNotificationManager::new();
        // Severity 3 → screen-only (no voice queued).
        mgr2.send_health_alert("提醒", 3, "检测到低活动", "起来活动", 12 * 3_600_000)
            .unwrap();
        assert_eq!(mgr2.voice_queue_size(), 0);
        assert_eq!(mgr2.notification_history().len(), 1);
    }

    #[test]
    fn mark_delivered_and_acknowledge() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.send_notification(
            "t",
            "b",
            NotificationType::Screen,
            5,
            VoiceCategory::System,
            0,
        )
        .unwrap();
        let id = mgr.notification_history()[0].id;
        // Unknown id → false.
        assert!(!mgr.mark_delivered(9999));
        assert!(!mgr.acknowledge(9999));
        // Known id → true, and unacknowledged() reflects the state.
        assert!(mgr.mark_delivered(id));
        assert_eq!(mgr.unacknowledged().len(), 1);
        assert!(mgr.acknowledge(id));
        assert_eq!(mgr.unacknowledged().len(), 0);
    }

    #[test]
    fn recent_notifications_returns_newest_first() {
        let mut mgr = VoiceNotificationManager::new();
        for i in 0..5 {
            mgr.send_notification(
                &format!("t{}", i),
                "b",
                NotificationType::Screen,
                5,
                VoiceCategory::System,
                i,
            )
            .unwrap();
        }
        let recent = mgr.recent_notifications(3);
        assert_eq!(recent.len(), 3);
        // Newest first: ids are 1..=5 (next_message_id starts at 1).
        assert_eq!(recent[0].title.as_str(), "t4");
        assert_eq!(recent[2].title.as_str(), "t2");
    }

    #[test]
    fn prepare_for_tts_adds_rate_dependent_pauses() {
        // Normal rate → ". " between sentences.
        let normal = TtsConfig::default(); // speech_rate 1.0
        assert_eq!(prepare_for_tts("a。b。c", &normal).as_str(), "a. b. c");

        // Slow rate → ", " pauses.
        let mut slow = normal.clone();
        slow.set_speech_rate(0.5);
        assert_eq!(prepare_for_tts("a。b。c", &slow).as_str(), "a, b, c");

        // Fast rate → no pauses.
        let mut fast = normal.clone();
        fast.set_speech_rate(1.5);
        assert_eq!(prepare_for_tts("a。b。c", &fast).as_str(), "abc");
    }

    #[test]
    fn emergency_alert_sms_body_contains_message() {
        let alert = EmergencyAlert::new("检测到跌倒，请确认", true, true, 1_700_000_000);
        let body = alert.sms_body();
        assert!(body.as_str().contains("检测到跌倒，请确认"));
        assert!(body.as_str().contains("紧急健康预警"));
        // Long messages are truncated to the bounded buffer, never panic.
        let long = EmergencyAlert::new(&"x".repeat(1000), false, false, 1);
        assert!(!long.sms_body().is_empty());
    }

    #[test]
    fn channel_toggles_and_tts_config_round_trip() {
        let mut mgr = VoiceNotificationManager::default();
        // `default()` must equal `new()`.
        assert!(mgr.is_voice_enabled());
        assert!(!mgr.is_speaking());

        mgr.set_vibration_enabled(false);
        mgr.set_led_enabled(false);
        mgr.set_screen_enabled(false);
        // The toggles are observable only through behaviour, so assert the
        // state that gates it: a disabled-voice manager still reports the flag.
        mgr.set_voice_enabled(false);
        assert!(!mgr.is_voice_enabled());

        let mut cfg = TtsConfig::default();
        cfg.set_speech_rate(1.5);
        mgr.set_tts_config(cfg);
        assert_eq!(mgr.tts_config().speech_rate, 1.5);
    }

    #[test]
    fn clear_helpers_empty_the_queues() {
        let mut mgr = VoiceNotificationManager::new();
        assert!(mgr.send_coaching("breathe in", true, 0).is_ok());
        assert!(mgr.send_encouragement("keep going", 0).is_ok());
        assert!(mgr.send_health_alert("hr", 5, "high", "rest", 0).is_ok());
        assert!(mgr.voice_queue_size() > 0);
        assert!(!mgr.notification_history().is_empty());

        mgr.clear_voice_queue();
        mgr.clear_notifications();
        assert_eq!(mgr.voice_queue_size(), 0);
        assert!(mgr.notification_history().is_empty());
    }

    #[test]
    fn coaching_routes_breathing_and_coaching_categories() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.send_coaching("inhale", true, 0).unwrap();
        mgr.send_coaching("faster", false, 0).unwrap();
        let cats: Vec<VoiceCategory, 8> = mgr.pending_voices().iter().map(|m| m.category).collect();
        assert!(cats.contains(&VoiceCategory::Breathing));
        assert!(cats.contains(&VoiceCategory::Coaching));
        // Breathing outranks plain coaching.
        assert_eq!(mgr.pending_voices()[0].category, VoiceCategory::Breathing);
    }

    /// When the bounded queue is full and a new message outranks at least one
    /// queued message, the insert-at-position fails and the lowest-priority
    /// entry is evicted to make room — the new message must still land.
    #[test]
    fn a_full_queue_evicts_the_lowest_priority_for_a_newcomer() {
        let mut mgr = VoiceNotificationManager::new();
        // 31 high-priority entries then 1 low, so the queue is exactly full and
        // the low entry sits last.
        for _ in 0..(MAX_VOICE_QUEUE - 1) {
            mgr.queue_voice("hi", 10, VoiceCategory::Alert, 0).unwrap();
        }
        mgr.queue_voice("low", 1, VoiceCategory::System, 0).unwrap();
        assert_eq!(mgr.voice_queue_size(), MAX_VOICE_QUEUE);

        // A mid-priority newcomer: position() finds the priority-1 tail, the
        // insert overflows, the tail is popped, and the newcomer is placed.
        mgr.queue_voice("mid", 5, VoiceCategory::Coaching, 0)
            .unwrap();
        assert_eq!(mgr.voice_queue_size(), MAX_VOICE_QUEUE);
        assert!(
            mgr.pending_voices()
                .iter()
                .any(|m| m.text.as_str() == "mid"),
            "the newcomer must be queued after evicting the lowest-priority entry",
        );
        assert!(
            !mgr.pending_voices()
                .iter()
                .any(|m| m.text.as_str() == "low"),
            "the lowest-priority entry is the one evicted",
        );
    }

    #[test]
    fn default_manager_is_indistinguishable_from_new() {
        // `Default::default()` delegates to `new()`; both must start with an
        // empty queue and no history.
        let d = VoiceNotificationManager::default();
        let n = VoiceNotificationManager::new();
        assert_eq!(d.voice_queue_size(), n.voice_queue_size());
        assert_eq!(
            d.notification_history().len(),
            n.notification_history().len()
        );
        assert_eq!(d.is_voice_enabled(), n.is_voice_enabled());
    }

    /// A cue the do-not-disturb substitution cannot deliver is not "routed".
    ///
    /// Voice routing bypasses DND for `priority >= 8` and substitutes a
    /// notification below that, but `send_notification` applies its *own*
    /// rule and drops `priority < 7`. A priority below 7 therefore fell
    /// between the two thresholds: it was refused by the notification layer
    /// while `queue_voice` reported `RoutedToNotification` — a delivery that
    /// never happened. The outcome has to say it was dropped. Priority 7 sits
    /// on the notification layer's threshold and is genuinely routed.
    #[test]
    fn a_dnd_substitution_that_is_itself_refused_is_reported_as_dropped() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.set_dnd(true, 0, 23); // hour 0 is inside the window

        // Priority 3: routed by voice, refused by the notification layer.
        assert_eq!(
            mgr.queue_voice("three", 3, VoiceCategory::System, 0)
                .unwrap(),
            QueueOutcome::DroppedInDnd
        );
        assert_eq!(mgr.voice_queue_size(), 0);
        assert!(
            mgr.notification_history().is_empty(),
            "nothing reached the screen either"
        );

        // Priority 7 is accepted by the notification layer, i.e. genuinely
        // routed.
        assert_eq!(
            mgr.queue_voice("seven", 7, VoiceCategory::Meditation, 0)
                .unwrap(),
            QueueOutcome::RoutedToNotification
        );
        assert_eq!(mgr.notification_history().len(), 1);
    }

    /// `send_notification` reports whether it stored anything. Returning
    /// `Ok(())` for a refused notification is what let callers claim a
    /// delivery that never happened.
    #[test]
    fn send_notification_reports_a_refusal() {
        let mut mgr = VoiceNotificationManager::new();
        mgr.set_dnd(true, 0, 23);
        // Refused: inside DND and below the notification threshold.
        assert!(
            !mgr.push_notification(
                "t",
                "b",
                NotificationType::Screen,
                6,
                VoiceCategory::System,
                0
            )
            .unwrap(),
            "a dropped notification must report false"
        );
        // Accepted: at/above the threshold.
        assert!(mgr
            .push_notification(
                "t",
                "b",
                NotificationType::Screen,
                7,
                VoiceCategory::System,
                0
            )
            .unwrap());
        // Outside DND everything is accepted.
        let mut mgr2 = VoiceNotificationManager::new();
        assert!(mgr2
            .push_notification(
                "t",
                "b",
                NotificationType::Screen,
                1,
                VoiceCategory::System,
                0
            )
            .unwrap());
    }

    /// A full history holding nothing acknowledged cannot take a new entry;
    /// that must be reported rather than returned as success.
    #[test]
    fn a_full_notification_history_reports_the_drop() {
        let mut mgr = VoiceNotificationManager::new();
        for i in 0..MAX_NOTIFICATION_HISTORY {
            assert!(mgr
                .push_notification(
                    "t",
                    &format!("b{i}"),
                    NotificationType::Screen,
                    9,
                    VoiceCategory::Alert,
                    0
                )
                .unwrap());
        }
        assert_eq!(mgr.notification_history().len(), MAX_NOTIFICATION_HISTORY);
        // Nothing is acknowledged, so there is no entry to evict.
        assert!(
            !mgr.push_notification(
                "t",
                "overflow",
                NotificationType::Screen,
                9,
                VoiceCategory::Alert,
                0
            )
            .unwrap(),
            "an entry that did not fit must report false"
        );
        assert_eq!(mgr.notification_history().len(), MAX_NOTIFICATION_HISTORY);
    }

    /// A dropped alert cue must not be reported as spoken because the
    /// follow-up filler made it in.
    ///
    /// Fill the queue with priority-9 entries, then raise a severity-8 alert.
    /// Its own cue (priority 8) is the *new* message with nothing lower to
    /// evict, so it is dropped; the follow-up (priority 9) is not higher than
    /// the occupants either — both are refused. Before the fix the outcome
    /// was `voice.was_queued() || follow_up.was_queued()`, and the simulator
    /// then told the model the alert had been spoken when its own message had
    /// been refused outright.
    #[test]
    fn a_dropped_alert_cue_is_not_reported_as_voiced_by_its_follow_up() {
        let mut mgr = VoiceNotificationManager::new();
        // Every slot holds a priority-8 cue: nothing lower can be evicted for
        // the alert's own priority-8 message, but the priority-9 follow-up
        // *can* evict one of them.
        for i in 0..MAX_VOICE_QUEUE {
            mgr.queue_voice(
                "occupied",
                8,
                VoiceCategory::Warning,
                (i as u32) * 1_000,
            )
            .unwrap();
        }
        assert_eq!(mgr.voice_queue_size(), MAX_VOICE_QUEUE);

        let outcome = mgr
            .send_health_alert("hr", 8, "severe tachycardia", "", 60_000)
            .unwrap();
        assert!(
            !outcome.voice,
            "the alert's own cue was refused; the filler that evicted a              queued entry must not be reported as it"
        );
        assert!(
            outcome.follow_up,
            "the follow-up did get in, and is reported as itself"
        );
    }

    /// The follow-up flag reports the filler separately and is never the
    /// alert's own delivery.
    #[test]
    fn the_follow_up_cue_is_reported_separately_from_the_alert() {
        let mut mgr = VoiceNotificationManager::new();
        let outcome = mgr
            .send_health_alert("hr", 9, "tachycardia", "", 12 * 3_600_000)
            .unwrap();
        assert!(outcome.voice, "the alert itself is queued");
        assert!(outcome.follow_up, "the filler is queued too");

        // Low severity has no follow-up at all.
        let mut mgr = VoiceNotificationManager::new();
        let outcome = mgr
            .send_health_alert("mood", 6, "low activity", "", 12 * 3_600_000)
            .unwrap();
        assert!(!outcome.follow_up);
    }

    /// The health-alert outcome names what each channel actually did, which
    /// the simulator's before/after counters could not distinguish (an
    /// eviction leaves the length unchanged).
    #[test]
    fn a_health_alert_reports_each_channels_outcome() {
        // Severity 9 blocks nothing: both channels fire.
        let mut mgr = VoiceNotificationManager::new();
        let outcome = mgr
            .send_health_alert("hr", 9, "tachycardia", "", 12 * 3_600_000)
            .unwrap();
        assert!(outcome.voice, "severity 9 queues voice");
        assert!(outcome.notification, "severity 9 raises a notification");

        // Severity 3 is screen-only.
        let mut mgr = VoiceNotificationManager::new();
        let outcome = mgr
            .send_health_alert("mood", 3, "low activity", "", 12 * 3_600_000)
            .unwrap();
        assert!(!outcome.voice, "severity 3 does not speak");
        assert!(outcome.notification);

        // Severity 6 under DND: both channels are suppressed, and the
        // outcome must say so on both.
        let mut mgr = VoiceNotificationManager::new();
        mgr.set_dnd(true, 0, 23);
        let outcome = mgr
            .send_health_alert("glucose", 6, "210 mg/dL", "", 0)
            .unwrap();
        assert!(!outcome.voice, "DND suppresses a priority-6 cue");
        assert!(!outcome.notification, "DND suppresses a priority-6 alert");
        assert_eq!(mgr.voice_queue_size(), 0);
        assert!(mgr.notification_history().is_empty());
    }
}
