//! Error recovery strategies for mAgent
//!
//! This module provides recovery strategies for different error types
//! to ensure aerospace-grade reliability and fault tolerance.

use crate::error::{AgentError, Result};

/// Recovery strategy for errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStrategy {
    /// Retry the operation immediately
    Retry,
    /// Retry with exponential backoff
    RetryWithBackoff,
    /// Use fallback value
    Fallback,
    /// Skip operation and continue
    Skip,
    /// Abort operation
    Abort,
    /// Reset component
    Reset,
}

/// Error recovery manager
pub struct RecoveryManager {
    max_retries: u8,
    backoff_base_ms: u32,
    /// Optional blocking delay hook (takes delay in ms). Installed by a host
    /// layer (e.g. a `std` build that calls `std::thread::sleep`). When `None`
    /// (the default), retries happen immediately — which is the original
    /// behaviour. Set this to make `RetryWithBackoff` actually wait.
    delay: Option<&'static dyn Fn(u32)>,
}

impl RecoveryManager {
    /// Create a new recovery manager
    pub fn new(max_retries: u8, backoff_base_ms: u32) -> Self {
        Self {
            max_retries,
            backoff_base_ms,
            delay: None,
        }
    }

    /// Create with default settings
    pub fn with_defaults() -> Self {
        Self::new(3, 100) // 3 retries, 100ms base backoff
    }

    /// Install a blocking delay hook so `RetryWithBackoff` actually waits
    /// `calculate_backoff(n)` ms between attempts.
    pub fn set_delay(&mut self, delay: &'static dyn Fn(u32)) {
        self.delay = Some(delay);
    }

    /// Get recovery strategy for error
    pub fn get_strategy(&self, error: &AgentError) -> RecoveryStrategy {
        match error {
            // Network errors: retry with backoff
            AgentError::NetworkConnectionFailed { .. } => RecoveryStrategy::RetryWithBackoff,
            AgentError::NetworkTimeout { .. } => RecoveryStrategy::RetryWithBackoff,

            // Storage errors: retry, then fallback
            AgentError::StorageReadFailed { reason, .. } => match reason {
                crate::error::StorageError::ReadError => RecoveryStrategy::Retry,
                crate::error::StorageError::CorruptedData => RecoveryStrategy::Fallback,
                _ => RecoveryStrategy::Abort,
            },
            AgentError::StorageWriteFailed { reason, .. } => match reason {
                crate::error::StorageError::WriteProtected => RecoveryStrategy::Reset,
                crate::error::StorageError::OutOfSpace => RecoveryStrategy::Fallback,
                _ => RecoveryStrategy::Retry,
            },

            // Sensor errors: retry, then fallback
            AgentError::SensorReadFailed { reason, .. } => match reason {
                crate::error::SensorError::Timeout => RecoveryStrategy::RetryWithBackoff,
                crate::error::SensorError::NotInitialized => RecoveryStrategy::Reset,
                _ => RecoveryStrategy::Fallback,
            },

            // Memory errors: abort
            AgentError::MemoryAllocationFailed { .. } => RecoveryStrategy::Abort,
            AgentError::MemoryBudgetExhausted { .. } => RecoveryStrategy::Abort,

            // Budget errors: skip
            AgentError::IterationBudgetExhausted { .. } => RecoveryStrategy::Skip,

            // Validation errors: abort
            AgentError::InputValidationFailed { .. } => RecoveryStrategy::Abort,

            // Configuration errors: reset
            AgentError::ConfigurationError { .. } => RecoveryStrategy::Reset,

            // GPIO errors: retry
            AgentError::GpioOperationFailed { .. } => RecoveryStrategy::Retry,

            // Operation timeout: retry with backoff, except the special
            // "security" timeout which is treated as fatal.
            AgentError::OperationTimeout {
                operation: "security",
                ..
            } => RecoveryStrategy::Abort,
            AgentError::OperationTimeout { .. } => RecoveryStrategy::RetryWithBackoff,

            // Corrupt / overflow state: abort (cannot be recovered by retry).
            AgentError::BufferOverflow { .. } => RecoveryStrategy::Abort,
            AgentError::StackOverflow { .. } => RecoveryStrategy::Abort,
            AgentError::InvalidStateTransition { .. } => RecoveryStrategy::Abort,

            // Security / unknown: fatal.
            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => RecoveryStrategy::Abort,
            AgentError::Unknown { .. } => RecoveryStrategy::Abort,
            AgentError::CryptoError { .. } => RecoveryStrategy::Abort,
        }
    }

