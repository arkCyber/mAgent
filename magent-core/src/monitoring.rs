//! Monitoring and logging for mAgent
//!
//! This module provides monitoring capabilities for the agent
//! including performance metrics, health checks, and event logging.

use crate::error::{AgentError, Result};
use heapless::{String, Vec};

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Verbose debugging — typically compiled out in release.
    Debug,
    /// Normal operation events.
    Info,
    /// Recoverable problems the operator should know about.
    Warning,
    /// Failures requiring intervention.
    Error,
}

/// Log entry
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Severity bucket this entry belongs to.
    pub level: LogLevel,
    /// Formatted, heapless log message.
    pub message: String<256>,
    /// RTC ticks (seconds since boot) at which the entry was recorded.
    pub timestamp: u32,
}

/// Performance metrics
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    /// Cumulative count of operations observed since startup.
    pub total_operations: u32,
    /// Subset of [`Self::total_operations`] that completed without error.
    pub successful_operations: u32,
    /// Subset of [`Self::total_operations`] that raised an error.
    pub failed_operations: u32,
    /// Rolling average execution time, in microseconds.
    pub average_execution_time_us: u32,
    /// Highest single-observation heap usage, in bytes.
    pub peak_memory_usage: u32,
}

/// Health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Component is operating within nominal bounds.
    Healthy,
    /// Component is up but reporting one or more non-fatal issues.
    Degraded,
    /// Component has failed and should be treated as unusable.
    Unhealthy,
}

/// Health check result
#[derive(Debug, Clone)]
pub struct HealthCheck {
    /// Logical component name (e.g. `"ble"`, `"sensor:hr"`).
    pub component: String<32>,
    /// Latest status verdict for this component.
    pub status: HealthStatus,
    /// Optional human-readable detail (empty when no extra context).
    pub message: String<128>,
}

/// Monitoring manager
pub struct MonitoringManager {
    logs: Vec<LogEntry, 64>,
    metrics: PerformanceMetrics,
    health_checks: Vec<HealthCheck, 16>,
}

impl MonitoringManager {
    /// Create a new monitoring manager
    pub fn new() -> Self {
        Self {
            logs: Vec::new(),
            metrics: PerformanceMetrics {
                total_operations: 0,
                successful_operations: 0,
                failed_operations: 0,
                average_execution_time_us: 0,
                peak_memory_usage: 0,
            },
            health_checks: Vec::new(),
        }
    }

    /// Create with default settings
    pub fn with_defaults() -> Self {
        Self::new()
    }

    /// Log a message
    pub fn log(&mut self, level: LogLevel, message: &str) -> Result<()> {
        let entry = LogEntry {
            level,
            message: String::try_from(message).map_err(|_| AgentError::MemoryAllocationFailed {
                requested: 256,
                available: 0,
            })?,
            timestamp: 0, // In real implementation, use embassy-time
        };

        if self.logs.push(entry.clone()).is_err() {
            // Log buffer full, remove oldest
            let _ = self.logs.remove(0);
            let _ = self.logs.push(entry.clone());
        }

        Ok(())
    }

    /// Get recent logs
    pub fn get_logs(&self) -> &[LogEntry] {
        &self.logs
    }

    /// Record operation start
    pub fn operation_start(&mut self) {
        self.metrics.total_operations += 1;
    }

    /// Record operation success
    pub fn operation_success(&mut self, execution_time_us: u32) {
        self.metrics.successful_operations += 1;
        
        // Update average execution time
        let total_time = self.metrics.average_execution_time_us * (self.metrics.successful_operations - 1);
        self.metrics.average_execution_time_us = (total_time + execution_time_us) / self.metrics.successful_operations;
    }

    /// Record operation failure
    pub fn operation_failure(&mut self) {
        self.metrics.failed_operations += 1;
    }

    /// Get performance metrics
    pub fn get_metrics(&self) -> &PerformanceMetrics {
        &self.metrics
    }

    /// Add health check
    pub fn add_health_check(&mut self, component: &str, status: HealthStatus, message: &str) -> Result<()> {
        let check = HealthCheck {
            component: String::try_from(component).map_err(|_| AgentError::MemoryAllocationFailed {
                requested: 32,
                available: 0,
            })?,
            status,
            message: String::try_from(message).map_err(|_| AgentError::MemoryAllocationFailed {
                requested: 128,
                available: 0,
            })?,
        };

        if self.health_checks.push(check.clone()).is_err() {
            // Health check buffer full, remove oldest
            let _ = self.health_checks.remove(0);
            let _ = self.health_checks.push(check);
        }

        Ok(())
    }

