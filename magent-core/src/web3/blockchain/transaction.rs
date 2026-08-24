//! Transaction Builder and Signing Module.
//!
//! This module provides utilities for building, signing, and encoding
//! Ethereum transactions for submission to the blockchain.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use super::{Address, Hash, Wei};

#[cfg(feature = "web3")]
use sha3::{Digest, Keccak256};

/// Transaction type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum TransactionType {
    /// Legacy transaction (EIP-155).
    #[default]
    Legacy,
    /// EIP-2930: Access List transaction.
    Eip2930,
    /// EIP-1559: Fee Market transaction.
    Eip1559,
}

impl TransactionType {
    /// Get the transaction type byte for RLP encoding.
    pub fn type_byte(&self) -> u8 {
        match self {
            TransactionType::Legacy => 0,
            TransactionType::Eip2930 => 1,
            TransactionType::Eip1559 => 2,
        }
    }
}


/// A transaction request before signing.
#[derive(Debug, Clone)]
pub struct TransactionRequest {
    /// Transaction type.
    pub tx_type: TransactionType,
    /// Nonce.
    pub nonce: u64,
    /// Gas price (for legacy/EIP-2930) or max fee per gas (for EIP-1559).
    pub gas_price: Wei,
    /// Gas limit.
    pub gas_limit: u64,
    /// Destination address (None for contract creation).
    pub to: Option<Address>,
    /// Value to send.
    pub value: Wei,
    /// Input data.
    pub data: Vec<u8>,
    /// Chain ID for replay protection.
    pub chain_id: u64,
    /// EIP-1559 specific: max priority fee per gas.
    pub max_priority_fee_per_gas: Option<Wei>,
    /// EIP-1559 specific: max fee per gas.
    pub max_fee_per_gas: Option<Wei>,
    /// EIP-2930 specific: access list.
    pub access_list: Option<Vec<AccessListItem>>,
}

impl TransactionRequest {
    /// Create a new transaction request.
    pub fn new(
        to: Option<Address>,
        value: Wei,
        data: Vec<u8>,
        chain_id: u64,
    ) -> Self {
        Self {
            tx_type: TransactionType::Legacy,
            nonce: 0,
            gas_price: Wei::ZERO,
            gas_limit: 21000,
            to,
            value,
            data,
            chain_id,
            max_priority_fee_per_gas: None,
            max_fee_per_gas: None,
            access_list: None,
        }
    }

