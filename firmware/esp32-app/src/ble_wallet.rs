//! Wallet BLE GATT Service
//!
//! Provides BLE GATT characteristics for wallet operations:
//! - Wallet creation
//! - Wallet import (mnemonic/private key)
//! - Balance checking
//! - Transaction signing
//!
//! ## Service UUID
//!
//! Wallet Service: `0x1851` (custom)
//!
//! ## Characteristics
//!
//! | UUID | Name | Properties | Description |
//! |------|------|-------------|-------------|
//! | 0x2A01 | WalletCmd | Write | Wallet commands (create/import/sign) |
//! | 0x2A02 | WalletData | Write/Read | Command parameters / response data |
//! | 0x2A03 | WalletStatus | Notify | Async status updates |

use heapless::{String as HeaplessString, Vec};

/// Wallet BLE Service UUID
pub const WALLET_SERVICE_UUID16: u16 = 0x1851;

/// Wallet Command Characteristic UUID
pub const WALLET_CMD_UUID16: u16 = 0x2A01;

/// Wallet Data Characteristic UUID
pub const WALLET_DATA_UUID16: u16 = 0x2A02;

/// Wallet Status Characteristic UUID
pub const WALLET_STATUS_UUID16: u16 = 0x2A03;

/// Maximum wallet name length
pub const MAX_WALLET_NAME_LEN: usize = 32;

/// Maximum data size for BLE transfers
pub const MAX_BLE_DATA_SIZE: usize = 512;

/// Wallet command types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletCommand {
    /// Create a new wallet (params: name, passphrase)
    Create = 0x01,
    /// Import wallet from mnemonic (params: name, phrase, passphrase)
    ImportMnemonic = 0x02,
    /// Import wallet from private key (params: name, private_key_hex)
    ImportPrivateKey = 0x03,
    /// Get wallet address
    GetAddress = 0x04,
    /// Sign transaction (params: transaction_hex)
    SignTransaction = 0x05,
    /// List all wallets
    ListWallets = 0x06,
    /// Delete wallet
    DeleteWallet = 0x07,
    /// Set default wallet
    SetDefault = 0x08,
    /// Get balance (requires RPC)
    GetBalance = 0x09,
    /// Export wallet info (name + address, no secrets)
    ExportInfo = 0x0A,
}

impl WalletCommand {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Create),
            0x02 => Some(Self::ImportMnemonic),
            0x03 => Some(Self::ImportPrivateKey),
            0x04 => Some(Self::GetAddress),
            0x05 => Some(Self::SignTransaction),
            0x06 => Some(Self::ListWallets),
            0x07 => Some(Self::DeleteWallet),
            0x08 => Some(Self::SetDefault),
            0x09 => Some(Self::GetBalance),
            0x0A => Some(Self::ExportInfo),
            _ => None,
        }
    }
}

/// Wallet status codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletStatus {
    /// Operation completed successfully
    Success = 0x00,
    /// Wallet not found
    NotFound = 0x01,
    /// Invalid parameters
    InvalidParams = 0x02,
    /// Wallet already exists
    AlreadyExists = 0x03,
    /// Storage error
    StorageError = 0x04,
    /// Crypto operation failed
    CryptoError = 0x05,
    /// Passphrase incorrect
    WrongPassphrase = 0x06,
    /// Network error (for balance check)
    NetworkError = 0x07,
    /// Wallet is locked
    WalletLocked = 0x08,
    /// Wallet is full (max wallets reached)
    WalletFull = 0x09,
}

/// Wallet BLE data format for command parameters
#[derive(Debug, Clone)]
pub struct WalletCmdParams {
    /// Wallet name
    pub name: HeaplessString<MAX_WALLET_NAME_LEN>,
    /// Command type
    pub command: WalletCommand,
    /// Parameter data (varies by command)
    pub data: Vec<u8, MAX_BLE_DATA_SIZE>,
}

impl WalletCmdParams {
    /// Parse command parameters from BLE data
    ///
    /// Format: [command:u8][name_len:u8][name:str][params:bytes...]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 3 {
            return None;
        }

        let command = WalletCommand::from_u8(data[0])?;
        let name_len = data[1] as usize;

        if name_len > MAX_WALLET_NAME_LEN || data.len() < 2 + name_len {
            return None;
        }

        let name_bytes = &data[2..2 + name_len];
        let name_str = core::str::from_utf8(name_bytes).ok()?;

        let mut name = HeaplessString::new();
        name.push_str(name_str).ok()?;

        let params = Vec::from_slice(&data[2 + name_len..]).ok()?;

        Some(Self {
            name,
            command,
            data: params,
        })
    }

    /// Serialize command parameters to bytes
    pub fn to_bytes(&self) -> Vec<u8, MAX_BLE_DATA_SIZE> {
        let mut result = Vec::new();
        result.push(self.command as u8).ok();
        result.push(self.name.len() as u8).ok();

        for &b in self.name.as_bytes() {
            result.push(b).ok();
        }

        for &b in &self.data {
            result.push(b).ok();
        }

        result
    }
}

/// Wallet response format
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletResponse {
    /// Status code
    pub status: u8,
    /// Response data (JSON encoded)
    pub data: Option<std::string::String>,
}

impl WalletResponse {
    pub fn success() -> Self {
        Self {
            status: WalletStatus::Success as u8,
            data: None,
        }
    }

    pub fn success_data(data: impl Into<std::string::String>) -> Self {
        Self {
            status: WalletStatus::Success as u8,
            data: Some(data.into()),
        }
    }

    pub fn error(status: WalletStatus) -> Self {
        Self {
            status: status as u8,
            data: None,
        }
    }

    pub fn error_data(status: WalletStatus, data: impl Into<std::string::String>) -> Self {
        Self {
            status: status as u8,
            data: Some(data.into()),
        }
    }

    /// Serialize to JSON bytes
    pub fn to_json_bytes(&self) -> Vec<u8, MAX_BLE_DATA_SIZE> {
        let json: serde_json_core::heapless::String<MAX_BLE_DATA_SIZE> =
            serde_json_core::to_string(self).unwrap_or_default();
        Vec::from_slice(json.as_bytes()).unwrap_or_default()
    }
}

/// Wallet information (public data only)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletInfo {
    /// Wallet name
    pub name: std::string::String,
    /// Ethereum address
    pub address: std::string::String,
    /// Creation timestamp
    pub created_at: u64,
    /// Is default wallet
    pub is_default: bool,
}

impl WalletInfo {
    pub fn new(
        name: &str,
        address: &str,
        created_at: u64,
        is_default: bool,
    ) -> Self {
        Self {
            name: std::string::String::from(name),
            address: std::string::String::from(address),
            created_at,
            is_default,
        }
    }
}

/// Balance information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BalanceInfo {
    /// Ethereum address
    pub address: std::string::String,
    /// Balance in wei (hex string)
    pub balance_wei: std::string::String,
    /// Balance in ETH (string)
    pub balance_eth: std::string::String,
    /// Chain name
    pub chain: std::string::String,
}

/// Transaction signing request
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignRequest {
    /// Transaction data (hex)
    pub tx_data: std::string::String,
    /// Chain ID
    pub chain_id: u64,
}

/// Transaction signing response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignResponse {
    /// Signature (hex)
    pub signature: std::string::String,
    /// Signed transaction data (hex)
    pub signed_tx: std::string::String,
}
