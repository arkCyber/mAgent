//! Aerospace-grade error handling for mAgent
//!
//! All errors are handled through Result types with no panics.
//! Error classification enables intelligent recovery strategies.

#![allow(dead_code)]

use core::fmt;

// `String` lives in `alloc` because several variants hold a raw
// identifier string (the offending DID, the decoded base58 error
// message, …). The crate's `lib.rs` brings `alloc` in via
// `#[macro_use] extern crate alloc;` but macros aren't types, so
// we re-import `String` here.
#[cfg(feature = "web3")]
use alloc::string::String;

/// Result type for mAgent operations
pub type Result<T> = core::result::Result<T, AgentError>;

/// Aerospace-grade error classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCategory {
    /// Memory-related errors (allocation, overflow)
    Memory = 0,
    /// Network/communication errors
    Network = 1,
    /// Storage/flash errors
    Storage = 2,
    /// Sensor/hardware errors
    Hardware = 3,
    /// Input validation errors
    Validation = 4,
    /// Budget exhaustion errors
    Budget = 5,
    /// Timeout errors
    Timeout = 6,
    /// Unknown/unclassified errors
    Unknown = 7,
}

/// Recovery strategy for error handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecoveryStrategy {
    /// Retry immediately
    RetryImmediate = 0,
    /// Retry with exponential backoff
    RetryBackoff = 1,
    /// Skip and continue
    Skip = 2,
    /// Graceful degradation
    Degrade = 3,
    /// Fatal error, requires reset
    Fatal = 4,
}

/// Comprehensive error type for mAgent
#[derive(Debug, Clone)]
pub enum AgentError {
    /// Memory allocation failed
    MemoryAllocationFailed {
        /// Number of bytes the caller tried to allocate.
        requested: usize,
        /// Number of bytes still free in the heap at the time of the request.
        available: usize,
    },
    /// Buffer overflow detected
    BufferOverflow {
        /// Total capacity of the buffer that overflowed.
        capacity: usize,
        /// Number of bytes the caller attempted to write past the end.
        attempted: usize,
    },
    /// Stack overflow detected
    StackOverflow {
        /// Stack bytes in use when the overflow was detected.
        used: usize,
        /// Configured stack depth limit.
        limit: usize,
    },
    /// Network connection failed
    NetworkConnectionFailed {
        /// Specific network-layer error (DNS, TCP reset, refused, …).
        reason: NetworkError,
    },
    /// Network timeout
    NetworkTimeout {
        /// Logical operation that was in flight when the timeout fired.
        operation: &'static str,
        /// How long the operation had been waiting, in milliseconds.
        duration_ms: u32,
    },
    /// Storage write failed
    StorageWriteFailed {
        /// Flash address (byte-offset) that the failed write targeted.
        address: u32,
        /// Specific storage-layer error reported by the driver.
        reason: StorageError,
    },
    /// Storage read failed
    StorageReadFailed {
        /// Flash address (byte-offset) that the failed read targeted.
        address: u32,
        /// Specific storage-layer error reported by the driver.
        reason: StorageError,
    },
    /// Sensor read failed
    SensorReadFailed {
        /// Sensor identifier (e.g. `"heart_rate"`, `"spo2"`) as it
        /// was registered with the sensor manager.
        sensor: &'static str,
        /// Specific sensor-layer error reported by the driver.
        reason: SensorError,
    },
    /// GPIO operation failed
    GpioOperationFailed {
        /// Pin number that the failed operation targeted.
        pin: u8,
        /// Which GPIO operation failed (read, write, configure).
        operation: GpioOperation,
    },
    /// Input validation failed
    InputValidationFailed {
        /// Name of the validated field (e.g. `"task"`, `"model"`).
        field: &'static str,
        /// Specific validation failure (too long, out of range, …).
        reason: ValidationError,
    },
    /// Iteration budget exhausted
    IterationBudgetExhausted {
        /// Number of ReAct loop iterations the agent consumed.
        used: usize,
        /// Configured iteration cap.
        limit: usize,
    },
    /// Memory budget exhausted
    MemoryBudgetExhausted {
        /// Heap bytes in use when the budget ran out.
        used: usize,
        /// Configured heap cap.
        limit: usize,
    },
    /// Operation timeout
    OperationTimeout {
        /// Logical operation that was in flight when the timeout fired.
        operation: &'static str,
        /// Configured timeout in milliseconds.
        timeout_ms: u32,
    },
    /// Invalid state transition
    InvalidStateTransition {
        /// State name the agent was leaving.
        from: &'static str,
        /// State name the agent tried to enter.
        to: &'static str,
    },
    /// Configuration error
    ConfigurationError {
        /// Name of the malformed config field.
        field: &'static str,
        /// Specific configuration failure (invalid value, missing
        /// field, type mismatch, …).
        reason: ConfigError,
    },
    /// Web3 / asymmetric-cryptography error.
    ///
    /// Covers everything `magent_core::web3` can report: invalid key
    /// encoding, signature verification failure, DID derivation
    /// mismatch, and so on. The nested [`Web3ErrorKind`] carries the
    /// specific cause so callers can branch on it without parsing the
    /// `Display` string.
    #[cfg(feature = "web3")]
    Web3Error {
        /// Specific Web3-layer error reported by the crypto module.
        kind: Web3ErrorKind,
    },
    /// Unknown error
    Unknown {
        /// Opaque numeric error code carried over from a foreign
        /// (FFI / C) source. Lossy by definition — only useful when
        /// matched against the foreign library's header.
        code: u32,
    },
}