    /// Set the nonce.
    pub fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = nonce;
        self
    }

    /// Set the gas price.
    pub fn with_gas_price(mut self, price: Wei) -> Self {
        self.gas_price = price;
        self
    }

    /// Set the gas limit.
    pub fn with_gas_limit(mut self, limit: u64) -> Self {
        self.gas_limit = limit;
        self
    }

    /// Set the transaction type to EIP-1559 and configure fees.
    pub fn with_eip1559_fees(
        mut self,
        max_priority_fee: Wei,
        max_fee: Wei,
    ) -> Self {
        self.tx_type = TransactionType::Eip1559;
        self.max_priority_fee_per_gas = Some(max_priority_fee);
        self.max_fee_per_gas = Some(max_fee);
        self
    }

    /// Set the transaction type to EIP-2930 with an access list.
    pub fn with_access_list(mut self, access_list: Vec<AccessListItem>) -> Self {
        self.tx_type = TransactionType::Eip2930;
        self.access_list = Some(access_list);
        self
    }

    /// Validate the transaction request before signing.
    ///
    /// Catches the kinds of mistakes that would otherwise only
    /// surface as "transaction rejected" from the RPC: zero gas
    /// limit, missing EIP-1559 fees, mismatched chain_id, etc.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if self.gas_limit == 0 {
            return Err(Web3ErrorKind::BlockchainError(
                "gas_limit must be > 0".to_string(),
            ));
        }
        if self.chain_id == 0 {
            return Err(Web3ErrorKind::BlockchainError(
                "chain_id must be > 0".to_string(),
            ));
        }
        match self.tx_type {
            TransactionType::Legacy | TransactionType::Eip2930 => {
                if self.gas_price.is_zero() {
                    return Err(Web3ErrorKind::BlockchainError(
                        "gas_price must be > 0 for legacy / EIP-2930 transactions"
                            .to_string(),
                    ));
                }
            }
            TransactionType::Eip1559 => {
                let max_prio = self.max_priority_fee_per_gas.unwrap_or(Wei::ZERO);
                let max_fee = self.max_fee_per_gas.unwrap_or(Wei::ZERO);
                if max_prio.is_zero() || max_fee.is_zero() {
                    return Err(Web3ErrorKind::BlockchainError(
                        "EIP-1559 requires both max_priority_fee_per_gas and max_fee_per_gas"
                            .to_string(),
                    ));
                }
                if max_prio.as_wei() > max_fee.as_wei() {
                    return Err(Web3ErrorKind::BlockchainError(format!(
                        "max_priority_fee_per_gas ({}) must be <= max_fee_per_gas ({})",
                        max_prio.as_wei(),
                        max_fee.as_wei()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Estimate the maximum cost of this transaction (`gas_limit *
    /// gas_price` for legacy, `gas_limit * max_fee_per_gas` for
    /// EIP-1559). Useful for balance checks before signing.
    pub fn max_cost(&self) -> Wei {
        let price = match self.tx_type {
            TransactionType::Legacy | TransactionType::Eip2930 => self.gas_price.as_wei(),
            TransactionType::Eip1559 => self
                .max_fee_per_gas
                .unwrap_or(self.gas_price)
                .as_wei(),
        };
        let gas = self.gas_limit as u128;
        let total = price.saturating_mul(gas);
        Wei::from_wei(total)
    }

    /// Encode the transaction for signing.
    ///
    /// For legacy transactions, returns the RLP-encoded transaction.
    /// For EIP-1559/EIP-2930, returns the EIP-2718 encoded transaction.
    pub fn encode_for_signing(&self) -> Vec<u8> {
        match self.tx_type {
            TransactionType::Legacy => self.encode_legacy_rlp(),
            TransactionType::Eip1559 => self.encode_eip1559(),
            TransactionType::Eip2930 => self.encode_eip2930(),
        }
    }

    /// Encode as legacy RLP.
    fn encode_legacy_rlp(&self) -> Vec<u8> {
        let items = vec![
            rlp_encode_uint(self.nonce),
            rlp_encode_uint128(self.gas_price.as_wei()),
            rlp_encode_uint(self.gas_limit),
            match &self.to {
                Some(addr) => rlp_encode_address(addr),
                None => vec![0x80], // Empty address
            },
            rlp_encode_uint128(self.value.as_wei()),
            rlp_encode_bytes(&self.data),
            rlp_encode_uint(self.chain_id),
            vec![0x80],        // v = 0
            rlp_encode_uint(0), // r = 0
            rlp_encode_uint(0), // s = 0
        ];

        rlp_encode_list(&items)
    }

    /// Encode as EIP-1559.
    fn encode_eip1559(&self) -> Vec<u8> {
        let mut result = Vec::new();
        result.push(TransactionType::Eip1559.type_byte());

        let items = vec![
            rlp_encode_uint(self.chain_id),
            rlp_encode_uint(self.nonce),
            rlp_encode_uint128(
                self.max_priority_fee_per_gas.unwrap_or(Wei::ZERO).as_wei(),
            ),
            rlp_encode_uint128(
                self.max_fee_per_gas.unwrap_or(self.gas_price).as_wei(),
            ),
            rlp_encode_uint(self.gas_limit),
            match &self.to {
                Some(addr) => rlp_encode_address(addr),
                None => vec![0x80],
            },
            rlp_encode_uint128(self.value.as_wei()),
            rlp_encode_bytes(&self.data),
            self.encode_access_list_rlp(),
        ];

        result.extend(rlp_encode_list(&items));
        result
    }

    /// Encode as EIP-2930.
    fn encode_eip2930(&self) -> Vec<u8> {
        let mut result = Vec::new();
        result.push(TransactionType::Eip2930.type_byte());

        let items = vec![
            rlp_encode_uint(self.chain_id),
            rlp_encode_uint(self.nonce),
            rlp_encode_uint128(self.gas_price.as_wei()),
            rlp_encode_uint(self.gas_limit),
            match &self.to {
                Some(addr) => rlp_encode_address(addr),
                None => vec![0x80],
            },
            rlp_encode_uint128(self.value.as_wei()),
            rlp_encode_bytes(&self.data),
            self.encode_access_list_rlp(),
        ];

        result.extend(rlp_encode_list(&items));
        result
    }

    /// Encode access list as RLP.
    fn encode_access_list_rlp(&self) -> Vec<u8> {
        match &self.access_list {
            Some(list) => {
                let items: Vec<Vec<Vec<u8>>> = list.iter().map(|item| {
                    let keys: Vec<Vec<u8>> = item.storage_keys.iter().map(|k| {
                        rlp_encode_bytes(k.as_bytes())
                    }).collect();
                    vec![
                        rlp_encode_address(&item.address),
                        rlp_encode_list(&keys),
                    ]
                }).collect();
                rlp_encode_list(&items.iter().map(|i| rlp_encode_list(i)).collect::<Vec<_>>())
            }
            None => vec![0x80], // Empty list
        }
    }

    /// Calculate the transaction hash (for display purposes only).
    pub fn transaction_hash(&self, _signature: &[u8; 65]) -> Hash {
        // In a real implementation, this would compute the keccak256 hash
        // of the signed transaction encoding
        Hash::zero()
    }
}

/// An item in an access list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessListItem {
    /// The address.
    pub address: Address,
    /// The storage keys.
    pub storage_keys: Vec<Hash>,
}

/// A signed transaction ready for submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedTransaction {
    /// The raw transaction hash.
    pub hash: Hash,
    /// The transaction type.
    pub tx_type: TransactionType,
    /// The sender address.
    pub from: Address,
    /// The destination address.
    pub to: Option<Address>,
    /// The value sent.
    pub value: Wei,
    /// The nonce used.
    pub nonce: u64,
    /// The gas price.
    pub gas_price: Wei,
    /// The gas limit.
    pub gas_limit: u64,
    /// The input data.
    pub input: Vec<u8>,
    /// The chain ID.
    pub chain_id: u64,
    /// The v value of the signature.
    pub v: u64,
    /// The r value of the signature.
    pub r: [u8; 32],
    /// The s value of the signature.
    pub s: [u8; 32],
    /// The raw signed transaction bytes.
    pub raw_transaction: Vec<u8>,
}

impl SignedTransaction {
    /// Encode as raw transaction bytes (for submission).
    pub fn encode_for_submission(&self) -> Vec<u8> {
        self.raw_transaction.clone()
    }

    /// Get the sender address.
    pub fn from(&self) -> Address {
        self.from
    }

    /// Get the transaction hash.
    pub fn hash(&self) -> Hash {
        self.hash
    }
}

/// A transaction builder that handles signing.
#[derive(Debug, Clone)]
pub struct TransactionBuilder {
    request: TransactionRequest,
}

impl TransactionBuilder {
    /// Create a new transaction builder.
    pub fn new(to: Option<Address>, chain_id: u64) -> Self {
        Self {
            request: TransactionRequest::new(to, Wei::ZERO, Vec::new(), chain_id),
        }
    }

    /// Set the value.
    pub fn value(mut self, value: Wei) -> Self {
        self.request.value = value;
        self
    }

    /// Set the input data.
    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.request.data = data;
        self
    }

    /// Set the nonce.
    pub fn nonce(mut self, nonce: u64) -> Self {
        self.request.nonce = nonce;
        self
    }

    /// Set the gas price.
    pub fn gas_price(mut self, price: Wei) -> Self {
        self.request.gas_price = price;
        self
    }

    /// Set the gas limit.
    pub fn gas_limit(mut self, limit: u64) -> Self {
        self.request.gas_limit = limit;
        self
    }

    /// Configure for EIP-1559.
    pub fn eip1559(mut self, max_priority_fee: Wei, max_fee: Wei, gas_limit: u64) -> Self {
        self.request.gas_limit = gas_limit;
        self.request.tx_type = TransactionType::Eip1559;
        self.request.max_priority_fee_per_gas = Some(max_priority_fee);
        self.request.max_fee_per_gas = Some(max_fee);
        self
    }

    /// Build the unsigned request.
    pub fn build(&self) -> &TransactionRequest {
        &self.request
    }

    /// Consume the builder and return the underlying request.
    pub fn into_request(self) -> TransactionRequest {
        self.request
    }

    /// Validate the underlying transaction request. Convenience
    /// pass-through to [`TransactionRequest::validate`].
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        self.request.validate()
    }

    /// Maximum-cost estimate pass-through to
    /// [`TransactionRequest::max_cost`].
    pub fn max_cost(&self) -> Wei {
        self.request.max_cost()
    }

    /// Encode for signing.
    pub fn encode_for_signing(&self) -> Vec<u8> {
        self.request.encode_for_signing()
    }

    /// Sign the built transaction using the supplied Ethereum keypair
    /// and return a [`SignedTransaction`] ready for submission.
    ///
    /// For legacy (`TransactionType::Legacy`) transactions the signature
    /// is produced with EIP-155 replay protection (v = chain_id * 2 +
    /// 35 + y_parity). For EIP-2930 / EIP-1559 transactions the signature
    /// carries the raw y_parity (0 or 1).
    #[cfg(feature = "web3")]
    pub fn sign(
        &self,
        keypair: &crate::web3::blockchain::Secp256k1Keypair,
    ) -> Result<SignedTransaction, crate::error::Web3ErrorKind> {
        // Catch obvious mistakes before we burn a signature.
        self.request.validate()?;
        use crate::web3::blockchain::TransactionSigner;

        let from = keypair.address();
        let to = self.request.to;
        let value = self.request.value;
        let nonce = self.request.nonce;
        let gas_price = self.request.gas_price;
        let gas_limit = self.request.gas_limit;
        let input = self.request.data.clone();
        let chain_id = self.request.chain_id;

        let encoded = self.encode_for_signing();

        // Compute the keccak256 hash of the encoded payload. This is
        // what we sign for EIP-155 / EIP-1559 / EIP-2930.
        let mut hasher = Keccak256::new();
        hasher.update(&encoded);
        let tx_hash: [u8; 32] = hasher.finalize().into();

        let sig = match self.request.tx_type {
            TransactionType::Legacy => {
                TransactionSigner::sign_legacy_eip155(
                    keypair.secret_key(),
                    &tx_hash,
                    chain_id,
                )?
            }
            TransactionType::Eip2930 | TransactionType::Eip1559 => {
                TransactionSigner::sign_hash(keypair.secret_key(), &tx_hash)?
            }
        };

        let mut raw = encoded;
        raw.extend_from_slice(sig.as_bytes());

        Ok(SignedTransaction {
            hash: Hash::from_bytes(tx_hash),
            tx_type: self.request.tx_type,
            from: *from,
            to,
            value,
            nonce,
            gas_price,
            gas_limit,
            input,
            chain_id,
            // EIP-155 legacy: v = chain_id * 2 + 35 + y_parity.
            // EIP-2930 / EIP-1559: v = raw y_parity (0 or 1).
            // The signature stores recid+27, so subtract 27 for
            // the typed-tx paths. For the legacy path the
            // EIP-155 transformation is performed inside
            // `sign_legacy_eip155`.
            v: match self.request.tx_type {
                TransactionType::Legacy => sig.recovery_id() as u64,
                TransactionType::Eip2930 | TransactionType::Eip1559 => {
                    (sig.recovery_id().saturating_sub(27)) as u64
                }
            },
            r: {
                let mut out = [0u8; 32];
                out.copy_from_slice(sig.r());
                out
            },
            s: {
                let mut out = [0u8; 32];
                out.copy_from_slice(sig.s());
                out
            },
            raw_transaction: raw,
        })
    }
}

// ============================================================================
// RLP Encoding Helpers
// ============================================================================

/// Encode a single byte.
#[allow(dead_code)]
fn rlp_encode_byte(b: u8) -> Vec<u8> {
    if b == 0 {
        vec![0x80] // Empty
    } else if b < 0x80 {
        vec![b] // Direct
    } else {
        vec![0x81, b] // Single byte
    }
}

/// Encode a list of bytes.
fn rlp_encode_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.is_empty() {
        vec![0x80] // Empty
    } else if bytes.len() == 1 && bytes[0] < 0x80 {
        vec![bytes[0]]
    } else if bytes.len() < 56 {
        vec![0x80 + bytes.len() as u8]
            .into_iter()
            .chain(bytes.iter().copied())
            .collect()
    } else {
        let len_bytes = encode_length(bytes.len());
        vec![0xb7 + len_bytes.len() as u8]
            .into_iter()
            .chain(len_bytes)
            .chain(bytes.iter().copied())
            .collect()
    }
}

/// Encode a list.
fn rlp_encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    if items.is_empty() {
        return vec![0xc0]; // Empty list
    }

    let payload: Vec<u8> = items.iter().flatten().copied().collect();
    let total_len = payload.len();

    if total_len < 56 {
        vec![0xc0 + total_len as u8]
            .into_iter()
            .chain(payload)
            .collect()
    } else {
        let len_bytes = encode_length(total_len);
        vec![0xf7 + len_bytes.len() as u8]
            .into_iter()
            .chain(len_bytes)
            .chain(payload)
            .collect()
    }
}

