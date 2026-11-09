//! Transaction Builder and Signing Module.
//!
//! This module provides utilities for building, signing, and encoding
//! Ethereum transactions for submission to the blockchain.

use alloc::string::ToString;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use super::{Address, Hash, Wei};
use crate::error::Web3ErrorKind;

#[cfg(feature = "web3")]
use sha3::{Digest, Keccak256};

/// Transaction type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    pub fn new(to: Option<Address>, value: Wei, data: Vec<u8>, chain_id: u64) -> Self {
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
    pub fn with_eip1559_fees(mut self, max_priority_fee: Wei, max_fee: Wei) -> Self {
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
                        "gas_price must be > 0 for legacy / EIP-2930 transactions".to_string(),
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
            TransactionType::Eip1559 => self.max_fee_per_gas.unwrap_or(self.gas_price).as_wei(),
        };
        let gas = self.gas_limit as u128;
        let total = price.saturating_mul(gas);
        Wei::from_wei(total)
    }

    /// Encode the transaction for signing.
    ///
    /// For a legacy transaction this is the EIP-155 signing form — the six
    /// transaction fields, the chain id, then two zero placeholders standing in
    /// for `r` and `s` (the chain id takes the `v` slot), nine items in total.
    /// For EIP-1559/EIP-2930 it is the EIP-2718 envelope without a signature.
    pub fn encode_for_signing(&self) -> Vec<u8> {
        match self.tx_type {
            TransactionType::Legacy => self.encode_legacy_rlp(),
            TransactionType::Eip1559 | TransactionType::Eip2930 => self.encode_typed_unsigned(),
        }
    }

    /// Encode the destination, or an empty string for contract creation.
    fn encode_to_rlp(&self) -> Vec<u8> {
        match &self.to {
            Some(addr) => rlp_encode_address(addr),
            None => vec![0x80],
        }
    }

    /// The EIP-155 signing form of a legacy transaction.
    fn encode_legacy_rlp(&self) -> Vec<u8> {
        let items = vec![
            rlp_encode_uint(self.nonce),
            rlp_encode_uint128(self.gas_price.as_wei()),
            rlp_encode_uint(self.gas_limit),
            self.encode_to_rlp(),
            rlp_encode_uint128(self.value.as_wei()),
            rlp_encode_bytes(&self.data),
            rlp_encode_uint(self.chain_id),
            vec![0x80], // 0 — placeholder for `r`
            vec![0x80], // 0 — placeholder for `s`
        ];

        rlp_encode_list(&items)
    }

    /// Encode the *signed* form of a legacy transaction: the six fields, then
    /// `v`, `r` and `s` as their own items.
    ///
    /// A legacy transaction is a plain RLP list, not an EIP-2718 envelope, so
    /// the signed encoding cannot be produced by appending the 65 signature
    /// bytes to the signing payload — and `v` must be encoded as a full integer
    /// (309 for Polygon), not as the signature's single `v` byte.
    fn encode_legacy_signed(&self, v: u64, r: &[u8; 32], s: &[u8; 32]) -> Vec<u8> {
        let items = vec![
            rlp_encode_uint(self.nonce),
            rlp_encode_uint128(self.gas_price.as_wei()),
            rlp_encode_uint(self.gas_limit),
            self.encode_to_rlp(),
            rlp_encode_uint128(self.value.as_wei()),
            rlp_encode_bytes(&self.data),
            rlp_encode_uint(v),
            rlp_encode_u256_be(r),
            rlp_encode_u256_be(s),
        ];

        rlp_encode_list(&items)
    }

    /// The RLP field list a typed transaction shares between its unsigned and
    /// signed encodings — everything before the signature.
    fn typed_unsigned_items(&self) -> Vec<Vec<u8>> {
        match self.tx_type {
            TransactionType::Eip1559 => vec![
                rlp_encode_uint(self.chain_id),
                rlp_encode_uint(self.nonce),
                rlp_encode_uint128(self.max_priority_fee_per_gas.unwrap_or(Wei::ZERO).as_wei()),
                rlp_encode_uint128(self.max_fee_per_gas.unwrap_or(self.gas_price).as_wei()),
                rlp_encode_uint(self.gas_limit),
                self.encode_to_rlp(),
                rlp_encode_uint128(self.value.as_wei()),
                rlp_encode_bytes(&self.data),
                self.encode_access_list_rlp(),
            ],
            // Legacy is never routed here — it is an RLP list, not an EIP-2718
            // envelope — but the arm has to exist for the match to be total.
            TransactionType::Eip2930 | TransactionType::Legacy => vec![
                rlp_encode_uint(self.chain_id),
                rlp_encode_uint(self.nonce),
                rlp_encode_uint128(self.gas_price.as_wei()),
                rlp_encode_uint(self.gas_limit),
                self.encode_to_rlp(),
                rlp_encode_uint128(self.value.as_wei()),
                rlp_encode_bytes(&self.data),
                self.encode_access_list_rlp(),
            ],
        }
    }

    /// Encode a typed transaction (EIP-1559 / EIP-2930) without a signature.
    fn encode_typed_unsigned(&self) -> Vec<u8> {
        let mut result = vec![self.tx_type.type_byte()];
        result.extend(rlp_encode_list(&self.typed_unsigned_items()));
        result
    }

    /// Encode the *signed* form of a typed transaction: the type byte, the
    /// field list, then `y_parity`, `r` and `s`.
    ///
    /// A typed signature carries the raw `y_parity` (0 or 1), not the
    /// `recid + 27` that [`crate::web3::blockchain::EthereumSignature`] stores
    /// for `personal_sign`/recovery.
    fn encode_typed_signed(&self, y_parity: u64, r: &[u8; 32], s: &[u8; 32]) -> Vec<u8> {
        let mut items = self.typed_unsigned_items();
        items.push(rlp_encode_uint(y_parity));
        items.push(rlp_encode_u256_be(r));
        items.push(rlp_encode_u256_be(s));

        let mut result = vec![self.tx_type.type_byte()];
        result.extend(rlp_encode_list(&items));
        result
    }

    /// Encode access list as RLP.
    fn encode_access_list_rlp(&self) -> Vec<u8> {
        match &self.access_list {
            Some(list) => {
                let items: Vec<Vec<Vec<u8>>> = list
                    .iter()
                    .map(|item| {
                        let keys: Vec<Vec<u8>> = item
                            .storage_keys
                            .iter()
                            .map(|k| rlp_encode_bytes(k.as_bytes()))
                            .collect();
                        vec![rlp_encode_address(&item.address), rlp_encode_list(&keys)]
                    })
                    .collect();
                rlp_encode_list(&items.iter().map(|i| rlp_encode_list(i)).collect::<Vec<_>>())
            }
            // RLP: an *empty list* is 0xc0; 0x80 is the empty *string*, which
            // nodes reject with "expected input list". `eip1559()` never sets
            // an access list, so every typed transaction hit this branch.
            None => vec![0xc0],
        }
    }

    /// Compute the keccak256 hash of this transaction as signed by
    /// `signature`.
    ///
    /// `signature` is the 65-byte `r ‖ s ‖ v` form
    /// ([`crate::web3::blockchain::EthereumSignature`]),
    /// whose `v` byte is the canonical `recid + 27`. For a legacy request the
    /// on-chain `v` is reconstructed as the EIP-155 value
    /// `chain_id * 2 + 35 + y_parity`, exactly as [`TransactionBuilder::sign`]
    /// encodes it; a typed request carries the raw `y_parity`.
    ///
    /// This is the hash of the *signed* encoding — the one an RPC echoes back
    /// for the submitted transaction and the one `poll_transaction` looks up —
    /// not the hash of the signing payload. The previous implementation
    /// returned `Hash::zero()` for every input, so a caller that displayed the
    /// result would print an all-zero hash that resolves to nothing.
    #[cfg(feature = "web3")]
    pub fn transaction_hash(&self, signature: &[u8; 65]) -> Hash {
        let sig = crate::web3::blockchain::EthereumSignature::from_bytes(*signature);
        let y_parity = u64::from(sig.recovery_id().saturating_sub(27));
        let raw = match self.tx_type {
            TransactionType::Legacy => {
                let v = self
                    .chain_id
                    .saturating_mul(2)
                    .saturating_add(35 + y_parity);
                self.encode_legacy_signed(v, sig.r(), sig.s())
            }
            TransactionType::Eip2930 | TransactionType::Eip1559 => {
                self.encode_typed_signed(y_parity, sig.r(), sig.s())
            }
        };
        let mut hasher = Keccak256::new();
        hasher.update(&raw);
        Hash::from_bytes(hasher.finalize().into())
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

        // (`signature` keeps the canonical `recid + 27` byte so it can be fed
        // back to `verify`/`recover_from`; `v` is the value that goes on-chain.)
        let (signature, v) = match self.request.tx_type {
            TransactionType::Legacy => {
                TransactionSigner::sign_legacy_eip155(keypair.secret_key(), &tx_hash, chain_id)?
            }
            TransactionType::Eip2930 | TransactionType::Eip1559 => {
                let sig = TransactionSigner::sign_hash(keypair.secret_key(), &tx_hash)?;
                let y_parity = u64::from(sig.recovery_id().saturating_sub(27));
                (sig, y_parity)
            }
        };

        // Build the transaction as it goes on the wire. For legacy that is an
        // RLP list with `v`, `r`, `s` as their own items — appending the 65
        // signature bytes to the signing payload is not a valid transaction at
        // all — and for a typed transaction it is the EIP-2718 envelope with
        // the raw `y_parity` (0/1) in the signature slot.
        let raw = match self.request.tx_type {
            TransactionType::Legacy => {
                self.request
                    .encode_legacy_signed(v, signature.r(), signature.s())
            }
            TransactionType::Eip2930 | TransactionType::Eip1559 => self
                .request
                .encode_typed_signed(v, signature.r(), signature.s()),
        };

        // The hash of the signed transaction, not of the signing payload.
        let mut hasher = Keccak256::new();
        hasher.update(&raw);
        let tx_hash_signed: [u8; 32] = hasher.finalize().into();

        Ok(SignedTransaction {
            hash: Hash::from_bytes(tx_hash_signed),
            tx_type: self.request.tx_type,
            from: *from,
            to,
            value,
            nonce,
            gas_price,
            gas_limit,
            input,
            chain_id,
            v,
            r: {
                let mut out = [0u8; 32];
                out.copy_from_slice(signature.r());
                out
            },
            s: {
                let mut out = [0u8; 32];
                out.copy_from_slice(signature.s());
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

/// Encode a 256-bit big-endian integer as RLP, stripping leading zero bytes.
///
/// `r` and `s` are fixed-width 32-byte values, but RLP encodes them as
/// integers: a signature whose top byte is zero must not be written as a
/// 32-byte string, or the encoding (and the transaction hash it produces) is
/// wrong.
fn rlp_encode_u256_be(bytes: &[u8; 32]) -> Vec<u8> {
    let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    rlp_encode_bytes(&bytes[first_nonzero..])
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
    // Byte-concatenate, never `from_utf8_lossy`: replacing a non-UTF-8 byte
    // with U+FFFD changes the hashed bytes while the length prefix still
    // describes the original, so signing and verification disagreed for any
    // binary message.
    let mut data = Vec::new();
    data.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    data.extend_from_slice(message.len().to_string().as_bytes());
    data.extend_from_slice(message);
    Hash::from_bytes(keccak256(&data))
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
        let tx = TransactionRequest::new(Some(to), Wei::from_ether(1), vec![], 1);

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
            .eip1559(Wei::from_gwei(2), Wei::from_gwei(50), 21000)
            .build()
            .clone();

        assert_eq!(tx.tx_type, TransactionType::Eip1559);
        assert!(tx.max_priority_fee_per_gas.is_some());
        assert!(tx.max_fee_per_gas.is_some());
    }

    /// EIP-1559's access-list field must be an RLP *list* even when empty.
    /// The old `None => 0x80` encoded an empty string, which nodes reject.
    #[test]
    fn eip1559_empty_access_list_encodes_as_a_list() {
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let tx = TransactionBuilder::new(Some(to), 1)
            .eip1559(Wei::from_gwei(2), Wei::from_gwei(50), 21_000)
            .build()
            .clone();
        let encoded = tx.encode_for_signing();
        assert_eq!(encoded[0], 0x02, "EIP-1559 type byte");
        // The access list is the final field of the typed payload; an empty
        // RLP list is the single byte 0xc0 (0x80 would be an empty string).
        assert_eq!(encoded.last(), Some(&0xc0), "empty access list must be 0xc0");
    }

    #[test]
    fn test_personal_sign_hash() {
        let message = b"Hello, Ethereum!";
        let hash = personal_sign_hash(message);

        assert!(!hash.is_zero());
        assert_eq!(hash.to_hex().len(), 66); // 0x + 64 chars
    }

    /// EIP-191 hashes the raw message bytes. The old `from_utf8_lossy` path
    /// replaced 0xFF with the 3-byte U+FFFD while keeping the 2-byte length
    /// prefix, so the digest did not match what the signer signed.
    #[test]
    fn personal_sign_hash_keeps_raw_non_utf8_bytes() {
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x19Ethereum Signed Message:\n2h");
        expected.push(0xff);
        assert_eq!(
            personal_sign_hash(b"h\xff"),
            Hash::from_bytes(keccak256(&expected))
        );
    }

    /// A real `eth_getTransactionReceipt` response: camelCase keys, hex
    /// quantity strings, hex addresses/hashes. The derive used to expect
    /// snake_case and JSON numbers, so every real node response failed to
    /// deserialise.
    #[test]
    fn receipt_deserialises_from_a_real_rpc_payload() {
        let hash = format!("0x{}", "aa".repeat(32));
        let block_hash = format!("0x{}", "bb".repeat(32));
        let from = format!("0x{}", "cc".repeat(20));
        let json = format!(
            r#"{{"transactionHash":"{hash}","transactionIndex":"0x0","blockHash":"{block_hash}","blockNumber":"0x10","from":"{from}","to":null,"gasUsed":"0x5208","cumulativeGasUsed":"0x5208","contractAddress":null,"logs":[],"status":"0x1","effectiveGasPrice":"0x3b9aca00"}}"#
        );
        let receipt: TransactionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(receipt.block_number, 16);
        assert_eq!(receipt.transaction_index, 0);
        assert_eq!(receipt.status, 1);
        assert_eq!(receipt.gas_used, 21_000);
        assert_eq!(receipt.effective_gas_price, Some(Wei(1_000_000_000)));
        assert_eq!(receipt.transaction_hash.to_hex(), hash);
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
        use crate::web3::blockchain::Secp256k1Keypair;

        // Sign a real transaction and check EIP-155 v.
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();
        let tx = TransactionBuilder::new(Some(to), 1)
            .value(Wei::from_gwei(100))
            .nonce(7)
            .gas_price(Wei::from_gwei(20))
            .gas_limit(21_000);

        let signed = tx.sign(&kp).unwrap();

        assert_eq!(signed.from.to_hex(), kp.address().to_hex());
        assert_eq!(signed.tx_type, TransactionType::Legacy);
        assert_eq!((signed.v - 35) / 2, 1, "EIP-155 chain_id mismatch");

        // The signing payload is the EIP-155 form: nine items, the chain id
        // taking the `v` slot.
        assert_eq!(rlp_list_items(&tx.encode_for_signing()).len(), 9);

        // The signed transaction is an RLP list too, with `v`, `r` and `s` as
        // their own items — not the signing payload with 65 bytes glued on.
        let items = rlp_list_items(&signed.raw_transaction);
        assert_eq!(items.len(), 9);
        assert_eq!(rlp_item_u64(&items[6]), signed.v);
        assert_eq!(rlp_encode_bytes(&items[7]), rlp_encode_u256_be(&signed.r));
        assert_eq!(rlp_encode_bytes(&items[8]), rlp_encode_u256_be(&signed.s));

        // And it is a *valid* signature over that payload: rebuild a
        // recoverable 65-byte signature from the encoded `r`/`s` plus the
        // y_parity that `v` carries and recover the signer.
        let mut hasher = Keccak256::new();
        hasher.update(tx.encode_for_signing());
        let hash: [u8; 32] = hasher.finalize().into();
        let mut recoverable = [0u8; 65];
        recoverable[..32].copy_from_slice(&signed.r);
        recoverable[32..64].copy_from_slice(&signed.s);
        recoverable[64] = (27 + signed.v - (signed.chain_id * 2 + 35)) as u8;
        let recovered =
            crate::web3::blockchain::Secp256k1PublicKey::recover_from(&hash, &recoverable).unwrap();
        assert_eq!(recovered.to_address().to_hex(), kp.address().to_hex());
    }

    /// A legacy transaction on a chain whose EIP-155 `v` does not fit in a byte.
    ///
    /// `v = chain_id * 2 + 35 + y_parity` reaches 309 on Polygon (137), and
    /// `309 as u8` is 53. The signature is stored in a 65-byte array with a
    /// single `v` byte, so the old code truncated: the signed transaction
    /// carried `v = 53` and `(53 - 35) / 2 != 137`, and no node would accept
    /// it.
    #[cfg(feature = "web3")]
    #[test]
    fn sign_legacy_keeps_v_full_width_above_chain_id_110() {
        use crate::web3::blockchain::Secp256k1Keypair;
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();
        let tx = TransactionBuilder::new(Some(to), 137)
            .value(Wei::from_gwei(100))
            .nonce(7)
            .gas_price(Wei::from_gwei(20))
            .gas_limit(21_000);

        let signed = tx.sign(&kp).unwrap();

        assert!(
            signed.v == 309 || signed.v == 310,
            "expected EIP-155 v for chain 137, got {}",
            signed.v
        );
        assert_eq!((signed.v - 35) / 2, 137, "EIP-155 chain_id mismatch");

        // The value in the encoded transaction is the full-width v, not a byte.
        let items = rlp_list_items(&signed.raw_transaction);
        assert_eq!(items.len(), 9);
        assert_eq!(rlp_item_u64(&items[6]), signed.v);
        assert_ne!(items[6], rlp_encode_uint(u64::from(signed.v as u8)));
    }

    #[cfg(feature = "web3")]
    #[test]
    fn test_transaction_builder_sign_eip1559_y_parity_is_zero_or_one() {
        use crate::web3::blockchain::Secp256k1Keypair;
        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();
        let tx = TransactionBuilder::new(Some(to), 1).eip1559(
            Wei::from_gwei(2),
            Wei::from_gwei(50),
            21_000,
        );
        let signed = tx.sign(&kp).unwrap();
        // For EIP-1559 / EIP-2930 transactions, v must be 0 or 1
        // (y_parity), NOT the EIP-155-formatted v (which would be >= 37).
        assert!(signed.v <= 1, "unexpected EIP-1559 v={}", signed.v);

        // The raw bytes must agree with `v`: a typed signature carries the raw
        // y_parity, whereas `EthereumSignature` stores `recid + 27`. Appending
        // the signature verbatim put 27/28 on the wire.
        let items = rlp_list_items(&signed.raw_transaction[1..]);
        assert_eq!(items.len(), 12, "9 fields + y_parity + r + s");
        assert_eq!(rlp_item_u64(&items[9]), signed.v);
        assert_eq!(rlp_encode_bytes(&items[10]), rlp_encode_u256_be(&signed.r));
        assert_eq!(rlp_encode_bytes(&items[11]), rlp_encode_u256_be(&signed.s));
    }

    /// `transaction_hash` must return the hash of the *signed* encoding.
    ///
    /// The old body was a stub that returned `Hash::zero()` for every input,
    /// so a caller that displayed the result would print an all-zero hash that
    /// resolves to no transaction. Signing now produces the real hash, and
    /// this checks the standalone method agrees with it for both container
    /// formats.
    #[cfg(feature = "web3")]
    #[test]
    fn transaction_hash_matches_the_signed_encoding() {
        use crate::web3::blockchain::Secp256k1Keypair;

        let to = Address::from_hex("0x742d35Cc6634C0532925a3b844Bc9e7595f8bE21").unwrap();
        let kp = Secp256k1Keypair::generate();

        // Legacy on a chain whose EIP-155 `v` does not fit in a byte: the
        // method has to reconstruct `chain_id * 2 + 35 + y_parity`, since the
        // 65-byte signature only carries `recid + 27`.
        let legacy = TransactionBuilder::new(Some(to), 137)
            .value(Wei::from_gwei(100))
            .nonce(7)
            .gas_price(Wei::from_gwei(20))
            .gas_limit(21_000);
        let signed = legacy.sign(&kp).unwrap();
        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&signed.r);
        sig[32..64].copy_from_slice(&signed.s);
        sig[64] = (27 + signed.v - (signed.chain_id * 2 + 35)) as u8;
        let hash = legacy.build().transaction_hash(&sig);
        assert_ne!(hash, Hash::zero(), "a real signature must not hash to zero");
        assert_eq!(hash, signed.hash, "legacy hash mismatch");

        // Typed: the wire signature slot carries the raw y_parity, not
        // recid + 27, so a legacy-only reconstruction would hash the wrong
        // bytes.
        let typed = TransactionBuilder::new(Some(to), 1).eip1559(
            Wei::from_gwei(2),
            Wei::from_gwei(50),
            21_000,
        );
        let signed = typed.sign(&kp).unwrap();
        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&signed.r);
        sig[32..64].copy_from_slice(&signed.s);
        sig[64] = (27 + signed.v) as u8;
        let hash = typed.build().transaction_hash(&sig);
        assert_eq!(hash, signed.hash, "eip-1559 hash mismatch");
    }

    /// Read the items of a top-level RLP list, returning each item's content
    /// bytes (test helper — the module only ships encoders).
    fn rlp_list_items(data: &[u8]) -> Vec<Vec<u8>> {
        let (payload_start, payload_len) = match data[0] {
            0xc0..=0xf7 => (1usize, (data[0] - 0xc0) as usize),
            0xf8..=0xff => {
                let n = (data[0] - 0xf7) as usize;
                let mut len = 0usize;
                for &b in &data[1..1 + n] {
                    len = (len << 8) | b as usize;
                }
                (1 + n, len)
            }
            other => panic!("not an RLP list header: 0x{other:02x}"),
        };

        let end = payload_start + payload_len;
        let mut items = Vec::new();
        let mut i = payload_start;
        while i < end {
            match data[i] {
                0x00..=0x7f => {
                    items.push(vec![data[i]]);
                    i += 1;
                }
                0x80..=0xb7 => {
                    let l = (data[i] - 0x80) as usize;
                    items.push(data[i + 1..i + 1 + l].to_vec());
                    i += 1 + l;
                }
                0xb8..=0xbf => {
                    let n = (data[i] - 0xb7) as usize;
                    let mut l = 0usize;
                    for &b in &data[i + 1..i + 1 + n] {
                        l = (l << 8) | b as usize;
                    }
                    let s = i + 1 + n;
                    items.push(data[s..s + l].to_vec());
                    i = s + l;
                }
                // Nested list (e.g. the EIP-1559 access list). Keep its
                // encoded form as the item; tests that inspect later
                // positions only need the item boundaries.
                0xc0..=0xf7 => {
                    let l = (data[i] - 0xc0) as usize;
                    items.push(data[i..i + 1 + l].to_vec());
                    i += 1 + l;
                }
                other => panic!("unexpected nested list / bad RLP at {i}: 0x{other:02x}"),
            }
        }
        items
    }

    /// Interpret an RLP item's content as a big-endian unsigned integer.
    fn rlp_item_u64(item: &[u8]) -> u64 {
        item.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
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
///
/// The wire shape is camelCase and every quantity is a `0x` hex string; the
/// serde helpers mirror that so a real node response deserialises. (The old
/// derive expected snake_case keys and JSON numbers, which no node sends.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    /// Transaction hash.
    pub transaction_hash: Hash,
    /// Transaction index in block.
    #[serde(with = "crate::web3::blockchain::serde_helpers::hex_u64")]
    pub transaction_index: u64,
    /// Block hash.
    pub block_hash: Hash,
    /// Block number.
    #[serde(with = "crate::web3::blockchain::serde_helpers::hex_u64")]
    pub block_number: u64,
    /// Sender address.
    pub from: Address,
    /// Recipient address.
    pub to: Option<Address>,
    /// Gas used by this transaction.
    #[serde(with = "crate::web3::blockchain::serde_helpers::hex_u64")]
    pub gas_used: u64,
    /// Cumulative gas used in the block up to and including this tx.
    #[serde(with = "crate::web3::blockchain::serde_helpers::hex_u64")]
    pub cumulative_gas_used: u64,
    /// Contract address (for contract-creation transactions).
    pub contract_address: Option<Address>,
    /// Logs emitted by this transaction.
    pub logs: Vec<crate::web3::blockchain::events::EventLog>,
    /// Status: 1 = success, 0 = failure (EIP-658).
    #[serde(with = "crate::web3::blockchain::serde_helpers::hex_u8")]
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
