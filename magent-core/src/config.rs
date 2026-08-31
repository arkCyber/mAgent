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
/// Default reflects the ESP32-C61 N8R2 (320 KB internal SRAM + 2 MB in-package
/// PSRAM): a 1 MiB safety ceiling on the `std::alloc` heap. When compiled with
/// `--features s3-8mb-psram` (the ESP32-S3-WROOM-1-N8R8, 8 MB octal PSRAM), the
/// ceiling is raised to 4 MiB — still deliberately well below the 8 MB pool so
/// a runaway LLM reply or a long context cache cannot exhaust PSRAM
/// (heap-blast protection, REQ-SCHED-001 / mem-1). Kept as one constant so the
/// validation and the builder cannot drift apart.
#[cfg(feature = "s3-8mb-psram")]
pub const MAX_CONFIGURABLE_MEMORY: u32 = 4 * 1024 * 1024; // 4 MiB — S3 8 MB octal PSRAM
/// C61 / default 2 MB PSRAM ceiling: 1 MiB (bounded to prevent heap-blast).
#[cfg(not(feature = "s3-8mb-psram"))]
pub const MAX_CONFIGURABLE_MEMORY: u32 = 1024 * 1024; // 1 MiB — C61 2 MB PSRAM

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
        // Upper bound is the board's safety ceiling (see MAX_CONFIGURABLE_MEMORY):
        // 1 MiB on the C61's 2 MB PSRAM, 4 MiB on the S3's 8 MB octal PSRAM. A
        // value above it is a configuration error — it would let a runaway LLM
        // reply or an unbounded context cache exhaust PSRAM (heap-blast guard).
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
        let buf =
            postcard::to_vec::<Self, 256>(self).map_err(|_| AgentError::ConfigurationError {
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
        assert!(AgentConfig::default().with_max_memory(512 * 1024).is_ok());
        assert!(AgentConfig::default()
            .with_max_memory(MAX_CONFIGURABLE_MEMORY)
            .is_ok());
        assert!(AgentConfig::default()
            .with_max_memory(MAX_CONFIGURABLE_MEMORY + 1)
            .is_err());
        assert!(AgentConfig::default().with_max_memory(0).is_err());
    }

    #[cfg(feature = "s3-8mb-psram")]
    #[test]
    fn s3_ceiling_is_raised_but_still_bounded() {
        // The S3 8 MB octal PSRAM profile raises the budget ceiling to 4 MiB
        // but still rejects anything above it (heap-blast guard) and zero.
        assert_eq!(MAX_CONFIGURABLE_MEMORY, 4 * 1024 * 1024);
        assert!(AgentConfig::default()
            .with_max_memory(4 * 1024 * 1024)
            .is_ok());
        assert!(AgentConfig::default()
            .with_max_memory(4 * 1024 * 1024 + 1)
            .is_err());
        assert!(AgentConfig::default().with_max_memory(0).is_err());
    }

    #[cfg(not(feature = "s3-8mb-psram"))]
    #[test]
    fn default_ceiling_is_1_mib() {
        // Without the S3 large-heap profile (C61 / host), the ceiling stays
        // at the C61's 2 MB PSRAM budget of 1 MiB.
        assert_eq!(MAX_CONFIGURABLE_MEMORY, 1024 * 1024);
    }

    #[test]
    fn validate_rejects_empty_name() {
        let err = AgentConfig {
            name: heapless::String::new(),
            ..AgentConfig::default()
        }
        .validate()
        .unwrap_err();
        assert!(matches!(
            err,
            AgentError::ConfigurationError {
                field: "name",
                reason: ConfigError::Empty
            }
        ));
    }

    #[test]
    fn validate_rejects_bad_ranges() {
        // max_iterations
        assert!(matches!(
            AgentConfig {
                max_iterations: 0,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_iterations",
                ..
            })
        ));
        assert!(matches!(
            AgentConfig {
                max_iterations: 1001,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_iterations",
                ..
            })
        ));

        // max_memory
        assert!(matches!(
            AgentConfig {
                max_memory: 0,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_memory",
                ..
            })
        ));
        assert!(matches!(
            AgentConfig {
                max_memory: MAX_CONFIGURABLE_MEMORY + 1,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_memory",
                ..
            })
        ));

        // watchdog (upper bound)
        assert!(matches!(
            AgentConfig {
                watchdog_timeout_secs: 61,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "watchdog_timeout_secs",
                ..
            })
        ));

        // ble (upper bound)
        assert!(matches!(
            AgentConfig {
                ble_timeout_secs: 121,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "ble_timeout_secs",
                ..
            })
        ));

        // max_skills (lower + upper)
        assert!(matches!(
            AgentConfig {
                max_skills: 0,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_skills",
                ..
            })
        ));
        assert!(matches!(
            AgentConfig {
                max_skills: 51,
                ..AgentConfig::default()
            }
            .validate(),
            Err(AgentError::ConfigurationError {
                field: "max_skills",
                ..
            })
        ));
    }

    #[test]
    fn validate_accepts_defaults() {
        assert!(AgentConfig::default().validate().is_ok());
    }

    #[test]
    fn builder_with_name_validates_length() {
        let ok = AgentConfig::default().with_name("assistant").unwrap();
        assert_eq!(ok.name.as_str(), "assistant");
        assert!(AgentConfig::default().with_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn builder_with_max_iterations_validates() {
        assert_eq!(
            AgentConfig::default()
                .with_max_iterations(50)
                .unwrap()
                .max_iterations,
            50
        );
        assert!(AgentConfig::default().with_max_iterations(0).is_err());
        assert!(AgentConfig::default().with_max_iterations(1001).is_err());
    }

    #[test]
    fn bytes_round_trip() {
        let c = AgentConfig::default()
            .with_name("prod-agent")
            .unwrap()
            .with_max_iterations(42)
            .unwrap();
        let bytes = c.to_bytes().unwrap();
        let back = AgentConfig::from_bytes(&bytes).unwrap();
        assert_eq!(back.name, c.name);
        assert_eq!(back.max_iterations, c.max_iterations);
        assert_eq!(back.max_memory, c.max_memory);
        // Malformed bytes must be rejected as an error, never panic.
        assert!(AgentConfig::from_bytes(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }
}
