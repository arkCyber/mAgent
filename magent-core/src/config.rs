//! Configuration management for mAgent
//!
//! Provides configuration loading, validation, and management
//! with aerospace-grade safety checks.

use crate::error::{try_heapless, AgentError, ConfigError, Result};
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum configuration field length
const MAX_FIELD_LENGTH: usize = 64;

/// Upper bound for the configurable agent memory budget (bytes).
///
/// Reflects the ESP32-C61 N8R2 target (320 KB internal SRAM + 2 MB in-package
/// PSRAM). 1 MiB is a deliberate safety ceiling on the `std::alloc` heap; the
/// agent's own `heapless` buffers stay well below it, and larger budgets exist
/// for context stored on the 2 MB PSRAM heap. Kept as one constant so the
/// validation and the builder cannot drift apart.
pub const MAX_CONFIGURABLE_MEMORY: u32 = 1024 * 1024; // 1 MiB

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent name
    pub name: String<MAX_FIELD_LENGTH>,
    /// Maximum iterations per task
    pub max_iterations: u16,
    /// Maximum memory budget in bytes
    pub max_memory: u32,
    /// Watchdog timeout in seconds
    pub watchdog_timeout_secs: u16,
    /// BLE connection timeout in seconds
    pub ble_timeout_secs: u16,
    /// Enable skills system
    pub skills_enabled: bool,
    /// Maximum number of skills
    pub max_skills: u16,
    /// Enable debug logging
    pub debug_enabled: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            // HARDENING (audit-2026-08 unwrap sweep): use `try_heapless` so a
            // future rename of the default agent name cannot introduce
            // a panic on platforms with tight < 7-byte string limits.
            name: try_heapless::<64>("mAgent"),
            max_iterations: crate::MAX_ITERATION_BUDGET as u16,
            max_memory: crate::MAX_MEMORY_BUDGET as u32,
            watchdog_timeout_secs: crate::WATCHDOG_TIMEOUT_SECS as u16,
            ble_timeout_secs: 30,
            skills_enabled: true,
            max_skills: 10,
            debug_enabled: false,
        }
    }
}

impl AgentConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        // Validate name
        if self.name.is_empty() {
            return Err(AgentError::ConfigurationError {
                field: "name",
                reason: ConfigError::Empty,
            });
        }

        // Validate max_iterations
        if self.max_iterations == 0 {
            return Err(AgentError::ConfigurationError {
                field: "max_iterations",
                reason: ConfigError::OutOfRange,
            });
        }
        if self.max_iterations > 1000 {
            return Err(AgentError::ConfigurationError {
                field: "max_iterations",
                reason: ConfigError::OutOfRange,
            });
        }

        // Validate max_memory
        if self.max_memory == 0 {
            return Err(AgentError::ConfigurationError {
                field: "max_memory",
                reason: ConfigError::OutOfRange,
            });
        }
        // Upper bound reflects the ESP32-C61 N8R2 target (320 KB internal
        // SRAM + 2 MB in-package PSRAM). 1 MiB is a deliberate safety ceiling
        // for the std::alloc heap — the agent's heapless buffers stay far
        // below it, and larger budgets are for context that lives on the
        // 2 MB PSRAM heap. (Historically this was hard-capped at 256 KiB.)
        if self.max_memory > MAX_CONFIGURABLE_MEMORY {
            return Err(AgentError::ConfigurationError {
                field: "max_memory",
                reason: ConfigError::OutOfRange,
            });
        }

        // Validate watchdog_timeout_secs
        if self.watchdog_timeout_secs == 0 {
            return Err(AgentError::ConfigurationError {
                field: "watchdog_timeout_secs",
                reason: ConfigError::OutOfRange,
            });
        }
        if self.watchdog_timeout_secs > 60 {
            return Err(AgentError::ConfigurationError {
                field: "watchdog_timeout_secs",
                reason: ConfigError::OutOfRange,
            });
        }

        // Validate ble_timeout_secs
        if self.ble_timeout_secs == 0 {
            return Err(AgentError::ConfigurationError {
                field: "ble_timeout_secs",
                reason: ConfigError::OutOfRange,
            });
        }
        if self.ble_timeout_secs > 120 {
            return Err(AgentError::ConfigurationError {
                field: "ble_timeout_secs",
                reason: ConfigError::OutOfRange,
            });
        }

        // Validate max_skills
        if self.max_skills == 0 {
            return Err(AgentError::ConfigurationError {
                field: "max_skills",
                reason: ConfigError::OutOfRange,
            });
        }
        if self.max_skills > 50 {
            return Err(AgentError::ConfigurationError {
                field: "max_skills",
                reason: ConfigError::OutOfRange,
            });
        }

        Ok(())
    }

    /// Set agent name
    pub fn with_name(mut self, name: &str) -> Result<Self> {
        if name.len() > MAX_FIELD_LENGTH {
            return Err(AgentError::ConfigurationError {
                field: "name",
                reason: ConfigError::TooLong,
            });
        }
        self.name = heapless::String::try_from(name).unwrap_or_else(|_| heapless::String::new());
        Ok(self)
    }

    /// Set max iterations
    pub fn with_max_iterations(mut self, max: u16) -> Result<Self> {
        if max == 0 || max > 1000 {
            return Err(AgentError::ConfigurationError {
                field: "max_iterations",
                reason: ConfigError::OutOfRange,
            });
        }
        self.max_iterations = max;
        Ok(self)
    }

    /// Set max memory
    pub fn with_max_memory(mut self, max: u32) -> Result<Self> {
        if max == 0 || max > MAX_CONFIGURABLE_MEMORY {
            return Err(AgentError::ConfigurationError {
                field: "max_memory",
                reason: ConfigError::OutOfRange,
            });
        }
        self.max_memory = max;
        Ok(self)
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8, 256>> {
        // PATCHED (MicroAgent): `postcard::to_vec` returns
        // `heapless::Vec<u8, N>` (from postcard's transitive
        // `heapless 0.7` dep). Specify `N` explicitly so the
        // compiler doesn't have to infer it, then copy into our
        // `heapless 0.9` `Vec`.
        let buf = postcard::to_vec::<Self, 256>(self)
            .map_err(|_| AgentError::ConfigurationError {
                field: "serialization",
                reason: ConfigError::TypeMismatch,
            })?;
        let mut out = Vec::<u8, 256>::new();
        let take = buf.len().min(256);
        for &b in &buf.as_slice()[..take] {
            let _ = out.push(b);
        }
        Ok(out)
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|_| AgentError::ConfigurationError {
            field: "deserialization",
            reason: ConfigError::TypeMismatch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_memory_allows_full_psram_budget() {
        // The ESP32-C61 N8R2 has 2 MB PSRAM; the configurable budget must
        // permit a 512 KiB budget (what the firmware requests) and the 1 MiB
        // ceiling. It must still reject absurd values.
        assert!(AgentConfig::default()
            .with_max_memory(512 * 1024)
            .is_ok());
        assert!(AgentConfig::default()
            .with_max_memory(MAX_CONFIGURABLE_MEMORY)
            .is_ok());
        assert!(AgentConfig::default()
            .with_max_memory(MAX_CONFIGURABLE_MEMORY + 1)
            .is_err());
        assert!(AgentConfig::default().with_max_memory(0).is_err());
    }
}

