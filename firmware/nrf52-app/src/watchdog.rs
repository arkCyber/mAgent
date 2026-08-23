//! Watchdog and Error Handling Module for nRF52840
//!
//! Implements system monitoring and error recovery.

use defmt::{info, warn};

// =============================================================================
// Watchdog Timer
// =============================================================================

/// Watchdog configuration
pub struct WatchdogConfig {
    /// Timeout in milliseconds
    pub timeout_ms: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
        }
    }
}

/// Watchdog state
pub struct Watchdog {
    last_feed: u64,
    timeout_ms: u32,
}

impl Watchdog {
    pub fn new(config: &WatchdogConfig) -> Self {
        Self {
            last_feed: 0,
            timeout_ms: config.timeout_ms,
        }
    }

    pub fn feed(&mut self, current_time_ms: u64) {
        self.last_feed = current_time_ms;
    }

    pub fn check_timeout(&self, current_time_ms: u64) -> bool {
        (current_time_ms - self.last_feed) > self.timeout_ms as u64
    }

    pub fn remaining_ms(&self, current_time_ms: u64) -> u32 {
        let elapsed = (current_time_ms - self.last_feed) as u32;
        self.timeout_ms.saturating_sub(elapsed)
    }
}

// =============================================================================
// Error Types
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SystemError {
    OutOfMemory,
    BleConnectionFailed,
    SensorInitFailed,
    WatchdogTimeout,
    InvalidConfig,
    CommError,
    Unknown,
}

impl defmt::Format for SystemError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Self::OutOfMemory => defmt::write!(f, "OutOfMemory"),
            Self::BleConnectionFailed => defmt::write!(f, "BleConnectionFailed"),
            Self::SensorInitFailed => defmt::write!(f, "SensorInitFailed"),
            Self::WatchdogTimeout => defmt::write!(f, "WatchdogTimeout"),
            Self::InvalidConfig => defmt::write!(f, "InvalidConfig"),
            Self::CommError => defmt::write!(f, "CommError"),
            Self::Unknown => defmt::write!(f, "Unknown"),
        }
    }
}

// =============================================================================
// Error Context
// =============================================================================

pub struct ErrorContext {
    pub last_error: Option<SystemError>,
    pub error_count: u32,
    pub reset_count: u32,
    pub uptime_ms: u64,
}

impl Default for ErrorContext {
    fn default() -> Self {
        Self {
            last_error: None,
            error_count: 0,
            reset_count: 0,
            uptime_ms: 0,
        }
    }
}

impl ErrorContext {
    pub fn record_error(&mut self, error: SystemError) {
        self.last_error = Some(error);
        self.error_count += 1;
        warn!("System error: {:?} (count: {})", error, self.error_count);
    }

    pub fn record_reset(&mut self) {
        self.reset_count += 1;
        info!("System reset #{}", self.reset_count);
    }

    pub fn status(&self) -> &'static str {
        if self.error_count > 10 {
            "DEGRADED"
        } else if self.last_error.is_some() {
            "RECOVERING"
        } else {
            "HEALTHY"
        }
    }
}

// =============================================================================
// Recovery Handler
// =============================================================================

pub struct RecoveryHandler {
    pub error_context: ErrorContext,
}

impl Default for RecoveryHandler {
    fn default() -> Self {
        Self {
            error_context: ErrorContext::default(),
        }
    }
}

impl RecoveryHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_error(&mut self, error: SystemError) {
        self.error_context.record_error(error);
    }

    pub fn record_reset(&mut self) {
        self.error_context.record_reset();
    }

    pub fn context(&self) -> &ErrorContext {
        &self.error_context
    }
}

// =============================================================================
// Health Status
// =============================================================================

pub struct HealthStatus {
    pub memory_ok: bool,
    pub ble_ok: bool,
    pub sensors_ok: bool,
    pub watchdog_ok: bool,
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self {
            memory_ok: true,
            ble_ok: true,
            sensors_ok: true,
            watchdog_ok: true,
        }
    }
}

impl HealthStatus {
    pub fn check_all(&mut self, watchdog: &Watchdog, current_time_ms: u64) {
        self.memory_ok = true;
        self.ble_ok = true;
        self.sensors_ok = true;
        self.watchdog_ok = !watchdog.check_timeout(current_time_ms);
    }

    pub fn score(&self) -> u8 {
        let mut score = 100u8;
        if !self.memory_ok { score = score.saturating_sub(20); }
        if !self.ble_ok { score = score.saturating_sub(30); }
        if !self.sensors_ok { score = score.saturating_sub(30); }
        if !self.watchdog_ok { score = score.saturating_sub(40); }
        score
    }

    pub fn is_healthy(&self) -> bool {
        self.score() >= 80
    }
}
