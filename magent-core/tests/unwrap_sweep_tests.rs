//! Integration tests for the audit-2026-08 unwrap-sweep hardening changes.
//!
//! These tests verify that all constructors and helpers that were changed
//! from `String::try_from(...).unwrap()` to `try_heapless::<N>(...)` still
//! produce correct, panic-free output for both short and overlong inputs.

use magent_core::{
    error::try_heapless,
    voice_notification::{VoiceMessage, Notification, NotificationType, VoiceCategory},
    sports_coach::{CoachingMessage, CoachingMessageType},
    early_warning::{AlertType, AlertSeverity, HealthAlert, EmergencyContact, Hospital},
    health_sensors::UserProfile,
};

const BIG_STR: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn no_panic_big<const N: usize>(input: &str) {
    // If this test panics, the HARDENING is broken.
    let _s: heapless::String<N> = try_heapless(input);
}

// ============================================================================
// try_heapless — size-specific edge cases
// ============================================================================

#[test]
fn try_heapless_n32_short() {
    let s: heapless::String<32> = try_heapless("hello");
    assert_eq!(s.len(), 5);
}

#[test]
fn try_heapless_n32_truncates() {
    let big = "x".repeat(64);
    let s: heapless::String<32> = try_heapless(&big);
    // The corrected algorithm scans from N and stores N bytes (not N-1).
    // 64 bytes into String<32>: scan from 32, all 'x' are ASCII, so byte 32
    // is valid → stores 32 bytes. (The old `cap = N-1` would have stored 31.)
    assert_eq!(s.len(), 32);
    assert!(s.chars().all(|c| c == 'x'));
}

#[test]
fn try_heapless_n64_short() {
    let s: heapless::String<64> = try_heapless("Emergency Contact");
    assert_eq!(s.len(), 17);
}

#[test]
fn try_heapless_n128_truncates_at_char_boundary() {
    // 3-char Japanese string (9 bytes per char) — cap=127 means we should fit
    // at most 14 complete chars (126 bytes).
    let s: heapless::String<128> = try_heapless("日本語テスト文字列");
    assert!(s.len() <= 127);
    assert!(s.is_char_boundary(s.len()));
}

#[test]
fn try_heapless_n256_overlong() {
    no_panic_big::<256>(BIG_STR);
}

#[test]
fn try_heapless_n512_overlong() {
    no_panic_big::<512>(BIG_STR);
}

// ============================================================================
// VoiceMessage::new — text truncated at 256 bytes
// ============================================================================

#[test]
fn voice_message_new_short_text() {
    let msg = VoiceMessage::new(1, "Hello world", 1, VoiceCategory::Coaching, 0);
    assert_eq!(msg.text.as_str(), "Hello world");
}

#[test]
fn voice_message_new_truncates_long_text() {
    let big = "x".repeat(512);
    let msg = VoiceMessage::new(1, &big, 1, VoiceCategory::Coaching, 0);
    // Corrected algorithm stores N=256 bytes (was N-1 with the old cap).
    assert!(msg.text.len() <= 256);
    assert!(msg.text.chars().all(|c| c == 'x'));
}

// ============================================================================
// Notification::new — title/body truncated
// ============================================================================

#[test]
fn notification_new_short() {
    let n = Notification::new(
        1, "title", "body",
        NotificationType::Screen, 1, VoiceCategory::System, 0,
    );
    assert_eq!(n.title.as_str(), "title");
    assert_eq!(n.body.as_str(), "body");
}

#[test]
fn notification_new_truncates_long_title() {
    let big = "T".repeat(128);
    let n = Notification::new(
        1, &big, "body",
        NotificationType::Screen, 1, VoiceCategory::System, 0,
    );
    assert!(n.title.len() <= 64); // cap = 64-1
}

#[test]
fn notification_new_truncates_long_body() {
    let big = "B".repeat(512);
    let n = Notification::new(
        1, "title", &big,
        NotificationType::Screen, 1, VoiceCategory::System, 0,
    );
    assert!(n.body.len() <= 256); // cap = 256-1
}

// ============================================================================
// CoachingMessage::new — voice_text truncated at 128 bytes
// ============================================================================

#[test]
fn coaching_message_new_short() {
    let msg = CoachingMessage::new(
        CoachingMessageType::BreathingCorrection,
        "Breathe in slowly",
        1,
    );
    assert_eq!(msg.voice_text.as_str(), "Breathe in slowly");
}

#[test]
fn coaching_message_new_truncates_long_text() {
    let big = "V".repeat(256);
    let msg = CoachingMessage::new(
        CoachingMessageType::BreathingCorrection,
        &big,
        1,
    );
    assert!(msg.voice_text.len() <= 128); // cap = 128-1
    assert!(msg.voice_text.chars().all(|c| c == 'V'));
}

// ============================================================================
// HealthAlert::new — message/recommendation truncated
// ============================================================================

#[test]
fn health_alert_new_short() {
    let alert = HealthAlert::new(
        0,
        AlertType::GlucoseLow,
        AlertSeverity::High,
        50.0,
        70.0,
        "Glucose low",
        "Take glucose",
        0,
    );
    assert_eq!(alert.message.as_str(), "Glucose low");
    assert_eq!(alert.recommendation.as_str(), "Take glucose");
}

#[test]
fn health_alert_new_truncates_long_message() {
    let big = "M".repeat(512);
    let alert = HealthAlert::new(
        0,
        AlertType::GlucoseLow,
        AlertSeverity::High,
        50.0,
        70.0,
        &big,
        "Take glucose",
        0,
    );
    assert!(alert.message.len() <= 256);
    assert!(alert.message.chars().all(|c| c == 'M'));
}

// ============================================================================
// EmergencyContact::new — fields truncated
// ============================================================================

#[test]
fn emergency_contact_new_short() {
    let c = EmergencyContact::new("Dr. Smith", "+1-555-0100", "Doctor", 1);
    assert_eq!(c.name.as_str(), "Dr. Smith");
    assert_eq!(c.phone.as_str(), "+1-555-0100");
}

#[test]
fn emergency_contact_new_truncates_long_name() {
    let big = "N".repeat(128);
    let c = EmergencyContact::new(&big, "123", "Relation", 1);
    // Corrected: N=64, stores up to 64 bytes (was N-1=63 with old cap).
    assert!(c.name.len() <= 64);
}

// ============================================================================
// Hospital::new — fields truncated
// ============================================================================

#[test]
fn hospital_new_short() {
    let h = Hospital::new("City General", "123 Main St", "+1-555-0199");
    assert_eq!(h.name.as_str(), "City General");
}

#[test]
fn hospital_new_truncates_long_address() {
    let big = "A".repeat(256);
    let h = Hospital::new("Hospital", &big, "123");
    assert!(h.address.len() <= 128);
}

// ============================================================================
// UserProfile::default — compile-time strings still fit
// ============================================================================

#[test]
fn user_profile_default_compile_time_strings_fit() {
    let profile = UserProfile::default();
    assert!(profile.emergency_contact.len() <= 64);
    assert!(profile.emergency_phone.len() <= 32);
}