    /// Calculate backoff delay for retry
    pub fn calculate_backoff(&self, retry_count: u8) -> u32 {
        if retry_count == 0 {
            return 0;
        }

        // Exponential backoff: base * 2^(retry_count - 1)
        let delay = self.backoff_base_ms * (1 << (retry_count - 1));

        // Cap at 5 seconds
        delay.min(5000)
    }

    /// Should retry operation
    pub fn should_retry(&self, error: &AgentError, retry_count: u8) -> bool {
        if retry_count >= self.max_retries {
            return false;
        }

        matches!(
            self.get_strategy(error),
            RecoveryStrategy::Retry | RecoveryStrategy::RetryWithBackoff
        )
    }

    /// Execute operation with retry logic
    pub async fn execute_with_retry<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = Result<T>>,
    {
        let mut retry_count = 0;

        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let strategy = self.get_strategy(&error);

                    match strategy {
                        RecoveryStrategy::Retry | RecoveryStrategy::RetryWithBackoff => {
                            if retry_count >= self.max_retries {
                                return Err(error);
                            }

                            retry_count += 1;

                            // PATCHED (MicroAgent): actually apply the
                            // exponential backoff instead of "just continue".
                            // If a delay hook is installed, sleep for
                            // `calculate_backoff(n)` ms between attempts; if
                            // none is set we keep the previous immediate-retry
                            // behaviour.
                            if strategy == RecoveryStrategy::RetryWithBackoff {
                                let delay_ms = self.calculate_backoff(retry_count);
                                if let Some(f) = self.delay {
                                    f(delay_ms);
                                }
                            }
                        }
                        RecoveryStrategy::Fallback => {
                            // Return error, caller should handle fallback
                            return Err(error);
                        }
                        RecoveryStrategy::Skip => {
                            // Return error, caller should skip
                            return Err(error);
                        }
                        RecoveryStrategy::Abort => {
                            return Err(error);
                        }
                        RecoveryStrategy::Reset => {
                            // In real implementation, this would reset component
                            return Err(error);
                        }
                    }
                }
            }
        }
    }
}