    /// Get overall health status
    pub fn get_health_status(&self) -> HealthStatus {
        if self.health_checks.is_empty() {
            return HealthStatus::Healthy;
        }

        let mut unhealthy_count = 0;
        let mut degraded_count = 0;

        for check in &self.health_checks {
            match check.status {
                HealthStatus::Unhealthy => unhealthy_count += 1,
                HealthStatus::Degraded => degraded_count += 1,
                HealthStatus::Healthy => {}
            }
        }

        if unhealthy_count > 0 {
            HealthStatus::Unhealthy
        } else if degraded_count > 0 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        }
    }

    /// Get health checks
    pub fn get_health_checks(&self) -> &[HealthCheck] {
        &self.health_checks
    }

    /// Clear logs
    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    /// Reset metrics
    pub fn reset_metrics(&mut self) {
        self.metrics = PerformanceMetrics {
            total_operations: 0,
            successful_operations: 0,
            failed_operations: 0,
            average_execution_time_us: 0,
            peak_memory_usage: 0,
        };
    }
}

impl Default for MonitoringManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_stores_entries_in_order() {
        let mut m = MonitoringManager::new();
        m.log(LogLevel::Info, "boot").unwrap();
        m.log(LogLevel::Warning, "low battery").unwrap();
        let logs = m.get_logs();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].level, LogLevel::Info);
        assert_eq!(logs[0].message.as_str(), "boot");
        assert_eq!(logs[1].level, LogLevel::Warning);
    }

    #[test]
    fn log_rejects_message_over_buffer() {
        let mut m = MonitoringManager::new();
        let long = "x".repeat(300);
        let r = m.log(LogLevel::Info, &long);
        assert!(matches!(r, Err(AgentError::MemoryAllocationFailed { .. })));
        assert_eq!(m.get_logs().len(), 0, "failed log must not be appended");
    }

    #[test]
    fn log_buffer_evicts_oldest_when_full() {
        let mut m = MonitoringManager::new();
        for i in 0..70 {
            m.log(LogLevel::Debug, &format!("entry {}", i)).unwrap();
        }
        // Capacity is 64; the 6 oldest are evicted.
        assert_eq!(m.get_logs().len(), 64);
        assert!(m.get_logs()[0].message.as_str().contains("entry 6"));
        assert!(m.get_logs()[63].message.as_str().contains("entry 69"));
        m.clear_logs();
        assert_eq!(m.get_logs().len(), 0);
    }

    #[test]
    fn performance_metrics_track_operations_and_average() {
        let mut m = MonitoringManager::new();
        m.operation_start();
        m.operation_success(100);
        assert_eq!(m.get_metrics().total_operations, 1);
        assert_eq!(m.get_metrics().successful_operations, 1);
        assert_eq!(m.get_metrics().average_execution_time_us, 100);

        m.operation_start();
        m.operation_success(200);
        // Rolling average: (100 + 200) / 2 = 150.
        assert_eq!(m.get_metrics().average_execution_time_us, 150);

        m.operation_start();
        m.operation_success(300);
        // (150*2 + 300)/3 = 200.
        assert_eq!(m.get_metrics().average_execution_time_us, 200);

        m.operation_start();
        m.operation_failure();
        assert_eq!(m.get_metrics().failed_operations, 1);
        assert_eq!(m.get_metrics().total_operations, 4);

        m.reset_metrics();
        assert_eq!(m.get_metrics().total_operations, 0);
        assert_eq!(m.get_metrics().average_execution_time_us, 0);
    }

    #[test]
    fn health_status_aggregation() {
        let mut m = MonitoringManager::new();
        // No checks → Healthy.
        assert_eq!(m.get_health_status(), HealthStatus::Healthy);

        // All healthy → Healthy.
        m.add_health_check("ble", HealthStatus::Healthy, "").unwrap();
        assert_eq!(m.get_health_status(), HealthStatus::Healthy);

        // One degraded → Degraded.
        m.add_health_check("sensor:hr", HealthStatus::Degraded, "noisy").unwrap();
        assert_eq!(m.get_health_status(), HealthStatus::Degraded);

        // Any unhealthy dominates.
        m.add_health_check("wifi", HealthStatus::Unhealthy, "link down").unwrap();
        assert_eq!(m.get_health_status(), HealthStatus::Unhealthy);
    }

    #[test]
    fn add_health_check_rejects_long_component() {
        let mut m = MonitoringManager::new();
        let long = "x".repeat(64);
        let r = m.add_health_check(&long, HealthStatus::Healthy, "");
        assert!(matches!(r, Err(AgentError::MemoryAllocationFailed { .. })));
        assert_eq!(m.get_health_checks().len(), 0);
    }

    #[test]
    fn health_check_buffer_evicts_oldest() {
        let mut m = MonitoringManager::new();
        for i in 0..20 {
            m.add_health_check(&format!("comp{}", i), HealthStatus::Healthy, "").unwrap();
        }
        // Capacity is 16; the 4 oldest are evicted.
        assert_eq!(m.get_health_checks().len(), 16);
        assert!(m.get_health_checks()[0].component.as_str().contains("comp4"));
        assert!(m.get_health_checks()[15].component.as_str().contains("comp19"));
    }
}