/// Network-specific errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    /// Remote endpoint actively refused the connection (TCP RST or ECONNREFUSED).
    ConnectionRefused,
    /// Connection was established and then abruptly dropped by the peer.
    ConnectionReset,
    /// No route to the destination host (DNS or routing failure).
    HostUnreachable,
    /// The local network interface reports `down` — Wi-Fi off, cable unplugged, etc.
    NetworkDown,
    /// Operation timed out waiting for the remote to respond.
    Timeout,
    /// Peer replied with a malformed or schema-violating payload.
    InvalidResponse,
    /// Credentials were rejected by the remote.
    AuthenticationFailed,
    /// TLS / noise handshake failed or peer cert was untrusted.
    EncryptionFailed,
}

/// Storage-specific errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    /// The target sector is hardware write-protected (WP# pin asserted).
    WriteProtected,
    /// Existing bytes do not match the expected ECC / CRC signature.
    CorruptedData,
    /// No free sector remains to satisfy the write request.
    OutOfSpace,
    /// Address is outside the configured flash range.
    BadAddress,
    /// Driver-level read failure (SPI timeout, GPIO error, …).
    ReadError,
    /// Sector erase command failed or was vetoed by the flash controller.
    EraseError,
}

/// Sensor-specific errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorError {
    /// Sensor was never powered up / configured.
    NotInitialized,
    /// Sensor is on the bus but not responding to the probe.
    NotAvailable,
    /// Self-test or factory calibration sequence failed.
    CalibrationFailed,
    /// Driver timed out waiting for the data-ready interrupt.
    Timeout,
    /// Reading was outside the sensor's supported physical range.
    InvalidValue,
}

/// GPIO operation types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioOperation {
    /// Read the current level of a pin.
    Read,
    /// Drive a pin high or low.
    Write,
    /// Reconfigure direction, pull, drive mode, etc.
    Configure,
}

/// Validation errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    /// Input exceeded the maximum allowed length.
    TooLong,
    /// Input was below the minimum required length.
    TooShort,
    /// Format did not match the expected pattern (regex / charset).
    InvalidFormat,
    /// Numeric value was outside the accepted inclusive range.
    OutOfRange,
    /// String contained characters not on the allow-list.
    ContainsInvalidChars,
    /// Required input was absent.
    Empty,
}

/// Configuration errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Field was present but did not parse to the expected type/value.
    InvalidValue,
    /// Field was absent and no default applies.
    Empty,
    /// String-typed field exceeded its schema-declared length cap.
    TooLong,
    /// Required field was not present in the config source.
    MissingField,
    /// Numeric field exceeded or fell below its declared bounds.
    OutOfRange,
    /// Field type did not match the schema (e.g. expected bool, got int).
    TypeMismatch,
    /// Backend not configured for the target platform (e.g. RPC URL
    /// absent on a bare-metal firmware build). TRACE: REQ-NET-002.
    NotConfigured,
}