impl Default for RecoveryManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // FallbackProvider/etc. live below the test module on purpose.
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        let r = RecoveryManager::with_defaults();
        assert_eq!(r.calculate_backoff(0), 0);
        assert_eq!(r.calculate_backoff(1), 100); // 100 * 2^0
        assert_eq!(r.calculate_backoff(2), 200); // 100 * 2^1
        assert_eq!(r.calculate_backoff(3), 400); // 100 * 2^2
                                                 // Capped at 5000ms.
        assert_eq!(r.calculate_backoff(10), 5000);
    }

    // Uses std-only types (Arc, AtomicU32, futures::block_on) so it is gated
    // on the `std` feature — keeps `cargo test` compiling under the default
    // no_std feature set (matches the CI host job).
    #[test]
    #[cfg(feature = "std")]
    fn retry_with_backoff_applies_delay_hook() {
        use std::boxed::Box;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let total = Arc::new(AtomicU32::new(0));
        let total2 = total.clone();
        let mut r = RecoveryManager::with_defaults();
        // Record the total sleep time instead of actually sleeping. The delay
        // hook is `&'static dyn Fn`, so leak a boxed closure to get a `'static`
        // reference (fine in a test).
        let delay: &'static dyn Fn(u32) = Box::leak(Box::new(move |ms: u32| {
            total2.fetch_add(ms, Ordering::SeqCst);
        }));
        r.set_delay(delay);

        // Fails 3 times with a NetworkTimeout (RetryWithBackoff), then succeeds.
        // `execute_with_retry` requires `F: Fn()`, so we can't `move` the Arc
        // out of the closure; clone it into a fresh local for each attempt.
        let attempts = Arc::new(AtomicU32::new(0));
        let result = futures::executor::block_on(r.execute_with_retry(|| {
            let a2 = attempts.clone();
            async move {
                let n = a2.fetch_add(1, Ordering::SeqCst);
                if n < 3 {
                    Err(AgentError::NetworkTimeout {
                        operation: "test",
                        duration_ms: 1,
                    })
                } else {
                    Ok::<u8, AgentError>(42)
                }
            }
        }));
        assert_eq!(result.ok(), Some(42));
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        // Delays applied on retries 1,2,3 => 100 + 200 + 400 = 700ms.
        assert_eq!(total.load(Ordering::SeqCst), 700);
    }

    #[test]
    fn get_strategy_maps_network_and_storage() {
        use crate::error::{AgentError as E, NetworkError, SensorError, StorageError};
        let r = RecoveryManager::with_defaults();

        assert_eq!(
            r.get_strategy(&E::NetworkConnectionFailed {
                reason: NetworkError::Timeout
            }),
            RecoveryStrategy::RetryWithBackoff
        );
        assert_eq!(
            r.get_strategy(&E::NetworkTimeout {
                operation: "fetch",
                duration_ms: 1
            }),
            RecoveryStrategy::RetryWithBackoff
        );

        assert_eq!(
            r.get_strategy(&E::StorageReadFailed {
                address: 0,
                reason: StorageError::ReadError
            }),
            RecoveryStrategy::Retry
        );
        assert_eq!(
            r.get_strategy(&E::StorageReadFailed {
                address: 0,
                reason: StorageError::CorruptedData
            }),
            RecoveryStrategy::Fallback
        );
        assert_eq!(
            r.get_strategy(&E::StorageReadFailed {
                address: 0,
                reason: StorageError::BadAddress
            }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::StorageWriteFailed {
                address: 0,
                reason: StorageError::WriteProtected
            }),
            RecoveryStrategy::Reset
        );
        assert_eq!(
            r.get_strategy(&E::StorageWriteFailed {
                address: 0,
                reason: StorageError::OutOfSpace
            }),
            RecoveryStrategy::Fallback
        );
        assert_eq!(
            r.get_strategy(&E::StorageWriteFailed {
                address: 0,
                reason: StorageError::WriteError
            }),
            RecoveryStrategy::Retry
        );

        assert_eq!(
            r.get_strategy(&E::SensorReadFailed {
                sensor: "hr",
                reason: SensorError::Timeout
            }),
            RecoveryStrategy::RetryWithBackoff
        );
        assert_eq!(
            r.get_strategy(&E::SensorReadFailed {
                sensor: "hr",
                reason: SensorError::NotInitialized
            }),
            RecoveryStrategy::Reset
        );
        assert_eq!(
            r.get_strategy(&E::SensorReadFailed {
                sensor: "hr",
                reason: SensorError::InvalidValue
            }),
            RecoveryStrategy::Fallback
        );
    }

    #[test]
    fn get_strategy_maps_budget_validation_and_fatal() {
        use crate::error::{AgentError as E, ConfigError, GpioOperation, ValidationError};
        let r = RecoveryManager::with_defaults();

        assert_eq!(
            r.get_strategy(&E::MemoryAllocationFailed {
                requested: 1,
                available: 0
            }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::MemoryBudgetExhausted { used: 1, limit: 0 }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::IterationBudgetExhausted { used: 1, limit: 0 }),
            RecoveryStrategy::Skip
        );

        assert_eq!(
            r.get_strategy(&E::InputValidationFailed {
                field: "task",
                reason: ValidationError::TooLong
            }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::ConfigurationError {
                field: "model",
                reason: ConfigError::InvalidValue
            }),
            RecoveryStrategy::Reset
        );
        assert_eq!(
            r.get_strategy(&E::GpioOperationFailed {
                pin: 1,
                operation: GpioOperation::Read
            }),
            RecoveryStrategy::Retry
        );

        assert_eq!(
            r.get_strategy(&E::OperationTimeout {
                operation: "security",
                timeout_ms: 1
            }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::OperationTimeout {
                operation: "http",
                timeout_ms: 1
            }),
            RecoveryStrategy::RetryWithBackoff
        );

        assert_eq!(
            r.get_strategy(&E::BufferOverflow {
                capacity: 1,
                attempted: 2
            }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::StackOverflow { used: 1, limit: 0 }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::InvalidStateTransition { from: "a", to: "b" }),
            RecoveryStrategy::Abort
        );
        assert_eq!(
            r.get_strategy(&E::Unknown { code: 0 }),
            RecoveryStrategy::Abort
        );

        #[cfg(feature = "web3")]
        assert_eq!(
            r.get_strategy(&E::Web3Error {
                kind: crate::error::Web3ErrorKind::InvalidDid {
                    raw: "did:key:bad".into(),
                },
            }),
            RecoveryStrategy::Abort
        );
    }

    #[test]
    fn should_retry_respects_max_retries_and_strategy() {
        use crate::error::AgentError as E;
        let r = RecoveryManager::new(3, 100);

        let retryable = E::NetworkTimeout {
            operation: "fetch",
            duration_ms: 1,
        };
        assert!(r.should_retry(&retryable, 0));
        assert!(r.should_retry(&retryable, 2));
        assert!(!r.should_retry(&retryable, 3), ">= max_retries must stop");

        let abort = E::Unknown { code: 1 };
        assert!(!r.should_retry(&abort, 0));
        let skip = E::IterationBudgetExhausted { used: 1, limit: 0 };
        assert!(!r.should_retry(&skip, 0));
    }

    #[test]
    fn default_fallback_provides_zero_values() {
        let f = DefaultFallback;
        let f32v: f32 = f.get_fallback();
        let u32v: u32 = f.get_fallback();
        let boolv: bool = f.get_fallback();
        assert_eq!(f32v, 0.0);
        assert_eq!(u32v, 0);
        assert!(!boolv);
    }

    #[test]
    fn new_and_defaults_set_expected_limits() {
        let r = RecoveryManager::new(0, 5);
        assert_eq!(r.max_retries, 0);
        assert_eq!(r.backoff_base_ms, 5);
        let d = RecoveryManager::default();
        assert_eq!(d.max_retries, 3);
        assert_eq!(d.backoff_base_ms, 100);
        assert!(d.delay.is_none());
    }

    // ============================================================================
    // TRACE: AEROSPACE-AUDIT-2026-09-15
    // execute_with_retry must respect every RecoveryStrategy variant, not just
    // the Retry/RetryWithBackoff paths tested above. Each non-Retry variant
    // (Fallback, Skip, Abort, Reset) MUST return the operation's Err on the
    // first failure — no silent retry, no swallowed error. These tests pin
    // the contract so a future refactor of `execute_with_retry` cannot
    // accidentally demote any non-Retry strategy into a retry loop.
    // ============================================================================
    #[cfg(feature = "std")]
    mod execute_with_retry_strategy_coverage {
        use super::*;
        use crate::error::{AgentError as E, ValidationError};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        /// Drives `execute_with_retry` with a closure that returns a fresh
        /// clone of `error` on every invocation. Returns `(result, attempts)`.
        /// We hand-clone the error each call because `F: Fn()` must own a
        /// fresh future each invocation.
        async fn drive_once(r: &RecoveryManager, error: E) -> (Result<u8>, u32) {
            let attempts = Arc::new(AtomicU32::new(0));
            let attempts_for_closure = attempts.clone();
            let result = r
                .execute_with_retry(move || {
                    let a = attempts_for_closure.clone();
                    let error_clone = clone_error(&error);
                    async move {
                        a.fetch_add(1, Ordering::SeqCst);
                        Err::<u8, E>(error_clone)
                    }
                })
                .await;
            (result, attempts.load(Ordering::SeqCst))
        }

        /// Hand-rolled clone: every variant we use in this module holds only
        /// `Copy` / `&'static str` fields, so a `match`-based reconstruction
        /// is sufficient and avoids forcing `AgentError: Clone`.
        fn clone_error(e: &E) -> E {
            match e {
                E::InputValidationFailed { field, reason } => E::InputValidationFailed {
                    field,
                    reason: *reason,
                },
                E::IterationBudgetExhausted { used, limit } => E::IterationBudgetExhausted {
                    used: *used,
                    limit: *limit,
                },
                E::MemoryAllocationFailed { requested, available } => E::MemoryAllocationFailed {
                    requested: *requested,
                    available: *available,
                },
                E::Unknown { code } => E::Unknown { code: *code },
                E::StorageReadFailed { address, reason } => E::StorageReadFailed {
                    address: *address,
                    reason: *reason,
                },
                E::StorageWriteFailed { address, reason } => E::StorageWriteFailed {
                    address: *address,
                    reason: *reason,
                },
                E::OperationTimeout {
                    operation,
                    timeout_ms,
                } => E::OperationTimeout {
                    operation,
                    timeout_ms: *timeout_ms,
                },
                _ => E::Unknown { code: 0 }, // not exercised in these tests
            }
        }

        #[test]
        fn abort_strategy_runs_exactly_once() {
            let r = RecoveryManager::with_defaults();
            let err = E::InputValidationFailed {
                field: "field",
                reason: ValidationError::Empty,
            };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1, "Abort must NOT retry");
            assert!(matches!(
                result,
                Err(E::InputValidationFailed { field: "field", .. })
            ));
        }

        #[test]
        fn skip_strategy_runs_exactly_once() {
            let r = RecoveryManager::with_defaults();
            let err = E::IterationBudgetExhausted { used: 1, limit: 0 };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1, "Skip must NOT retry");
            assert!(matches!(
                result,
                Err(E::IterationBudgetExhausted { used: 1, limit: 0 })
            ));
        }

        #[test]
        fn fallback_strategy_runs_exactly_once() {
            let r = RecoveryManager::with_defaults();
            let err = E::StorageReadFailed {
                address: 0,
                reason: crate::error::StorageError::CorruptedData,
            };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1, "Fallback must NOT retry");
            assert!(matches!(
                result,
                Err(E::StorageReadFailed {
                    reason: crate::error::StorageError::CorruptedData,
                    ..
                })
            ));
        }

        #[test]
        fn reset_strategy_runs_exactly_once() {
            let r = RecoveryManager::with_defaults();
            let err = E::StorageWriteFailed {
                address: 0,
                reason: crate::error::StorageError::WriteProtected,
            };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1, "Reset must NOT retry");
            assert!(matches!(
                result,
                Err(E::StorageWriteFailed {
                    reason: crate::error::StorageError::WriteProtected,
                    ..
                })
            ));
        }

        #[test]
        fn memory_allocation_failed_is_abort() {
            let r = RecoveryManager::with_defaults();
            let err = E::MemoryAllocationFailed {
                requested: 1024,
                available: 0,
            };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1);
            assert!(matches!(result, Err(E::MemoryAllocationFailed { .. })));
        }

        #[test]
        fn security_timeout_is_abort_not_retry() {
            // TRACE: AEROSPACE-AUDIT-2026-09-15 — `OperationTimeout { operation:
            // "security", .. }` is documented as Abort because a security
            // timeout is treated as a fatal signal. A future refactor that
            // maps all timeouts to RetryWithBackoff would let an attacker
            // pin the security subsystem indefinitely. This test pins the
            // carve-out.
            let r = RecoveryManager::with_defaults();
            let err = E::OperationTimeout {
                operation: "security",
                timeout_ms: 1,
            };
            let (result, attempts) = futures::executor::block_on(drive_once(&r, err));
            assert_eq!(attempts, 1, "security timeout must Abort, not Retry");
            assert!(matches!(result, Err(E::OperationTimeout { .. })));
        }
    }
}

/// Fallback value provider
pub trait FallbackProvider<T> {
    /// Get fallback value
    fn get_fallback(&self) -> T;
}

/// Default fallback provider for common types
pub struct DefaultFallback;

impl FallbackProvider<f32> for DefaultFallback {
    fn get_fallback(&self) -> f32 {
        0.0
    }
}

impl FallbackProvider<u32> for DefaultFallback {
    fn get_fallback(&self) -> u32 {
        0
    }
}

impl FallbackProvider<bool> for DefaultFallback {
    fn get_fallback(&self) -> bool {
        false
    }
}
