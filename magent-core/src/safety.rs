//! Aerospace-grade safety mechanisms for mAgent
//!
//! This module provides safety-critical components including:
//! - Budget enforcement (iteration, memory, time)
//! - Watchdog timer integration
//! - Stack depth monitoring
//! - Memory guards
//! - Fault detection and recovery

use crate::error::{AgentError, Result};
use core::cell::Cell;

// On bare-metal targets whose `core::sync::atomic` only supports
// `load`/`store` (e.g. `riscv32imc-unknown-none-elf` — the `rv32imc`
// ISA lacks the `A` extension that provides `lr/sc`), use
// `portable-atomic` for the budget / depth / error counters. The
// selection mirrors `portable-atomic`'s own internal cfg so the
// behavior matches what `heapless` / `futures-core` use on the same
// target.
#[cfg(any(
    not(target_has_atomic = "ptr"),
    all(target_arch = "riscv32", not(target_feature = "a"))
))]
mod atomic_int {
    pub use portable_atomic::AtomicUsize;
    pub use portable_atomic::Ordering;
}
#[cfg(not(any(
    not(target_has_atomic = "ptr"),
    all(target_arch = "riscv32", not(target_feature = "a"))
)))]
mod atomic_int {
    pub use core::sync::atomic::AtomicUsize;
    pub use core::sync::atomic::Ordering;
}
use atomic_int::{AtomicUsize, Ordering};

// Re-export critical-section for no_std compatibility. Only the chip-family
// features pull in the `critical-section` crate (via their arch deps); a bare
// host `--features std` build has no such dependency, so the alias is gated
// out there. It is only a compatibility re-export (unused in this module).
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
#[allow(unused_imports)]
use critical_section::Mutex as CriticalSection;

/// Budget enforcer for resource limits
pub struct BudgetEnforcer {
    iteration_budget: AtomicUsize,
    iteration_limit: usize,
    memory_budget: AtomicUsize,
    memory_limit: usize,
    time_budget: AtomicUsize,
    #[allow(dead_code)]
    time_limit_ms: u32,
}

impl BudgetEnforcer {
    /// Create a new budget enforcer with specified limits
    pub fn new(iteration_limit: usize, memory_limit: usize, time_limit_ms: u32) -> Self {
        Self {
            iteration_budget: AtomicUsize::new(0),
            iteration_limit,
            memory_budget: AtomicUsize::new(0),
            memory_limit,
            time_budget: AtomicUsize::new(0),
            time_limit_ms,
        }
    }

    /// Create with default limits
    pub fn with_defaults() -> Self {
        Self::new(
            crate::MAX_ITERATION_BUDGET,
            crate::MAX_MEMORY_BUDGET,
            10000, // 10 seconds
        )
    }

    /// Check and consume iteration budget
    pub fn consume_iteration(&self) -> Result<()> {
        let used = self.iteration_budget.fetch_add(1, Ordering::SeqCst);
        if used >= self.iteration_limit {
            return Err(AgentError::IterationBudgetExhausted {
                used: used + 1,
                limit: self.iteration_limit,
            });
        }
        Ok(())
    }

    /// Check and consume memory budget
    pub fn consume_memory(&self, bytes: usize) -> Result<()> {
        let used = self.memory_budget.fetch_add(bytes, Ordering::SeqCst);
        if used + bytes > self.memory_limit {
            self.memory_budget.fetch_sub(bytes, Ordering::SeqCst);
            return Err(AgentError::MemoryBudgetExhausted {
                used: used + bytes,
                limit: self.memory_limit,
            });
        }
        Ok(())
    }

    /// Release memory budget
    pub fn release_memory(&self, bytes: usize) {
        let current = self.memory_budget.load(Ordering::SeqCst);
        let new = current.saturating_sub(bytes);
        self.memory_budget.store(new, Ordering::SeqCst);
    }

    /// Reset iteration budget
    pub fn reset_iteration(&self) {
        self.iteration_budget.store(0, Ordering::SeqCst);
    }

    /// Reset memory budget
    pub fn reset_memory(&self) {
        self.memory_budget.store(0, Ordering::SeqCst);
    }

    /// Reset time budget
    pub fn reset_time(&self) {
        self.time_budget.store(0, Ordering::SeqCst);
    }

    /// Get current iteration usage
    pub fn iteration_usage(&self) -> usize {
        self.iteration_budget.load(Ordering::SeqCst)
    }

    /// Get current memory usage
    pub fn memory_usage(&self) -> usize {
        self.memory_budget.load(Ordering::SeqCst)
    }

    /// Get iteration limit
    pub fn iteration_limit(&self) -> usize {
        self.iteration_limit
    }

