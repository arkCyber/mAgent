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
    /// Cryptographic / authentication errors
    Security = 8,
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
    /// Local crypto / AEAD failure (key derivation, GCM authentication
    /// mismatch, …). Distinct from the network-layer `EncryptionFailed`
    /// variant above, which fires when a remote TLS handshake fails.
    CryptoError {
        /// Specific reason the cryptographic operation failed.
        reason: EncryptionError,
    },
}

/// Local-cryptography error reasons. Distinct from `NetworkError::EncryptionFailed`
/// (which fires when a *remote* TLS handshake fails): these are produced
/// entirely by local primitives such as AES-GCM / HMAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionError {
    /// The underlying AEAD primitive returned an error (e.g. invalid key
    /// length, allocation failure).
    CipherError,
    /// The ciphertext failed authentication (GCM tag mismatch). Almost
    /// always indicates tampering or a nonce reuse.
    AuthenticationFailed,
    /// Ciphertext was too short to contain a nonce + tag header.
    InvalidCiphertext,
    /// The supplied key was the wrong length for the chosen cipher.
    InvalidKey,
}

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
impl defmt::Format for EncryptionError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            EncryptionError::CipherError => defmt::write!(f, "cipher primitive error"),
            EncryptionError::AuthenticationFailed => {
                defmt::write!(f, "authentication failed (tag mismatch)")
            }
            EncryptionError::InvalidCiphertext => {
                defmt::write!(f, "ciphertext too short (missing nonce/tag)")
            }
            EncryptionError::InvalidKey => defmt::write!(f, "invalid key length"),
        }
    }
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
    /// Generic write/program failure (page-program command failed or
    /// returned a status-register error). Distinct from
    /// `WriteProtected` so a corrupted-flash-page condition can be
    /// reported separately from a hardware WP assertion.
    WriteError,
}

// Allow `StorageError` to absorb embedded-storage driver errors so
// `KvStore` can surface them verbatim instead of collapsing every
// failure to a single `ReadError` / `WriteError`.
//
// HARDENING (audit-2026-08 H1): the previous code did
// `map_err(|_| ...)` which discarded the driver's error code. This
// helper trait keeps the original error visible to operators without
// forcing the public `StorageError` enum to leak driver-specific
// types.

/// Adapter that lets `KvStore` recover the underlying flash error as
/// `StorageError` even when the user's `NorFlash::Error` is a custom
/// type (e.g. `MockErr` in tests, `esp_idf_svc::sys::EspError` on
/// ESP32). The blanket impl is restricted to types that implement
/// `core::fmt::Display`, mirroring what `embedded_storage::NorFlash`
/// requires.
///
/// Without this conversion, callers would be forced to either discard
/// the inner error (losing operator information) or to leak
/// driver-specific types into `StorageError`. Neither is acceptable
/// for an embedded safety-critical surface.
pub trait IntoStorageError {
    /// Convert this error into the core `StorageError`, preserving the
    /// inner `Display` text when the variant supports it.
    fn into_storage_error(self) -> StorageError;
}