/// What kind of wire-format input failed to parse. Distinct
/// from [`Web3ErrorKind::HexDecode`] / `Base58Decode` because
/// those encode problems inside the *encoded payload* (e.g. a
/// signature with a bad hex digit), while `Parse` covers the
/// envelope / container format itself (the JSON shape, the
/// `SignedMessage` field set, …).
#[cfg(feature = "web3")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseFailureKind {
    /// The input was not valid JSON (serde_json failed to
    /// deserialise it).
    InvalidJson,
    /// The input parsed as JSON but did not have the
    /// `SignedMessage` schema (missing required field, wrong
    /// type, …).
    SchemaMismatch,
    /// The hex- or base58-decoded field was the wrong length
    /// for what it was supposed to represent (e.g. a
    /// `signature_hex` that decoded to 32 bytes instead of 64).
    WrongLength,
}

/// Web3 / asymmetric-cryptography specific errors.
///
/// Returned by the `magent_core::web3` module. Each variant carries
/// enough context for the caller to log a useful message without
/// having to unwrap a `Display` string.
#[cfg(feature = "web3")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Web3ErrorKind {
    /// A 32-byte Ed25519 secret key could not be derived from the
    /// supplied input (e.g. wrong length after decoding from hex).
    InvalidSecretKeyLength {
        /// Number of bytes the caller actually provided.
        actual: usize,
    },
    /// A 32-byte Ed25519 public key could not be parsed (wrong
    /// length, non-canonical encoding, …).
    InvalidPublicKey {
        /// Decoded byte length, when available, for diagnostics.
        actual_len: usize,
    },
    /// A 64-byte Ed25519 signature could not be parsed.
    InvalidSignature {
        /// Decoded byte length, when available, for diagnostics.
        actual_len: usize,
    },
    /// A `did:key` identifier was malformed (wrong prefix, bad
    /// multibase encoding, wrong multicodec tag for the claimed
    /// key type).
    InvalidDid {
        /// The raw DID string the caller supplied.
        raw: String,
    },
    /// Multibase / base58 decoding failed.
    ///
    /// Carries a short, human-readable reason rather than the raw
    /// `bs58` error so callers don't need to keep `bs58`'s error
    /// type in scope to log a useful message.
    Base58Decode(String),
    /// Hex decoding failed.
    ///
    /// Carries a short, human-readable reason (`"invalid hex
    /// digit: 'g'"`, `"odd number of hex digits"`, …). For
    /// envelope-shape errors (the input wasn't JSON at all,
    /// or the JSON didn't match the `SignedMessage` schema),
    /// use [`Web3ErrorKind::Parse`] instead — those are
    /// categorically different from a *bad digit inside an
    /// otherwise-valid hex field*.
    HexDecode(String),
    /// A wire-format envelope (currently `SignedMessage`) failed
    /// to parse. Distinct from `HexDecode` so callers can tell
    /// "the JSON shape is wrong" apart from "the signature hex
    /// has a bad digit".
    Parse {
        /// What went wrong.
        kind: ParseFailureKind,
        /// Underlying error message (from `serde_json`, …) for
        /// diagnostics.
        message: String,
    },
    /// Random-number generator returned an error.
    RngError(String),
    /// Signature verification failed (signature did not validate
    /// against the claimed public key + payload).
    SignatureVerificationFailed,
    /// A public key derived from a `did:key` does not match the
    /// public key the caller expected (e.g. a signature was made
    /// by a different identity than the DID claims).
    DidKeyMismatch {
        /// DID whose embedded public key did not match.
        did: String,
    },
    /// A blockchain RPC / on-chain operation failed (network
    /// error, contract revert, …). The wrapped string is meant
    /// for diagnostics only.
    BlockchainError(String),
}

