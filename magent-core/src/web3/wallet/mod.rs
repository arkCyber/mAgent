//! Digital Wallet Module
//!
//! Provides BIP-39 mnemonic generation/validation, HD wallet key derivation,
//! and encrypted keystore storage for secp256k1-based Ethereum wallets.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Wallet Module                              │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │  BIP39 (mnemonic ↔ entropy ↔ seed)                 │   │
//! │  │  - 12/24 word phrases                             │   │
//! │  │  - Entropy: 128/256 bits                          │   │
//! │  │  - PBKDF2-HMAC-SHA512 (2048 iterations)           │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │  Keystore (JSON + AES-256-GCM + Argon2id)         │   │
//! │  │  - Ethereum keystore v3 format                     │   │
//! │  │  - Argon2id for passphrase → key derivation       │   │
//! │  │  - AES-256-GCM for encryption                     │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │  Wallet Manager                                    │   │
//! │  │  - Create/import wallets                          │   │
//! │  │  - BIP-32 HD key derivation                       │   │
//! │  │  - Address generation (m/44'/60'/0'/0/0)          │   │
//! │  │  - Transaction signing                           │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Feature Flags
//!
//! - `wallet`: Enables all wallet features (implies `web3`, `std`)
//! - `esp32_nvs`: Enables ESP32 NVS storage (requires `esp32`)

#![cfg(feature = "wallet")]

pub mod bip39;
/// Official BIP-39 English wordlist (2048 words). Internal data table;
/// parse/generate through [`bip39`] rather than indexing this directly.
mod english;
pub mod error;
pub mod keystore;
pub mod wallet;

/// ESP32 NVS storage module (only available with `esp32_nvs` feature)
#[cfg(feature = "esp32_nvs")]
pub mod esp32_nvs;

pub use bip39::{Mnemonic, MnemonicType, WordList};
pub use error::{WalletError, WalletResult};
pub use keystore::{Keystore, KeystoreError, KeystoreMetadata};
pub use wallet::{
    derive_address, derive_private_key, sign_transaction_hash, DerivationPath, Wallet,
    WalletManager,
};