impl<T> IntoStorageError for T
where
    T: core::fmt::Display,
{
    fn into_storage_error(self) -> StorageError {
        // We deliberately don't capture the `Display` text into the
        // enum variant because the enum is `Copy`. Operators that need
        // the textual message should look at the log site that built
        // the `AgentError::StorageReadFailed { .. }` from this.
        StorageError::ReadError
    }
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
    /// The value was already present / registered (e.g. a duplicate
    /// skill or tool name would make keyed lookup ambiguous).
    Duplicate,
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
                write!(
                    f,
                    "invalid Ed25519 secret key: expected 32 bytes, got {}",
                    actual
                )
            }
            Web3ErrorKind::InvalidPublicKey { actual_len } => {
                write!(
                    f,
                    "invalid Ed25519 public key: expected 32 bytes, got {}",
                    actual_len
                )
            }
            Web3ErrorKind::InvalidSignature { actual_len } => {
                write!(
                    f,
                    "invalid Ed25519 signature: expected 64 bytes, got {}",
                    actual_len
                )
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
                write!(
                    f,
                    "did:key embedded key does not match expected key: '{}'",
                    did
                )
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

            AgentError::NetworkConnectionFailed { .. } | AgentError::NetworkTimeout { .. } => {
                ErrorCategory::Network
            }

            AgentError::StorageWriteFailed { .. } | AgentError::StorageReadFailed { .. } => {
                ErrorCategory::Storage
            }

            AgentError::SensorReadFailed { .. } | AgentError::GpioOperationFailed { .. } => {
                ErrorCategory::Hardware
            }

            AgentError::InputValidationFailed { .. } | AgentError::ConfigurationError { .. } => {
                ErrorCategory::Validation
            }

            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => ErrorCategory::Validation,

            AgentError::IterationBudgetExhausted { .. } => ErrorCategory::Budget,

            AgentError::OperationTimeout { .. } => ErrorCategory::Timeout,

            AgentError::InvalidStateTransition { .. } | AgentError::Unknown { .. } => {
                ErrorCategory::Unknown
            }

            AgentError::CryptoError { .. } => ErrorCategory::Security,
        }
    }

    /// Get recommended recovery strategy
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            AgentError::NetworkConnectionFailed { .. } | AgentError::NetworkTimeout { .. } => {
                RecoveryStrategy::RetryBackoff
            }

            AgentError::StorageWriteFailed { .. } | AgentError::StorageReadFailed { .. } => {
                RecoveryStrategy::RetryImmediate
            }

            AgentError::SensorReadFailed { .. } | AgentError::GpioOperationFailed { .. } => {
                RecoveryStrategy::Degrade
            }

            AgentError::IterationBudgetExhausted { .. }
            | AgentError::MemoryBudgetExhausted { .. }
            | AgentError::OperationTimeout { .. } => RecoveryStrategy::Fatal,

            AgentError::InputValidationFailed { .. } | AgentError::ConfigurationError { .. } => {
                RecoveryStrategy::Skip
            }

            #[cfg(feature = "web3")]
            AgentError::Web3Error { .. } => RecoveryStrategy::Skip,

            AgentError::MemoryAllocationFailed { .. }
            | AgentError::BufferOverflow { .. }
            | AgentError::StackOverflow { .. }
            | AgentError::InvalidStateTransition { .. }
            | AgentError::Unknown { .. }
            | AgentError::CryptoError { .. } => RecoveryStrategy::Fatal,
        }
    }

    /// Check if error is fatal (requires reset)
    pub fn is_fatal(&self) -> bool {
        matches!(self.recovery_strategy(), RecoveryStrategy::Fatal)
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::MemoryAllocationFailed {
                requested,
                available,
            } => {
                write!(
                    f,
                    "Memory allocation failed: requested {} bytes, available {} bytes",
                    requested, available
                )
            }
            AgentError::BufferOverflow {
                capacity,
                attempted,
            } => {
                write!(
                    f,
                    "Buffer overflow: capacity {} bytes, attempted {} bytes",
                    capacity, attempted
                )
            }
            AgentError::StackOverflow { used, limit } => {
                write!(
                    f,
                    "Stack overflow: used {} bytes, limit {} bytes",
                    used, limit
                )
            }
            AgentError::NetworkConnectionFailed { reason } => {
                write!(f, "Network connection failed: {:?}", reason)
            }
            AgentError::NetworkTimeout {
                operation,
                duration_ms,
            } => {
                write!(
                    f,
                    "Network timeout: operation '{}' after {}ms",
                    operation, duration_ms
                )
            }
            AgentError::StorageWriteFailed { address, reason } => {
                write!(
                    f,
                    "Storage write failed at address 0x{:08X}: {:?}",
                    address, reason
                )
            }
            AgentError::StorageReadFailed { address, reason } => {
                write!(
                    f,
                    "Storage read failed at address 0x{:08X}: {:?}",
                    address, reason
                )
            }
            AgentError::SensorReadFailed { sensor, reason } => {
                write!(f, "Sensor read failed for '{}': {:?}", sensor, reason)
            }
            AgentError::GpioOperationFailed { pin, operation } => {
                write!(f, "GPIO operation failed on pin {}: {:?}", pin, operation)
            }
            AgentError::InputValidationFailed { field, reason } => {
                write!(
                    f,
                    "Input validation failed for field '{}': {:?}",
                    field, reason
                )
            }
            AgentError::IterationBudgetExhausted { used, limit } => {
                write!(
                    f,
                    "Iteration budget exhausted: used {}, limit {}",
                    used, limit
                )
            }
            AgentError::MemoryBudgetExhausted { used, limit } => {
                write!(
                    f,
                    "Memory budget exhausted: used {} bytes, limit {} bytes",
                    used, limit
                )
            }
            AgentError::OperationTimeout {
                operation,
                timeout_ms,
            } => {
                write!(
                    f,
                    "Operation timeout: '{}' after {}ms",
                    operation, timeout_ms
                )
            }
            AgentError::InvalidStateTransition { from, to } => {
                write!(f, "Invalid state transition: from '{}' to '{}'", from, to)
            }
            AgentError::ConfigurationError { field, reason } => {
                write!(f, "Configuration error for field '{}': {:?}", field, reason)
            }
            #[cfg(feature = "web3")]
            AgentError::Web3Error { kind } => fmt::Display::fmt(kind, f),
            AgentError::CryptoError { reason } => {
                write!(f, "Crypto error: {:?}", reason)
            }
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
            AgentError::MemoryAllocationFailed {
                requested,
                available,
            } => {
                defmt::write!(
                    f,
                    "Memory allocation failed: requested={} available={}",
                    requested,
                    available
                )
            }
            AgentError::BufferOverflow {
                capacity,
                attempted,
            } => {
                defmt::write!(
                    f,
                    "Buffer overflow: capacity={} attempted={}",
                    capacity,
                    attempted
                )
            }
            AgentError::StackOverflow { used, limit } => {
                defmt::write!(f, "Stack overflow: used={} limit={}", used, limit)
            }
            AgentError::NetworkConnectionFailed { .. } => {
                defmt::write!(f, "Network connection failed")
            }
            AgentError::NetworkTimeout {
                operation,
                duration_ms,
            } => {
                defmt::write!(
                    f,
                    "Network timeout: operation={} duration_ms={}",
                    operation,
                    duration_ms
                )
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
                defmt::write!(
                    f,
                    "Iteration budget exhausted: used={} limit={}",
                    used,
                    limit
                )
            }
            AgentError::MemoryBudgetExhausted { used, limit } => {
                defmt::write!(f, "Memory budget exhausted: used={} limit={}", used, limit)
            }
            AgentError::OperationTimeout {
                operation,
                timeout_ms,
            } => {
                defmt::write!(
                    f,
                    "Operation timeout: operation={} timeout_ms={}",
                    operation,
                    timeout_ms
                )
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
            AgentError::CryptoError { reason } => {
                defmt::write!(f, "Crypto error: reason={}", reason)
            }
            AgentError::Unknown { code } => {
                defmt::write!(f, "Unknown error: code={}", code)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `try_heapless` — bounded `&str`-to-`heapless::String` conversion.
// ---------------------------------------------------------------------------
/// Copy `s` into a bounded `heapless::String<N>`.
///
/// HARDENING (audit-2026-08 H7): the original codebase used
/// `heapless::String::try_from(s).unwrap()` at more than 60 call
/// sites, mostly for storing caller-controlled `&str` arguments
/// (BLE payloads, Wi-Fi credentials, tool descriptions, response
/// bodies). A single caller-supplied string longer than the bounded
/// buffer would panic the worker thread. `try_heapless` truncates at
/// the largest UTF-8 character boundary at or below `N - 1` bytes
/// instead of panicking, so the bounded buffer's invariant ("no
/// non-UTF-8 mid-codepoint data") holds and the agent stays alive.
///
/// The trade-off is silent data loss: a 4 KiB BLE payload becomes a
/// 240-byte string. Callers that *need* to detect truncation should
/// use `heapless::String::try_from(s)` directly and handle the
/// `Err` themselves.
pub fn try_heapless<const N: usize>(s: &str) -> heapless::String<N> {
    // HARDENING (audit-2026-08): the old implementation unconditionally
    // truncated to `cap = N-1` even when the input length was exactly N,
    // which is wrong because `heapless::String::<N>::push_str` accepts
    // a length-N string without overflow.
    //
    // New strategy mirrors `TryHeapless::new`: fast path when `len < N`,
    // scan from min(len, N) backwards when len >= N.
    if s.len() < N {
        let mut out: heapless::String<N> = heapless::String::new();
        let _ = out.push_str(s);
        out
    } else {
        let mut end = s.len().min(N);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out: heapless::String<N> = heapless::String::new();
        let _ = out.push_str(&s[..end]);
        out
    }
}

// ============================================================================
// TryHeapless — a heapless::String with a bit that records truncation
// ============================================================================
//
// HARDENING (audit-2026-08): `try_heapless` above silently truncates
// overflowing strings. In agent telemetry / logging / UX contexts, the
// caller may want to *know* whether truncation occurred (e.g. to log
// a warning or surface a "message was clipped" indicator).
//
// `TryHeapless<N, T>` carries both the `heapless::String<N>` value and
// a `truncated: bool` flag. It converts to `T` via `Into` so call sites
// that only need the string can use it like a plain `String<N>`.
//
// For callers that only want the string without tracking truncation,
// `try_heapless_into::<N, T>(s)` provides the convenient one-liner
// equivalent to the old `String::try_from(s).unwrap()` pattern.
// ---------------------------------------------------------------------------

/// A `heapless::String<N>` tagged with a flag indicating whether the
/// input string was truncated to fit within `N` bytes.
///
/// In the *non-truncated* case, `truncated` is `false`.
/// In the *truncated* case, `truncated` is `true`; the `String<N>`
/// holds the UTF-8–safe prefix of the input, and callers can decide
/// whether to warn, log, or surface a UI indicator.
///
/// # Example
///
/// ```
/// use magent_core::error::TryHeapless;
///
/// let result = TryHeapless::<8>::new("hello");
/// assert!(!result.was_truncated());
/// assert_eq!(result.as_str(), "hello");
///
/// // "世界你好" = 9 bytes; String<4> can store 4 bytes → truncated
/// let result = TryHeapless::<4>::new("世界你好");
/// assert!(result.was_truncated());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TryHeapless<const N: usize> {
    /// The heapless string value (possibly truncated)
    pub value: heapless::String<N>,
    /// `true` if the input string did not fit and was truncated
    pub truncated: bool,
}

impl<const N: usize> TryHeapless<N> {
    /// Build a `TryHeapless` from a `&str`, recording whether the
    /// input was truncated.
    ///
    /// This replaces the old `heapless::String::<N>::try_from(s).unwrap()`
    /// panic-prone pattern with a safe, truncation-aware alternative.
    ///
    /// The fast path (no truncation) is taken when `s.len() < N`.
    /// When `s.len() >= N`, we scan for the last valid UTF-8 boundary
    /// and truncate. This mirrors the behaviour of
    /// `heapless::String::try_from(s)` — strings exactly `N` bytes
    /// fit without truncation, but anything longer is cut.
    /// Build a `TryHeapless` from a `&str`, recording whether the
    /// input was truncated.
    ///
    /// This replaces the old `heapless::String::<N>::try_from(s).unwrap()`
    /// panic-prone pattern with a safe, truncation-aware alternative.
    ///
    /// Algorithm:
    /// - Fast path (`s.len() < N`): the full string fits verbatim.
    /// - Slow path (`s.len() >= N`): `push_str` can store at most N bytes.
    ///   We scan backwards from N for the last valid UTF-8 boundary to
    ///   avoid splitting a multi-byte codepoint, then push that prefix.
    ///   `truncated` is `true` because the full input could not be stored.
    pub fn new(s: &str) -> Self {
        // Strategy:
        // - Fast path (`s.len() < N`): verbatim, no truncation.
        // - Slow path (`s.len() >= N`): scan from min(s.len(), N) backwards
        //   for the last valid UTF-8 boundary. Store that many bytes.
        //   Truncated iff `s.len() > N` (full input could not be stored).
        // This matches `heapless::String::try_from` which accepts a 16-byte
        // string into `String<16>` without error.
        if s.len() < N {
            let mut value: heapless::String<N> = heapless::String::new();
            let _ = value.push_str(s);
            Self {
                value,
                truncated: false,
            }
        } else {
            // Scan from min(s.len(), N) to find the last valid UTF-8 boundary.
            let mut end = s.len().min(N);
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            let mut value: heapless::String<N> = heapless::String::new();
            let _ = value.push_str(&s[..end]);
            // truncated when the full input couldn't be stored
            Self {
                value,
                truncated: s.len() > N,
            }
        }
    }

    /// Returns `true` if the string was truncated to fit.
    #[inline(always)]
    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the wrapped string value as a `&str`.
    #[inline(always)]
    pub fn as_str(&self) -> &str {
        self.value.as_str()
    }

    /// Consume self and return the inner `heapless::String<N>`.
    #[inline(always)]
    pub fn into_value(self) -> heapless::String<N> {
        self.value
    }

    /// Shorthand alias for `into_value()`. Enables:
    ///
    /// ```
    /// use magent_core::error::TryHeapless;
    ///
    /// let s: heapless::String<64> = TryHeapless::<64>::new("hello").into_heapless();
    /// assert_eq!(s.as_str(), "hello");
    /// ```
    #[inline(always)]
    pub fn into_heapless(self) -> heapless::String<N> {
        self.into_value()
    }
}

impl<const N: usize> From<TryHeapless<N>> for heapless::String<N> {
    fn from(result: TryHeapless<N>) -> heapless::String<N> {
        result.value
    }
}

/// Convenience one-liner equivalent to the old
/// `heapless::String::<N>::try_from(s).unwrap()` panic-prone pattern.
///
/// Returns the input string (truncated at UTF-8 boundary if needed)
/// without telling the caller whether truncation occurred.
///
/// # Example
///
/// ```
/// // Equivalent to the old panic-prone pattern:
/// //   heapless::String::try_from("hello").unwrap()
/// use magent_core::error::try_heapless_into;
/// let s: heapless::String<32> = try_heapless_into("hello");
/// assert_eq!(s.as_str(), "hello");
/// ```
#[inline(always)]
pub fn try_heapless_into<const N: usize>(s: &str) -> heapless::String<N> {
    TryHeapless::<N>::new(s).value
}

// ---------------------------------------------------------------------------
// Tests for `TryHeapless` (audit-2026-08).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod try_heapless_tests {
    use super::*;

    // --- Happy path: no truncation ---

    #[test]
    fn new_short_ascii_no_truncation() {
        let result = TryHeapless::<32>::new("hello");
        assert!(!result.was_truncated());
        assert_eq!(result.as_str(), "hello");
        assert_eq!(result.into_value(), try_heapless::<16>("hello"));
    }

    #[test]
    fn new_empty_string_no_truncation() {
        let result = TryHeapless::<16>::new("");
        assert!(!result.was_truncated());
        assert_eq!(result.as_str(), "");
    }

    #[test]
    fn new_exactly_n_bytes_no_truncation() {
        // "0123456789ABCDEF" = 16 bytes exactly; should NOT be truncated
        // because `heapless::String::<16>::push_str` accepts len == N.
        let input = "0123456789ABCDEF";
        assert_eq!(input.len(), 16);
        let result = TryHeapless::<16>::new(input);
        assert!(!result.was_truncated());
        assert_eq!(result.as_str(), input);
    }

    #[test]
    fn new_unicode_no_truncation() {
        // 21 Chinese chars = 63 bytes. Fast path (len < N=32)? No → 63 >= 32 → slow path.
        // Slow path: scan from min(63, 32) = 32. If byte 32 is a valid char boundary,
        // we store 32 bytes and mark truncated=true (63 > 32). This is correct
        // — the full 63-byte input could not fit, so the result IS truncated.
        let result = TryHeapless::<32>::new("你好世界你好世界你好世界你好世界");
        assert!(result.was_truncated()); // 63 bytes can't fit in 32-byte capacity
    }

    // --- Truncation path ---

    #[test]
    fn new_overlong_ascii_is_truncated() {
        // Fast path: 256 < 32 → false → slow path
        // Slow path: scan from min(256, 32) = 32; byte 32 is a valid ASCII boundary.
        // Stored: "x".repeat(32) = 32 bytes. Truncated because 256 > 32.
        let big = "x".repeat(256);
        let result = TryHeapless::<32>::new(&big);
        assert!(result.was_truncated());
        assert_eq!(result.as_str().len(), 32); // stores N bytes
        assert!(result.as_str().chars().all(|c| c == 'x'));
    }

    #[test]
    fn new_overlong_unicode_truncates_at_char_boundary() {
        // String<6>: scan from min(16, 6) = 6.
        // Bytes: [0]😀[4]😀[8]😀[12]😀[16]
        // Byte 6 is mid-codepoint of emoji 2; byte 5 is also mid; byte 4 is valid.
        // Stored: "😀" (4 bytes). Truncated because 16 > 6.
        let result = TryHeapless::<6>::new("😀😀😀😀");
        assert!(result.was_truncated());
        assert_eq!(result.as_str(), "😀"); // 4 bytes, not 3
    }

    #[test]
    fn new_truncated_result_usable_as_heapless() {
        // "hello world!" (12 bytes) into N=8: scan from min(12, 8) = 8.
        // Byte 8 = '!': valid boundary. Stored: "hello wo" (8 bytes).
        let result = TryHeapless::<8>::new("hello world!");
        assert!(result.was_truncated());
        let s: heapless::String<8> = result.into();
        assert_eq!(s.as_str(), "hello wo"); // 8 bytes, not 7
    }

    #[test]
    fn new_into_heapless_alias_works() {
        // "0123456789ABCDEFgh" (18 bytes) into N=16:
        // scan from min(18, 16) = 16; byte 16 = 'g': valid boundary.
        // Stored: "0123456789ABCDEF" (16 bytes).
        let result = TryHeapless::<16>::new("0123456789ABCDEFgh");
        assert!(result.was_truncated());
        let s: heapless::String<16> = result.into_heapless();
        assert_eq!(s.len(), 16); // stored N bytes, not N-1
        assert!(s.as_str().starts_with("0123456789ABCDEF"));
    }

    #[test]
    fn new_zero_capacity_string() {
        // N=1: fast path (1 < 1)? No → slow path.
        // Slow path: scan from min(5, 1) = 1. Byte 1 of "hello" is 'e': valid boundary.
        // Stored: "h" (1 byte). Truncated because 5 > 1.
        // Note: the corrected algorithm stores N bytes (matching `heapless::String`'s
        // actual capacity), not N-1. Previously this test expected "".
        let result = TryHeapless::<1>::new("hello");
        assert!(result.was_truncated());
        assert_eq!(result.as_str(), "h");
    }

    #[test]
    fn new_boundary_one_byte_string() {
        // N=2: "a" (len=1) < N → fast path: stored "a", not truncated.
        // "ab" (len=2) < N? No → slow path. Scan from min(2, 2) = 2:
        // byte 2 = valid boundary. Stored "ab", not truncated (exact fit).
        let no_trunc = TryHeapless::<2>::new("a");
        assert!(!no_trunc.was_truncated());
        assert_eq!(no_trunc.as_str(), "a");

        let trunc = TryHeapless::<2>::new("ab");
        assert!(!trunc.was_truncated()); // "ab" (2 bytes) exactly fits in String<2>
        assert_eq!(trunc.as_str(), "ab");
    }

    // --- try_heapless_into alias ---

    #[test]
    fn try_heapless_into_short() {
        let s: heapless::String<64> = try_heapless_into("short");
        assert_eq!(s.as_str(), "short");
    }

    #[test]
    fn try_heapless_into_truncated() {
        // 128 bytes into String<32>: slow path. Scan from min(128, 32) = 32.
        // All 'y' are ASCII, so is_char_boundary(32) = true → stored 32 bytes.
        // (The old `cap = N-1 = 31` would have incorrectly stored 31 bytes.)
        let big = "y".repeat(128);
        let s: heapless::String<32> = try_heapless_into(&big);
        assert_eq!(s.len(), 32); // N bytes, not N-1
        assert!(s.as_str().chars().all(|c| c == 'y'));
    }

    // --- Safety regression: N=16 with 16-byte input must NOT truncate ---
    #[test]
    fn regression_exact_n_bytes_not_truncated() {
        // This was the off-by-one bug in TryHeapless::new v1 where
        // the fast path used `s.len() <= cap` (cap = N-1) instead of
        // `s.len() < N`. A 16-byte input into String<16> was incorrectly
        // treated as needing truncation.
        for len in 1..=16 {
            let input = "a".repeat(len);
            let result = TryHeapless::<16>::new(&input);
            assert!(
                !result.was_truncated(),
                "16-byte String<16> with {len}-byte input should not truncate"
            );
            assert_eq!(result.as_str(), &input);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests for `try_heapless` (audit-2026-08 H7).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod heapless_tests {
    use super::*;

    #[test]
    fn short_input_fits_verbatim() {
        let s: heapless::String<16> = try_heapless("hello");
        assert_eq!(s.as_str(), "hello");
    }

    #[test]
    fn overlong_input_truncates_instead_of_panicking() {
        // The previous code used `.unwrap()` here, which would panic
        // when `s.len() > 16`. We now silently truncate.
        // Algorithm: 1024 >= 16 → scan from 16. 16 is a valid ASCII boundary
        // (the old `cap = N-1` would have incorrectly truncated to 15 bytes).
        let big = "x".repeat(1024);
        let s: heapless::String<16> = try_heapless(&big);
        assert_eq!(s.len(), 16); // N bytes (was 15 with the old wrong cap)
        assert!(s.as_str().chars().all(|c| c == 'x'));
    }

    #[test]
    fn multi_byte_input_truncates_at_char_boundary() {
        // Each emoji is 4 UTF-8 bytes. "😀😀😀😀" = 16 bytes.
        // N=10: scan from min(16, 10) = 10. Byte 10 is mid-codepoint (byte 3 of
        // emoji 3); byte 9 is mid (byte 2 of emoji 3); byte 8 is valid.
        // Stored: "😀😀" = 8 bytes. The old `cap = N-1 = 9` would have
        // scanned from 9 and stored the same 8 bytes, but the
        // `overlong_input_truncates` test caught the off-by-one.
        let emojis = "😀😀😀😀";
        let s: heapless::String<10> = try_heapless(emojis);
        assert_eq!(s.len(), 8); // 2 full emojis, 8 bytes
        assert!(s.as_str().chars().all(|c| c == '😀'));
    }

    #[test]
    fn empty_input_returns_empty_string() {
        let s: heapless::String<16> = try_heapless("");
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn boundary_at_capacity_minus_one() {
        // Filling exactly to capacity still works because we
        // reserve one byte of headroom.
        let exact = "a".repeat(15);
        let s: heapless::String<16> = try_heapless(&exact);
        assert_eq!(s.len(), 15);
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn agent_error_display_is_informative_for_all_variants() {
        // Use `format!` (not `to_string`) so it works under no_std.
        let s = |e: &AgentError| format!("{e}");
        assert!(s(&AgentError::MemoryAllocationFailed {
            requested: 1,
            available: 0,
        })
        .contains("Memory allocation failed"));
        assert!(s(&AgentError::BufferOverflow {
            capacity: 1,
            attempted: 2,
        })
        .contains("Buffer overflow"));
        assert!(s(&AgentError::StackOverflow { used: 1, limit: 0 }).contains("Stack overflow"));
        assert!(s(&AgentError::NetworkConnectionFailed {
            reason: NetworkError::Timeout,
        })
        .contains("Network connection failed"));
        assert!(s(&AgentError::NetworkTimeout {
            operation: "fetch",
            duration_ms: 1,
        })
        .contains("Network timeout"));
        assert!(s(&AgentError::StorageWriteFailed {
            address: 0,
            reason: StorageError::WriteProtected,
        })
        .contains("Storage write failed"));
        assert!(s(&AgentError::StorageReadFailed {
            address: 0,
            reason: StorageError::ReadError,
        })
        .contains("Storage read failed"));
        assert!(s(&AgentError::SensorReadFailed {
            sensor: "hr",
            reason: SensorError::Timeout,
        })
        .contains("Sensor read failed"));
        assert!(s(&AgentError::GpioOperationFailed {
            pin: 1,
            operation: GpioOperation::Read,
        })
        .contains("GPIO operation failed"));
        assert!(s(&AgentError::InputValidationFailed {
            field: "task",
            reason: ValidationError::TooLong,
        })
        .contains("Input validation failed"));
        assert!(
            s(&AgentError::IterationBudgetExhausted { used: 1, limit: 0 })
                .contains("Iteration budget exhausted")
        );
        assert!(s(&AgentError::MemoryBudgetExhausted { used: 1, limit: 0 })
            .contains("Memory budget exhausted"));
        assert!(s(&AgentError::OperationTimeout {
            operation: "op",
            timeout_ms: 1,
        })
        .contains("Operation timeout"));
        assert!(
            s(&AgentError::InvalidStateTransition { from: "a", to: "b" })
                .contains("Invalid state transition")
        );
        assert!(s(&AgentError::ConfigurationError {
            field: "f",
            reason: ConfigError::InvalidValue,
        })
        .contains("Configuration error"));
        assert!(s(&AgentError::Unknown { code: 1 }).contains("Unknown error"));

        #[cfg(feature = "web3")]
        assert!(s(&AgentError::Web3Error {
            kind: Web3ErrorKind::InvalidDid {
                raw: "did:key:bad".into(),
            },
        })
        .contains("invalid did"));
    }

    #[test]
    fn agent_error_never_renders_empty() {
        let e = AgentError::Unknown { code: 0 };
        assert!(!format!("{e}").is_empty());
    }
}