#[cfg(feature = "web3")]
impl fmt::Display for Web3ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Web3ErrorKind::InvalidSecretKeyLength { actual } => {
                write!(f, "invalid Ed25519 secret key: expected 32 bytes, got {}", actual)
            }
            Web3ErrorKind::InvalidPublicKey { actual_len } => {
                write!(f, "invalid Ed25519 public key: expected 32 bytes, got {}", actual_len)
            }
            Web3ErrorKind::InvalidSignature { actual_len } => {
                write!(f, "invalid Ed25519 signature: expected 64 bytes, got {}", actual_len)
            }
            Web3ErrorKind::InvalidDid { raw } => {
                write!(f, "invalid did:key identifier: '{}'", raw)
            }
            Web3ErrorKind::Base58Decode(msg) => {
                write!(f, "base58 decode failed: {}", msg)
            }
            Web3ErrorKind::HexDecode(msg) => {
                write!(f, "hex decode failed: {}", msg)
            }
            Web3ErrorKind::Parse { kind, message } => match kind {
                ParseFailureKind::InvalidJson => {
                    write!(f, "invalid JSON envelope: {}", message)
                }
                ParseFailureKind::SchemaMismatch => {
                    write!(f, "envelope schema mismatch: {}", message)
                }
                ParseFailureKind::WrongLength => {
                    write!(f, "envelope field wrong length: {}", message)
                }
            },
            Web3ErrorKind::RngError(msg) => {
                write!(f, "RNG error: {}", msg)
            }
            Web3ErrorKind::SignatureVerificationFailed => {
                write!(f, "signature verification failed")
            }
            Web3ErrorKind::DidKeyMismatch { did } => {
                write!(f, "did:key embedded key does not match expected key: '{}'", did)
            }
            Web3ErrorKind::BlockchainError(msg) => {
                write!(f, "blockchain error: {}", msg)
            }
        }
    }
}

impl AgentError {
    /// Get error category for classification
    pub fn category(&self) -> ErrorCategory {
        match self {
            AgentError::MemoryAllocationFailed { .. }
            | AgentError::BufferOverflow { .. }
            | AgentError::StackOverflow { .. }
            | AgentError::MemoryBudgetExhausted { .. } => ErrorCategory::Memory,

            AgentError::NetworkConnectionFailed { .. }
            | AgentError::NetworkTimeout { .. } => ErrorCategory::Network,

            AgentError::StorageWriteFailed { .. }
            | AgentError::StorageReadFailed { .. } => ErrorCategory::Storage,

            AgentError::SensorReadFailed { .. }
            | AgentError::GpioOperationFailed { .. } => ErrorCategory::Hardware,

            AgentError::InputValidationFailed { .. }
            | AgentError::ConfigurationError { .. } => ErrorCategory::Validation,

            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => ErrorCategory::Validation,

            AgentError::IterationBudgetExhausted { .. } => ErrorCategory::Budget,

            AgentError::OperationTimeout { .. } => ErrorCategory::Timeout,

            AgentError::InvalidStateTransition { .. }
            | AgentError::Unknown { .. } => ErrorCategory::Unknown,
        }
    }

    /// Get recommended recovery strategy
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            AgentError::NetworkConnectionFailed { .. }
            | AgentError::NetworkTimeout { .. } => RecoveryStrategy::RetryBackoff,

            AgentError::StorageWriteFailed { .. }
            | AgentError::StorageReadFailed { .. } => RecoveryStrategy::RetryImmediate,

            AgentError::SensorReadFailed { .. }
            | AgentError::GpioOperationFailed { .. } => RecoveryStrategy::Degrade,

            AgentError::IterationBudgetExhausted { .. }
            | AgentError::MemoryBudgetExhausted { .. }
            | AgentError::OperationTimeout { .. } => RecoveryStrategy::Fatal,

            AgentError::InputValidationFailed { .. }
            | AgentError::ConfigurationError { .. } => RecoveryStrategy::Skip,

            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => RecoveryStrategy::Skip,