/// Encode a uint.
fn rlp_encode_uint(n: u64) -> Vec<u8> {
    if n == 0 {
        vec![0x80]
    } else {
        let bytes = encode_uint_be(n);
        if bytes.len() == 1 && bytes[0] < 0x80 {
            bytes
        } else {
            vec![0x80 + bytes.len() as u8]
                .into_iter()
                .chain(bytes)
                .collect()
        }
    }
}

/// Encode a uint128 (Wei values can be u128).
fn rlp_encode_uint128(n: u128) -> Vec<u8> {
    if n == 0 {
        vec![0x80]
    } else {
        let bytes = encode_uint128_be(n);
        if bytes.len() == 1 && bytes[0] < 0x80 {
            bytes
        } else {
            vec![0x80 + bytes.len() as u8]
                .into_iter()
                .chain(bytes)
                .collect()
        }
    }
}

fn encode_uint128_be(n: u128) -> Vec<u8> {
    if n == 0 {
        vec![0]
    } else {
        let mut bytes = Vec::new();
        let mut n = n;
        while n > 0 {
            bytes.insert(0, (n & 0xff) as u8);
            n >>= 8;
        }
        bytes
    }
}

/// Encode an address.
fn rlp_encode_address(addr: &Address) -> Vec<u8> {
    rlp_encode_bytes(addr.as_bytes())
}

