//! Wallet error types

use alloc::string::String;

/// Wallet-specific errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletError {
    /// Invalid mnemonic phrase
    InvalidMnemonic(String),
    /// Invalid word in mnemonic
    InvalidWord(String),
    /// Checksum verification failed
    InvalidChecksum,
    /// Entropy length mismatch
    InvalidEntropyLength(usize),
    /// Invalid derivation path
    InvalidDerivationPath(String),
    /// Key derivation failed
    DerivationFailed(String),
    /// Keystore error
    KeystoreError(String),
    /// Encryption failed
    EncryptionFailed(String),
    /// Decryption failed
    DecryptionFailed(String),
    /// Invalid passphrase
    InvalidPassphrase,
    /// Invalid keystore format
    InvalidKeystoreFormat,
    /// Version mismatch
    VersionMismatch(u32),
    /// Crypto operation failed
    CryptoError(String),
}

/// Result type alias for wallet operations
pub type WalletResult<T> = Result<T, WalletError>;

impl core::fmt::Display for WalletError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WalletError::InvalidMnemonic(s) => write!(f, "invalid mnemonic: {}", s),
            WalletError::InvalidWord(s) => write!(f, "invalid word: {}", s),
            WalletError::InvalidChecksum => write!(f, "mnemonic checksum verification failed"),
            WalletError::InvalidEntropyLength(n) => write!(f, "invalid entropy length: {} bits", n * 8),
            WalletError::InvalidDerivationPath(p) => write!(f, "invalid derivation path: {}", p),
            WalletError::DerivationFailed(s) => write!(f, "key derivation failed: {}", s),
            WalletError::KeystoreError(s) => write!(f, "keystore error: {}", s),
            WalletError::EncryptionFailed(s) => write!(f, "encryption failed: {}", s),
            WalletError::DecryptionFailed(s) => write!(f, "decryption failed: {}", s),
            WalletError::InvalidPassphrase => write!(f, "invalid passphrase for keystore"),
            WalletError::InvalidKeystoreFormat => write!(f, "invalid keystore format"),
            WalletError::VersionMismatch(v) => write!(f, "unsupported keystore version: {}", v),
            WalletError::CryptoError(s) => write!(f, "cryptographic error: {}", s),
        }
    }
}

impl From<crate::error::Web3ErrorKind> for WalletError {
    fn from(e: crate::error::Web3ErrorKind) -> Self {
        WalletError::CryptoError(format!("{:?}", e))
    }
}

#[cfg(test)]
mod tests {
    //! Audit-2026-08 H6: the wallet index's "absent" vs "unparseable"
    //! distinction needs a `WalletError` variant that operators can grep
    //! for. The ESP32 NVS layer translates its `WalletStorageError::
    //! CorruptedIndex` into this variant on the cross-crate `From` impl,
    //! and we want a guarantee that the substring `"corrupted"` is
    //! present in the rendered message so log alerts can match on it.

    use super::*;

    #[test]
    fn keystore_error_carries_arbitrary_substring_verbatim() {
        let e = WalletError::KeystoreError("wallet index corrupted: bad byte 17".into());
        let s = format!("{e}");
        assert!(
            s.contains("corrupted"),
            "expected 'corrupted' in {s}"
        );
        assert!(
            s.contains("bad byte 17"),
            "inner message must be preserved verbatim in {s}"
        );
    }

    #[test]
    fn crypto_error_does_not_shadow_corrupted_keyword_path() {
        // The "corrupted" substring is reserved for storage-layer
        // diagnostics. A bare `Web3ErrorKind` going through
        // `From<Web3ErrorKind>` should not accidentally pick it up.
        let wek = crate::error::Web3ErrorKind::Base58Decode("unrelated failure".into());
        let e: WalletError = wek.into();
        let rendered = format!("{e}");
        assert!(matches!(e, WalletError::CryptoError(_)));
        assert!(
            !rendered.contains("corrupted"),
            "a non-storage Web3ErrorKind must not produce the 'corrupted' substring, got {rendered}"
        );
    }
}