            AgentError::MemoryAllocationFailed { .. }
            | AgentError::BufferOverflow { .. }
            | AgentError::StackOverflow { .. }
            | AgentError::InvalidStateTransition { .. }
            | AgentError::Unknown { .. } => RecoveryStrategy::Fatal,
        }
    }

    /// Check if error is fatal (requires reset)
    pub fn is_fatal(&self) -> bool {
        matches!(
            self.recovery_strategy(),
            RecoveryStrategy::Fatal
        )
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::MemoryAllocationFailed { requested, available } => {
                write!(f, "Memory allocation failed: requested {} bytes, available {} bytes", requested, available)
            }
            AgentError::BufferOverflow { capacity, attempted } => {
                write!(f, "Buffer overflow: capacity {} bytes, attempted {} bytes", capacity, attempted)
            }
            AgentError::StackOverflow { used, limit } => {
                write!(f, "Stack overflow: used {} bytes, limit {} bytes", used, limit)
            }
            AgentError::NetworkConnectionFailed { reason } => {
                write!(f, "Network connection failed: {:?}", reason)
            }
            AgentError::NetworkTimeout { operation, duration_ms } => {
                write!(f, "Network timeout: operation '{}' after {}ms", operation, duration_ms)
            }
            AgentError::StorageWriteFailed { address, reason } => {
                write!(f, "Storage write failed at address 0x{:08X}: {:?}", address, reason)
            }
            AgentError::StorageReadFailed { address, reason } => {
                write!(f, "Storage read failed at address 0x{:08X}: {:?}", address, reason)
            }
            AgentError::SensorReadFailed { sensor, reason } => {
                write!(f, "Sensor read failed for '{}': {:?}", sensor, reason)
            }
            AgentError::GpioOperationFailed { pin, operation } => {
                write!(f, "GPIO operation failed on pin {}: {:?}", pin, operation)
            }
            AgentError::InputValidationFailed { field, reason } => {
                write!(f, "Input validation failed for field '{}': {:?}", field, reason)
            }
            AgentError::IterationBudgetExhausted { used, limit } => {
                write!(f, "Iteration budget exhausted: used {}, limit {}", used, limit)
            }
            AgentError::MemoryBudgetExhausted { used, limit } => {
                write!(f, "Memory budget exhausted: used {} bytes, limit {} bytes", used, limit)
            }
            AgentError::OperationTimeout { operation, timeout_ms } => {
                write!(f, "Operation timeout: '{}' after {}ms", operation, timeout_ms)
            }
            AgentError::InvalidStateTransition { from, to } => {
                write!(f, "Invalid state transition: from '{}' to '{}'", from, to)
            }
            AgentError::ConfigurationError { field, reason } => {
                write!(f, "Configuration error for field '{}': {:?}", field, reason)
            }
            #[cfg(feature = "web3")]
            AgentError::Web3Error { kind } => fmt::Display::fmt(kind, f),
            AgentError::Unknown { code } => {
                write!(f, "Unknown error with code: {}", code)
            }
        }
    }
}

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
impl defmt::Format for AgentError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            AgentError::MemoryAllocationFailed { requested, available } => {
                defmt::write!(f, "Memory allocation failed: requested={} available={}", requested, available)
            }
            AgentError::BufferOverflow { capacity, attempted } => {
                defmt::write!(f, "Buffer overflow: capacity={} attempted={}", capacity, attempted)
            }
            AgentError::StackOverflow { used, limit } => {
                defmt::write!(f, "Stack overflow: used={} limit={}", used, limit)
            }
            AgentError::NetworkConnectionFailed { .. } => {
                defmt::write!(f, "Network connection failed")
            }
            AgentError::NetworkTimeout { operation, duration_ms } => {
                defmt::write!(f, "Network timeout: operation={} duration_ms={}", operation, duration_ms)
            }
            AgentError::StorageWriteFailed { address, .. } => {
                defmt::write!(f, "Storage write failed: address={:#x}", address)
            }
            AgentError::StorageReadFailed { address, .. } => {
                defmt::write!(f, "Storage read failed: address={:#x}", address)
            }
            AgentError::SensorReadFailed { sensor, .. } => {
                defmt::write!(f, "Sensor read failed: sensor={}", sensor)
            }
            AgentError::GpioOperationFailed { pin, .. } => {
                defmt::write!(f, "GPIO operation failed: pin={}", pin)
            }
            AgentError::InputValidationFailed { field, .. } => {
                defmt::write!(f, "Input validation failed: field={}", field)
            }
            AgentError::IterationBudgetExhausted { used, limit } => {
                defmt::write!(f, "Iteration budget exhausted: used={} limit={}", used, limit)
            }
            AgentError::MemoryBudgetExhausted { used, limit } => {
                defmt::write!(f, "Memory budget exhausted: used={} limit={}", used, limit)
            }
            AgentError::OperationTimeout { operation, timeout_ms } => {
                defmt::write!(f, "Operation timeout: operation={} timeout_ms={}", operation, timeout_ms)
            }
            AgentError::InvalidStateTransition { from, to } => {
                defmt::write!(f, "Invalid state transition: from={} to={}", from, to)
            }
            AgentError::ConfigurationError { field, .. } => {
                defmt::write!(f, "Configuration error: field={}", field)
            }
            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => {
                defmt::write!(f, "Web3 error")
            }
            AgentError::Unknown { code } => {
                defmt::write!(f, "Unknown error: code={}", code)
            }
        }
    }
}