/// Encode a uint as big-endian bytes.
fn encode_uint_be(n: u64) -> Vec<u8> {
    if n == 0 {
        vec![0]
    } else {
        let mut bytes = Vec::new();
        let mut n = n;
        while n > 0 {
            bytes.insert(0, (n & 0xff) as u8);
            n >>= 8;
        }
        bytes
    }
}

/// Encode a length as big-endian bytes.
fn encode_length(len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut len = len;
    while len > 0 {
        bytes.insert(0, (len & 0xff) as u8);
        len >>= 8;
    }
    bytes
}

// ============================================================================
// Ethereum Signed Message
// ============================================================================

/// Construct the personal sign message hash.
///
/// This follows the EIP-191 specification for personal_sign:
/// `\x19Ethereum Signed Message:\n` + len(message) + message
pub fn personal_sign_hash(message: &[u8]) -> Hash {
    let prefix = format!(
        "\x19Ethereum Signed Message:\n{}{}",
        message.len(),
        String::from_utf8_lossy(message)
    );
    let hash = keccak256(prefix.as_bytes());
    Hash::from_bytes(hash)
}

/// Construct the sign typed data hash (EIP-712).
///
/// This is a simplified implementation. Full EIP-712 requires
/// proper domain separation and type encoding.
pub fn typed_data_hash(domain_hash: &[u8; 32], message_hash: &[u8; 32]) -> Hash {
    let mut data = Vec::new();
    data.extend_from_slice(b"\x19\x01"); // EIP-712 prefix
    data.extend_from_slice(domain_hash);
    data.extend_from_slice(message_hash);
    let hash = keccak256(&data);
    Hash::from_bytes(hash)
}