    /// Get memory limit
    pub fn memory_limit(&self) -> usize {
        self.memory_limit
    }
}

/// Watchdog timer interface
pub struct Watchdog {
    fed: Cell<bool>,
    timeout_ms: u32,
}

impl Watchdog {
    /// Create a new watchdog with specified timeout
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            fed: Cell::new(true),
            timeout_ms,
        }
    }

    /// Create with default timeout
    pub fn with_defaults() -> Self {
        Self::new((crate::WATCHDOG_TIMEOUT_SECS * 1000) as u32)
    }

    /// Feed the watchdog (reset timer)
    pub fn feed(&self) {
        self.fed.set(true);
        // In real implementation, this would reset the hardware watchdog
        // For now, we simulate it
    }

    /// Check if watchdog needs feeding
    pub fn needs_feed(&self) -> bool {
        !self.fed.get()
    }

    /// Simulate watchdog timeout (for testing)
    pub fn simulate_timeout(&self) {
        self.fed.set(false);
    }

    /// Get timeout in milliseconds
    pub fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }
}

/// Stack depth monitor
pub struct StackMonitor {
    stack_base: usize,
    stack_limit: usize,
    current_depth: AtomicUsize,
}

impl StackMonitor {
    /// Create a new stack monitor
    pub fn new(stack_base: usize, stack_limit: usize) -> Self {
        Self {
            stack_base,
            stack_limit,
            current_depth: AtomicUsize::new(0),
        }
    }

    /// Create with default stack size
    pub fn with_defaults() -> Self {
        Self::new(0, crate::AGENT_STACK_SIZE)
    }

    /// Check stack depth
    pub fn check_depth(&self, current_sp: usize) -> Result<()> {
        let depth = self.stack_base.saturating_sub(current_sp);
        self.current_depth.store(depth, Ordering::SeqCst);

        if depth > self.stack_limit {
            return Err(AgentError::StackOverflow {
                used: depth,
                limit: self.stack_limit,
            });
        }
        Ok(())
    }

    /// Get current stack depth
    pub fn current_depth(&self) -> usize {
        self.current_depth.load(Ordering::SeqCst)
    }

    /// Get stack limit
    pub fn stack_limit(&self) -> usize {
        self.stack_limit
    }
}

/// Memory guard for buffer operations
pub struct MemoryGuard {
    capacity: usize,
    used: AtomicUsize,
}