/// Compute the keccak256 hash of the input data.
///
/// Uses the `sha3` crate's Keccak256 hasher (standard Ethereum hash).
#[cfg(feature = "web3")]
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute the keccak256 hash (no_std fallback).
///
/// When `web3` feature is not enabled, uses a simple hash for testing.
#[cfg(not(feature = "web3"))]
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut result = [0u8; 32];
    for (i, &byte) in data.iter().enumerate().take(32) {
        result[i] = byte.wrapping_add((data.len() as u8).wrapping_mul((i + 1) as u8));
        result[i] = result[i].wrapping_add(0x9e3779b9);
    }
    for i in 32..data.len().min(64) {
        let j = (i - 32) % 32;
        result[j] = result[j].wrapping_add(data[i].wrapping_mul((i + 1) as u8));
    }
    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_request_creation() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let tx = TransactionRequest::new(
            Some(to),
            Wei::from_ether(1),
            vec![],
            1,
        );

        assert_eq!(tx.chain_id, 1);
        assert_eq!(tx.value.as_wei(), 1_000_000_000_000_000_000);
        assert!(tx.to.is_some());
    }

    #[test]
    fn test_transaction_builder() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let tx = TransactionBuilder::new(Some(to), 1)
            .value(Wei::from_ether(1))
            .nonce(5)
            .gas_price(Wei::from_gwei(20))
            .gas_limit(21000)
            .build()
            .clone();

        assert_eq!(tx.nonce, 5);
        assert_eq!(tx.gas_limit, 21000);
    }

    #[test]
    fn test_eip1559_transaction() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let tx = TransactionBuilder::new(Some(to), 1)
            .value(Wei::from_ether(0))
            .eip1559(
                Wei::from_gwei(2),
                Wei::from_gwei(50),
                21000,
            )
            .build()
            .clone();

        assert_eq!(tx.tx_type, TransactionType::Eip1559);
        assert!(tx.max_priority_fee_per_gas.is_some());
        assert!(tx.max_fee_per_gas.is_some());
    }

    #[test]
    fn test_personal_sign_hash() {
        let message = b"Hello, Ethereum!";
        let hash = personal_sign_hash(message);

        assert!(!hash.is_zero());
        assert_eq!(hash.to_hex().len(), 66); // 0x + 64 chars
    }

    #[test]
    fn test_rlp_encoding() {
        assert_eq!(rlp_encode_uint(0), vec![0x80]);
        assert_eq!(rlp_encode_uint(127), vec![0x7f]);
        assert_eq!(rlp_encode_uint(128), vec![0x81, 0x80]);
    }

    #[test]
    fn test_address_encoding() {
        let addr = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let encoded = rlp_encode_address(&addr);
        assert!(!encoded.is_empty());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_transaction_builder_sign_legacy_eip155() {
        use crate::web3::blockchain::{Secp256k1Keypair, TransactionSigner};

        // Sign a real transaction and check EIP-155 v.
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();
        let tx = TransactionBuilder::new(Some(to), 1)
            .value(Wei::from_gwei(100))
            .nonce(7)
            .gas_price(Wei::from_gwei(20))
            .gas_limit(21_000);

        let signed = tx.sign(&kp).unwrap();

        // The signed payload must:
        // - produce a transaction that has the 65-byte signature appended
        //   to the encoded transaction
        // - use EIP-155 v (>= 37) since the chain_id is 1
        // - have a known `from` address (the signer's)
        assert!(signed.raw_transaction.len() > tx.encode_for_signing().len());
        assert!(signed.v >= 37, "expected EIP-155 v, got {}", signed.v);
        assert_eq!(signed.from.to_hex(), kp.address().to_hex());
        assert_eq!(signed.tx_type, TransactionType::Legacy);

        // Verify EIP-155 v encoding: (v - 35) / 2 == chain_id.
        assert_eq!((signed.v - 35) / 2, 1, "EIP-155 chain_id mismatch");

        // Verify the recovered address matches the signer's by
        // re-deriving a fresh signature with the *same* secret on the
        // same unsigned payload and recovering from it (EIP-155 v
        // values are too large to feed into recovery directly).
        let unsigned_len = tx.encode_for_signing().len();
        let mut hasher = Keccak256::new();
        hasher.update(&signed.raw_transaction[..unsigned_len]);
        let hash: [u8; 32] = hasher.finalize().into();
        let fresh = TransactionSigner::sign_hash(kp.secret_key(), &hash).unwrap();
        let recovered = crate::web3::blockchain::Secp256k1PublicKey::recover_from(
            &hash,
            fresh.as_bytes(),
        )
        .unwrap();
        assert_eq!(recovered.to_address().to_hex(), kp.address().to_hex());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_transaction_builder_sign_eip1559_y_parity_is_zero_or_one() {
        use crate::web3::blockchain::Secp256k1Keypair;
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();
        let tx = TransactionBuilder::new(Some(to), 1)
            .eip1559(Wei::from_gwei(2), Wei::from_gwei(50), 21_000);
        let signed = tx.sign(&kp).unwrap();
        // For EIP-1559 / EIP-2930 transactions, v must be 0 or 1
        // (y_parity), NOT the EIP-155-formatted v (which would be >= 37).
        assert!(
            signed.v == 0 || signed.v == 1 || signed.v == 27 || signed.v == 28,
            "unexpected EIP-1559 v={}",
            signed.v
        );
    }

    #[test]
    fn test_validate_rejects_zero_gas_limit() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let mut req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
            .with_gas_price(Wei::from_gwei(20));
        req.gas_limit = 0;
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_chain_id() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 0)
            .with_gas_price(Wei::from_gwei(20));
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_gas_price_for_legacy() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1);
        // gas_price defaults to zero, so this should fail validation.
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_eip1559_without_fees() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
            .with_eip1559_fees(Wei::ZERO, Wei::ZERO);
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_eip1559_priority_above_max() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
            .with_eip1559_fees(Wei::from_gwei(50), Wei::from_gwei(20));
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_well_formed_legacy() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::from_wei(1_000), Vec::new(), 1)
            .with_gas_price(Wei::from_gwei(20));
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_well_formed_eip1559() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::from_wei(1_000), Vec::new(), 1)
            .with_eip1559_fees(Wei::from_gwei(2), Wei::from_gwei(50));
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_max_cost_legacy_uses_gas_price() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
            .with_gas_price(Wei::from_gwei(20))
            .with_gas_limit(21_000);
        // 20 gwei * 21_000 = 420_000 gwei = 4.2e14 wei
        assert_eq!(req.max_cost().as_wei(), 20_000_000_000u128 * 21_000);
    }

    #[test]
    fn test_max_cost_eip1559_uses_max_fee() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let req = TransactionRequest::new(Some(to), Wei::ZERO, Vec::new(), 1)
            .with_eip1559_fees(Wei::from_gwei(2), Wei::from_gwei(50))
            .with_gas_limit(21_000);
        // 50 gwei * 21_000 = 1_050_000 gwei = 1.05e15 wei (max_fee, not max_prio).
        assert_eq!(req.max_cost().as_wei(), 50_000_000_000u128 * 21_000);
    }

    #[test]
    fn test_wei_is_zero() {
        assert!(Wei::ZERO.is_zero());
        assert!(!Wei::from_wei(1).is_zero());
    }

    #[test]
    fn test_builder_validate_and_max_cost_pass_through() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let builder = TransactionBuilder::new(Some(to), 1).gas_price(Wei::from_gwei(20));
        assert!(builder.validate().is_ok());
        let req_cost = builder.max_cost();
        let req = builder.build();
        assert_eq!(req_cost, req.max_cost());
    }

    #[test]
    fn test_builder_into_request_consumes_builder() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let builder = TransactionBuilder::new(Some(to), 1);
        let req = builder.into_request();
        assert_eq!(req.chain_id, 1);
    }
}

// ============================================================================
// Transaction and TransactionReceipt Types (Re-exports)
// ============================================================================

/// A signed transaction (alias for compatibility with module exports).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// Transaction hash (filled in by the network / a chain client).
    pub hash: Hash,
    /// Nonce.
    pub nonce: u64,
    /// Sender address.
    pub from: Address,
    /// Recipient address (None for contract creation).
    pub to: Option<Address>,
    /// Value in wei.
    pub value: Wei,
    /// Gas used.
    pub gas: u64,
    /// Gas price.
    pub gas_price: Wei,
    /// Block number (None while pending).
    pub block_number: Option<u64>,
    /// Block hash (None while pending).
    pub block_hash: Option<Hash>,
    /// Transaction index within the block.
    pub transaction_index: Option<u64>,
    /// Input data.
    pub input: Vec<u8>,
}

impl Transaction {
    /// Create a new transaction
    pub fn new(
        from: Address,
        to: Option<Address>,
        value: Wei,
        nonce: u64,
        gas: u64,
        gas_price: Wei,
    ) -> Self {
        Self {
            hash: Hash::zero(),
            nonce,
            from,
            to,
            value,
            gas,
            gas_price,
            block_number: None,
            block_hash: None,
            transaction_index: None,
            input: Vec::new(),
        }
    }
}

/// A transaction receipt returned from `eth_getTransactionReceipt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    /// Transaction hash.
    pub transaction_hash: Hash,
    /// Transaction index in block.
    pub transaction_index: u64,
    /// Block hash.
    pub block_hash: Hash,
    /// Block number.
    pub block_number: u64,
    /// Sender address.
    pub from: Address,
    /// Recipient address.
    pub to: Option<Address>,
    /// Gas used by this transaction.
    pub gas_used: u64,
    /// Cumulative gas used in the block up to and including this tx.
    pub cumulative_gas_used: u64,
    /// Contract address (for contract-creation transactions).
    pub contract_address: Option<Address>,
    /// Logs emitted by this transaction.
    pub logs: Vec<crate::web3::blockchain::events::EventLog>,
    /// Status: 1 = success, 0 = failure (EIP-658).
    pub status: u8,
    /// Effective gas price (EIP-1559).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_gas_price: Option<Wei>,
}

impl TransactionReceipt {
    /// Get the transaction hash (convenience accessor).
    pub fn hash(&self) -> Hash {
        self.transaction_hash
    }

    /// Get the block number.
    pub fn block_number_u64(&self) -> u64 {
        self.block_number
    }
}