impl MemoryGuard {
    /// Create a new memory guard
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: AtomicUsize::new(0),
        }
    }

    /// Allocate memory from guard
    pub fn allocate(&self, size: usize) -> Result<()> {
        let current = self.used.load(Ordering::SeqCst);
        if current + size > self.capacity {
            return Err(AgentError::BufferOverflow {
                capacity: self.capacity,
                attempted: current + size,
            });
        }
        self.used.fetch_add(size, Ordering::SeqCst);
        Ok(())
    }

    /// Free memory from guard
    pub fn free(&self, size: usize) {
        let current = self.used.load(Ordering::SeqCst);
        let new = current.saturating_sub(size);
        self.used.store(new, Ordering::SeqCst);
    }

    /// Get current usage
    pub fn usage(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    /// Get capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Fault detector for monitoring system health
pub struct FaultDetector {
    error_count: AtomicUsize,
    error_threshold: usize,
    last_error_type: Cell<Option<crate::error::ErrorCategory>>,
}

impl FaultDetector {
    /// Create a new fault detector
    pub fn new(error_threshold: usize) -> Self {
        Self {
            error_count: AtomicUsize::new(0),
            error_threshold,
            last_error_type: Cell::new(None),
        }
    }

    /// Create with default threshold
    pub fn with_defaults() -> Self {
        Self::new(10) // 10 errors before fault
    }

    /// Report an error
    pub fn report_error(&self, error: &AgentError) -> Result<()> {
        let count = self.error_count.fetch_add(1, Ordering::SeqCst);

        self.last_error_type.set(Some(error.category()));

        if count >= self.error_threshold {
            return Err(AgentError::Unknown { code: 0xDEAD });
        }
        Ok(())
    }

    /// Reset error count
    pub fn reset(&self) {
        self.error_count.store(0, Ordering::SeqCst);
        self.last_error_type.set(None);
    }

    /// Get error count
    pub fn error_count(&self) -> usize {
        self.error_count.load(Ordering::SeqCst)
    }

    /// Get last error type
    pub fn last_error_type(&self) -> Option<crate::error::ErrorCategory> {
        self.last_error_type.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AgentError, ErrorCategory};

    #[test]
    fn budget_enforcer_iteration_budget() {
        let b = BudgetEnforcer::new(3, 100, 1000);
        assert_eq!(b.iteration_limit(), 3);
        assert_eq!(b.iteration_usage(), 0);
        assert!(b.consume_iteration().is_ok());
        assert!(b.consume_iteration().is_ok());
        assert!(b.consume_iteration().is_ok());
        assert_eq!(b.iteration_usage(), 3);
        // 4th iteration exceeds the limit.
        let err = b.consume_iteration().unwrap_err();
        assert!(matches!(err, AgentError::IterationBudgetExhausted { .. }));
        b.reset_iteration();
        assert_eq!(b.iteration_usage(), 0);
    }

    #[test]
    fn budget_enforcer_memory_budget() {
        let b = BudgetEnforcer::new(10, 100, 1000);
        assert_eq!(b.memory_limit(), 100);
        assert!(b.consume_memory(40).is_ok());
        assert!(b.consume_memory(40).is_ok());
        assert_eq!(b.memory_usage(), 80);
        // Exceeding the limit rejects and does NOT change usage.
        let err = b.consume_memory(50).unwrap_err();
        assert!(matches!(err, AgentError::MemoryBudgetExhausted { .. }));
        assert_eq!(b.memory_usage(), 80);
        // Release drops usage with saturating semantics.
        b.release_memory(30);
        assert_eq!(b.memory_usage(), 50);
        b.release_memory(1000);
        assert_eq!(b.memory_usage(), 0);
        b.reset_memory();
        assert_eq!(b.memory_usage(), 0);
    }

    #[test]
    fn watchdog_feed_and_timeout() {
        let w = Watchdog::new(500);
        assert_eq!(w.timeout_ms(), 500);
        assert!(!w.needs_feed()); // newly created, already fed
        w.simulate_timeout();
        assert!(w.needs_feed());
        w.feed();
        assert!(!w.needs_feed());
    }

    #[test]
    fn stack_monitor_depth_checks() {
        let m = StackMonitor::new(1000, 100);
        assert_eq!(m.stack_limit(), 100);
        // Small depth is fine.
        assert!(m.check_depth(950).is_ok());
        assert_eq!(m.current_depth(), 50);
        // Large depth (low SP) overflows.
        let err = m.check_depth(500).unwrap_err();
        assert!(matches!(err, AgentError::StackOverflow { .. }));
        assert_eq!(m.current_depth(), 500);
    }

    #[test]
    fn memory_guard_allocate_free() {
        let g = MemoryGuard::new(100);
        assert_eq!(g.capacity(), 100);
        assert_eq!(g.usage(), 0);
        assert!(g.allocate(40).is_ok());
        assert!(g.allocate(40).is_ok());
        assert_eq!(g.usage(), 80);
        // Overflow is rejected without mutating usage.
        let err = g.allocate(50).unwrap_err();
        assert!(matches!(err, AgentError::BufferOverflow { .. }));
        assert_eq!(g.usage(), 80);
        g.free(30);
        assert_eq!(g.usage(), 50);
        g.free(1000); // saturating free
        assert_eq!(g.usage(), 0);
    }

    #[test]
    fn fault_detector_counts_and_resets() {
        // `report_error` compares the *pre-increment* count against the
        // threshold, so with threshold=2 the first two reports succeed and
        // the third trips the fault.
        let d = FaultDetector::new(2);
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.last_error_type(), None);
        assert!(d.report_error(&AgentError::Unknown { code: 0 }).is_ok());
        assert!(d.report_error(&AgentError::Unknown { code: 0 }).is_ok());
        assert_eq!(d.error_count(), 2);
        assert_eq!(d.last_error_type(), Some(ErrorCategory::Unknown));
        // The 3rd error crosses the threshold and reports a fatal fault.
        let err = d
            .report_error(&AgentError::Unknown { code: 0 })
            .unwrap_err();
        assert!(matches!(err, AgentError::Unknown { code: 0xDEAD }));
        assert_eq!(d.error_count(), 3);
        d.reset();
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.last_error_type(), None);
    }

    #[test]
    fn safety_components_with_defaults() {
        let b = BudgetEnforcer::with_defaults();
        assert_eq!(b.iteration_limit(), crate::MAX_ITERATION_BUDGET);
        assert_eq!(b.memory_limit(), crate::MAX_MEMORY_BUDGET);
        let w = Watchdog::with_defaults();
        assert_eq!(w.timeout_ms(), (crate::WATCHDOG_TIMEOUT_SECS * 1000) as u32);
        let m = StackMonitor::with_defaults();
        assert_eq!(m.stack_limit(), crate::AGENT_STACK_SIZE);
        let d = FaultDetector::with_defaults();
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.last_error_type(), None);
        b.reset_time();
        assert_eq!(b.iteration_usage(), 0);
    }
}
