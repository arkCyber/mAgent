//! HD Wallet Manager
//!
//! Provides HD wallet functionality for Ethereum.

#![cfg(feature = "wallet")]

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::web3::wallet::error::{WalletError, WalletResult};
use crate::web3::wallet::keystore::{Keystore, KeystoreError};
use crate::web3::wallet::Mnemonic;
use crate::web3::wallet::MnemonicType;
use crate::web3::blockchain::{Address, EthereumSignature, Secp256k1Keypair, TransactionSigner};

/// A single component of a BIP-32/BIP-44 derivation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivationIndex {
    /// The index value (e.g. `44`, `60`, `0`).
    pub index: u32,
    /// Whether this component is hardened (`true`) or not (`false`).
    pub hardened: bool,
}

impl DerivationIndex {
    /// A non-hardened (normal) child index.
    pub fn normal(index: u32) -> Self {
        Self { index, hardened: false }
    }

    /// A hardened child index.
    pub fn hardened(index: u32) -> Self {
        Self { index, hardened: true }
    }

    /// The 32-bit BIP-32 serialisation: sets bit 31 for hardened indexes.
    pub fn to_bip32(&self) -> u32 {
        if self.hardened {
            self.index | 0x8000_0000
        } else {
            self.index
        }
    }
}

impl core::fmt::Display for DerivationIndex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.hardened {
            write!(f, "{}'", self.index)
        } else {
            write!(f, "{}", self.index)
        }
    }
}

/// BIP-44 derivation path (`m/purpose'/coin_type'/account'/change/index`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationPath {
    /// Purpose (hardened `44` for BIP-44).
    pub purpose: DerivationIndex,
    /// Coin type (hardened `60` for Ethereum).
    pub coin_type: DerivationIndex,
    /// Account number (hardened).
    pub account: DerivationIndex,
    /// Change chain (non-hardened; `0` = external/receiving).
    pub change: DerivationIndex,
    /// Address index (non-hardened).
    pub index: DerivationIndex,
}

impl DerivationPath {
    /// The standard Ethereum path `m/44'/60'/0'/0/0`.
    pub fn ethereum_default() -> Self {
        Self {
            purpose: DerivationIndex::hardened(44),
            coin_type: DerivationIndex::hardened(60),
            account: DerivationIndex::hardened(0),
            change: DerivationIndex::normal(0),
            index: DerivationIndex::normal(0),
        }
    }
}

impl Default for DerivationPath {
    fn default() -> Self {
        Self::ethereum_default()
    }
}

/// HMAC-SHA512 (used by both BIP-39 seed and BIP-32 key derivation).
fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    use hmac::{Hmac, Mac};
    use sha2::Sha512;

    type HmacSha512 = Hmac<Sha512>;
    let mut mac = HmacSha512::new_from_slice(key).expect("HMAC-SHA512 accepts keys of any length");
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut result = [0u8; 64];
    result.copy_from_slice(&out);
    result
}

/// BIP-32 child-key derivation (private → private).
///
/// Starts from `seed` (the BIP-39 64-byte seed) and walks the path
/// `m/purpose'/coin_type'/account'/change/index`, returning the final
/// 32-byte secp256k1 private key. Both hardened and normal children are
/// supported (the only difference is the data hashed in HMAC-SHA512 —
/// a hardened child serialises `0x00 || k_par`, a normal child the
/// compressed parent public key).
pub fn derive_private_key(
    seed: &[u8; 64],
    path: &DerivationPath,
) -> Result<[u8; 32], WalletError> {
    use secp256k1::{PublicKey, Scalar, Secp256k1, SecretKey};

    let secp = Secp256k1::new();

    // Master key + chain code.
    let i = hmac_sha512(b"Bitcoin seed", seed);
    let mut k = SecretKey::from_slice(&i[..32])
        .map_err(|e| WalletError::DerivationFailed(format!("invalid master secret: {e}")))?;
    let mut c: [u8; 32] = i[32..]
        .try_into()
        .map_err(|_| WalletError::DerivationFailed("master chain code slice".into()))?;

    let steps = [
        path.purpose.to_bip32(),
        path.coin_type.to_bip32(),
        path.account.to_bip32(),
        path.change.to_bip32(),
        path.index.to_bip32(),
    ];

    for idx in steps {
        let hardened = idx & 0x8000_0000 != 0;

        // CKDpriv: I = HMAC-SHA512(c_par, data); data is
        //   0x00 || ser256(k_par) || ser32(i)            (hardened)
        //   serP(point(k_par)) || ser32(i)               (normal)
        let mut data: Vec<u8> = Vec::with_capacity(37);
        if hardened {
            data.push(0);
            data.extend_from_slice(&k.secret_bytes());
        } else {
            let pk = PublicKey::from_secret_key(&secp, &k);
            data.extend_from_slice(&pk.serialize());
        }
        data.extend_from_slice(&idx.to_be_bytes());

        let i = hmac_sha512(&c, &data);
        let il = Scalar::from_be_bytes(
            i[..32]
                .try_into()
                .map_err(|_| WalletError::DerivationFailed("child IL slice".into()))?,
        )
        .map_err(|_| WalletError::DerivationFailed("child IL out of range".into()))?;

        // k_child = (IL + k_par) mod n
        k = k
            .add_tweak(&il)
            .map_err(|e| WalletError::DerivationFailed(format!("child key tweak: {e}")))?;
        c = i[32..]
            .try_into()
            .map_err(|_| WalletError::DerivationFailed("child chain code slice".into()))?;
    }

    Ok(k.secret_bytes())
}

/// Derive the Ethereum address for `seed` at `path`.
pub fn derive_address(seed: &[u8; 64], path: &DerivationPath) -> Result<Address, WalletError> {
    let sk = derive_private_key(seed, path)?;
    let kp = crate::web3::blockchain::Secp256k1Keypair::from_secret_key(sk)
        .map_err(|e| WalletError::DerivationFailed(format!("{e:?}")))?;
    Ok(kp.public_key().to_address())
}

/// Sign a 32-byte transaction hash with the key derived from `phrase` /
/// `passphrase` at `path`, returning a 65-byte Ethereum signature.
///
/// The private key is derived on the fly and never stored; callers keep
/// the mnemonic + passphrase themselves (or persist a [`Keystore`]).
pub fn sign_transaction_hash(
    phrase: &str,
    passphrase: &str,
    path: &DerivationPath,
    tx_hash: &[u8; 32],
) -> Result<EthereumSignature, WalletError> {
    let mnemonic = Mnemonic::from_phrase(phrase)
        .map_err(|e| WalletError::InvalidMnemonic(format!("{e:?}")))?;
    let seed = mnemonic.to_seed(passphrase);
    let sk = derive_private_key(&seed, path)?;
    let kp = Secp256k1Keypair::from_secret_key(sk)
        .map_err(|e| WalletError::CryptoError(format!("{e:?}")))?;
    TransactionSigner::sign_transaction_hash(kp.secret_key(), tx_hash)
        .map_err(|e| WalletError::CryptoError(format!("{e:?}")))
}

/// A derived wallet
#[derive(Debug, Clone)]
pub struct Wallet {
    name: String,
    derivation_path: DerivationPath,
    address: Address,
}

impl Wallet {
    /// The raw derived Ethereum address.
    pub fn address(&self) -> &Address {
        &self.address
    }

    /// The checksummed hex address (EIP-55), e.g. `0x9858EfF…`.
    pub fn address_hex(&self) -> String {
        self.address.to_checksum()
    }

    /// The wallet's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The derivation path this wallet was derived at.
    pub fn path(&self) -> &DerivationPath {
        &self.derivation_path
    }
}

/// Wallet Manager
#[derive(Debug, Clone)]
pub struct WalletManager {
    wallets: Vec<Wallet>,
    active_wallet: Option<usize>,
    /// Encrypted keystores keyed by wallet name. In-memory mirror of what
    /// `esp32_nvs` persists to flash (the host-side, fully-testable path).
    keystores: BTreeMap<String, Keystore>,
}

impl WalletManager {
    /// Create an empty wallet manager with no wallets.
    pub fn new() -> Self {
        Self {
            wallets: Vec::new(),
            active_wallet: None,
            keystores: BTreeMap::new(),
        }
    }

    /// Create a new wallet from freshly generated secure entropy and store
    /// its private key encrypted (Argon2id + AES-256-GCM) under
    /// `passphrase`. Returns the derived wallet; the mnemonic is *not*
    /// returned — use [`WalletManager::create_wallet_phrase`] if you need
    /// to back it up.
    pub fn create_wallet(&mut self, name: &str, passphrase: &str, path: &DerivationPath) -> WalletResult<Wallet> {
        // Generate a fresh 12-word mnemonic from secure entropy and persist
        // the derived key encrypted under `passphrase`, so the created
        // wallet is both unique and recoverable from the encrypted keystore.
        let mnemonic = Mnemonic::generate(MnemonicType::Words12)?;
        self.finish_create(name, passphrase, path, &mnemonic)
    }

    /// Create a wallet from freshly generated entropy and return the
    /// mnemonic alongside the wallet so the caller can back it up.
    ///
    /// The mnemonic is the only way to recover the wallet after the
    /// encrypted keystore is gone — persist it somewhere safe (paper,
    /// password manager) and never expose it over the wire.
    pub fn create_wallet_phrase(
        &mut self,
        name: &str,
        passphrase: &str,
        path: &DerivationPath,
        kind: MnemonicType,
    ) -> WalletResult<(Wallet, Mnemonic)> {
        let mnemonic = Mnemonic::generate(kind)?;
        let wallet = self.finish_create(name, passphrase, path, &mnemonic)?;
        Ok((wallet, mnemonic))
    }

    /// Shared creation path: derive, push, and persist an encrypted
    /// keystore for `mnemonic`.
    fn finish_create(
        &mut self,
        name: &str,
        passphrase: &str,
        path: &DerivationPath,
        mnemonic: &Mnemonic,
    ) -> WalletResult<Wallet> {
        let seed = mnemonic.to_seed(passphrase);
        let address = derive_address(&seed, path)?;

        let wallet = Wallet {
            name: name.to_string(),
            derivation_path: path.clone(),
            address,
        };

        let sk = derive_private_key(&seed, path)?;
        let ks = Keystore::encrypt_private_key(name, &sk, passphrase, Some(wallet.address_hex()))
            .map_err(|e| WalletError::KeystoreError(e.to_string()))?;
        self.keystores.insert(name.to_string(), ks);

        self.active_wallet = Some(self.wallets.len());
        self.wallets.push(wallet.clone());

        Ok(wallet)
    }

    /// Import a wallet from an existing mnemonic phrase (in-memory only;
    /// use [`WalletManager::store_encrypted`] to persist an encrypted
    /// keystore for it). The phrase must have a valid BIP-39 checksum.
    pub fn import_wallet(&mut self, name: &str, phrase: &str, passphrase: &str, path: &DerivationPath) -> WalletResult<Wallet> {
        let mnemonic = Mnemonic::from_phrase(phrase)
            .map_err(|e| WalletError::InvalidMnemonic(format!("{:?}", e)))?;

        let seed = mnemonic.to_seed(passphrase);
        let address = derive_address(&seed, path)?;

        let wallet = Wallet {
            name: name.to_string(),
            derivation_path: path.clone(),
            address,
        };

        self.active_wallet = Some(self.wallets.len());
        self.wallets.push(wallet.clone());

        Ok(wallet)
    }

    /// Import a wallet from `phrase` and store its private key **encrypted**
    /// under `passphrase` (Argon2id + AES-256-GCM). Only the ciphertext is
    /// retained — never the plaintext key.
    ///
    /// This is the host-side analogue of the firmware's `esp32_nvs`
    /// `store_wallet`, and is fully testable. Returns the derived wallet.
    pub fn store_encrypted(
        &mut self,
        name: &str,
        phrase: &str,
        passphrase: &str,
        path: &DerivationPath,
    ) -> WalletResult<Wallet> {
        let wallet = self.import_wallet(name, phrase, passphrase, path)?;

        let mnemonic = Mnemonic::from_phrase(phrase)
            .map_err(|e| WalletError::InvalidMnemonic(format!("{e:?}")))?;
        let seed = mnemonic.to_seed(passphrase);
        let sk = derive_private_key(&seed, path)?;

        let ks = Keystore::encrypt_private_key(name, &sk, passphrase, Some(wallet.address_hex()))
            .map_err(|e| WalletError::KeystoreError(e.to_string()))?;
        self.keystores.insert(name.to_string(), ks);

        Ok(wallet)
    }

    /// Unlock a stored wallet's private key using `passphrase`. Returns an
    /// error if the wallet is unknown or the passphrase is wrong — never a
    /// corrupt/partial key.
    pub fn unlock_private_key(&self, name: &str, passphrase: &str) -> WalletResult<[u8; 32]> {
        let ks = self
            .keystores
            .get(name)
            .ok_or_else(|| WalletError::KeystoreError("no stored wallet with that name".into()))?;
        ks.decrypt_private_key(passphrase)
            .map_err(|e: KeystoreError| WalletError::KeystoreError(e.to_string()))
    }

    /// Whether a wallet with the given name has a stored (encrypted) keystore.
    pub fn has_stored(&self, name: &str) -> bool {
        self.keystores.contains_key(name)
    }

    /// The most recently created/imported wallet, if any.
    pub fn active_wallet(&self) -> Option<&Wallet> {
        self.active_wallet.and_then(|i| self.wallets.get(i))
    }

    /// All wallets created/imported by this manager, in creation order.
    pub fn wallets(&self) -> &[Wallet] {
        &self.wallets
    }
}

impl Default for WalletManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derivation_path_default() {
        let path = DerivationPath::ethereum_default();
        assert_eq!(path.purpose.index, 44);
        assert!(path.purpose.hardened);
    }

    #[test]
    fn test_wallet_manager() {
        let mut manager = WalletManager::new();
        let wallet = manager.create_wallet("test", "password", &DerivationPath::ethereum_default()).unwrap();
        assert_eq!(wallet.name(), "test");
    }

    #[test]
    fn create_derives_real_nonzero_address() {
        let mut manager = WalletManager::new();
        let wallet = manager
            .create_wallet("alice", "", &DerivationPath::ethereum_default())
            .unwrap();
        // A freshly created wallet must have a real, non-zero address and
        // an encrypted keystore persisted under its name.
        assert!(!wallet.address().is_zero());
        assert!(manager.has_stored("alice"));
        // The private key derived from the persisted keystore round-trips
        // and matches the wallet's own address.
        let sk = manager.unlock_private_key("alice", "").unwrap();
        let kp = crate::web3::blockchain::Secp256k1Keypair::from_secret_key(sk).unwrap();
        assert_eq!(kp.public_key().to_address().to_hex(), wallet.address().to_hex());
    }

    #[test]
    fn create_generates_unique_wallets() {
        let mut manager = WalletManager::new();
        let a = manager
            .create_wallet("a", "", &DerivationPath::ethereum_default())
            .unwrap();
        let b = manager
            .create_wallet("b", "", &DerivationPath::ethereum_default())
            .unwrap();
        // Random entropy means two creates must never collide.
        assert_ne!(a.address_hex(), b.address_hex());
        assert_ne!(a.name(), b.name());
    }

    #[test]
    fn create_wallet_phrase_returns_backup_mnemonic() {
        let mut manager = WalletManager::new();
        let (wallet, mnemonic) = manager
            .create_wallet_phrase("carol", "pw", &DerivationPath::ethereum_default(), MnemonicType::Words24)
            .unwrap();
        assert_eq!(mnemonic.word_count(), 24);
        assert!(mnemonic.has_valid_checksum());
        assert!(manager.has_stored("carol"));
        // The returned mnemonic re-imports to the same address.
        let sk = manager.unlock_private_key("carol", "pw").unwrap();
        let seed = mnemonic.to_seed("pw");
        assert_eq!(sk, derive_private_key(&seed, &DerivationPath::ethereum_default()).unwrap());
        assert!(!wallet.address().is_zero());
    }

    #[test]
    fn import_derives_address_from_phrase() {
        let mut manager = WalletManager::new();
        let wallet = manager
            .import_wallet(
                "imported",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "",
                &DerivationPath::ethereum_default(),
            )
            .unwrap();
        assert_eq!(
            wallet.address_hex(),
            "0x9858EfFD232B4033E47d90003D41EC34EcaEda94"
        );
    }

    #[test]
    fn import_different_derivation_paths_yield_different_address() {
        let mut manager = WalletManager::new();
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let path0 = DerivationPath::ethereum_default();
        let path1 = DerivationPath {
            index: DerivationIndex::normal(1),
            ..DerivationPath::ethereum_default()
        };
        let a = manager.import_wallet("a", phrase, "", &path0).unwrap();
        let b = manager.import_wallet("b", phrase, "", &path1).unwrap();
        assert_ne!(a.address_hex(), b.address_hex());
    }

    #[test]
    fn sign_transaction_recovers_address() {
        use crate::web3::blockchain::Secp256k1PublicKey;

        let path = DerivationPath::ethereum_default();
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let seed = Mnemonic::from_phrase(phrase).unwrap().to_seed("");
        let address = derive_address(&seed, &path).unwrap();

        let hash = [0x42u8; 32];
        let sig = sign_transaction_hash(phrase, "", &path, &hash).unwrap();
        assert_eq!(sig.as_bytes().len(), 65);

        // Recovering the public key from the signature must reproduce the
        // address the wallet derives from the same mnemonic+path.
        let recovered = Secp256k1PublicKey::recover_from(&hash, sig.as_bytes()).unwrap();
        assert_eq!(recovered.to_address().to_hex(), address.to_hex());
    }

    #[test]
    fn sign_differs_per_derivation_path() {
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let hash = [0x11u8; 32];
        let default = DerivationPath::ethereum_default();
        let other = DerivationPath {
            index: DerivationIndex::normal(1),
            ..DerivationPath::ethereum_default()
        };
        let s1 = sign_transaction_hash(phrase, "", &default, &hash).unwrap();
        let s2 = sign_transaction_hash(phrase, "", &other, &hash).unwrap();
        assert_ne!(s1.as_bytes(), s2.as_bytes());
    }

    #[test]
    fn derivation_is_deterministic() {
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let path = DerivationPath::ethereum_default();
        let seed = Mnemonic::from_phrase(phrase).unwrap().to_seed("");
        let sk1 = derive_private_key(&seed, &path).unwrap();
        let sk2 = derive_private_key(&seed, &path).unwrap();
        assert_eq!(sk1, sk2);
        let a1 = derive_address(&seed, &path).unwrap();
        let a2 = derive_address(&seed, &path).unwrap();
        assert_eq!(a1.to_hex(), a2.to_hex());
        // Different passphrase → different seed → different key.
        let seed_pp = Mnemonic::from_phrase(phrase).unwrap().to_seed("TREZOR");
        assert_ne!(derive_private_key(&seed_pp, &path).unwrap(), sk1);
    }

    #[test]
    fn import_rejects_invalid_checksum_phrase() {
        // 12×"abandon" has an invalid checksum — importing must fail with
        // an error, never panic or produce a bogus wallet.
        let mut manager = WalletManager::new();
        let res = manager.import_wallet(
            "bad",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon",
            "",
            &DerivationPath::ethereum_default(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn store_and_unlock_round_trips() {
        let mut m = WalletManager::new();
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let path = DerivationPath::ethereum_default();
        let wallet = m.store_encrypted("alice", phrase, "hunter2", &path).unwrap();
        assert!(m.has_stored("alice"));

        // Correct passphrase unlocks the exact derived key.
        let sk = m.unlock_private_key("alice", "hunter2").unwrap();
        let seed = Mnemonic::from_phrase(phrase).unwrap().to_seed("hunter2");
        assert_eq!(sk, derive_private_key(&seed, &path).unwrap());
        // ...and that key maps to the stored wallet's address.
        assert_eq!(
            derive_address(&seed, &path).unwrap().to_checksum(),
            wallet.address_hex()
        );

        // Wrong passphrase fails; unknown name fails.
        assert!(m.unlock_private_key("alice", "wrong").is_err());
        assert!(m.unlock_private_key("nobody", "hunter2").is_err());
    }

    #[test]
    fn stored_keystore_never_exposes_mnemonic() {
        let mut m = WalletManager::new();
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        m.store_encrypted("bob", phrase, "pw", &DerivationPath::ethereum_default())
            .unwrap();
        // The manager retains only the encrypted keystore — neither the
        // mnemonic phrase nor the passphrase may survive in Debug output
        // (only the AES-GCM ciphertext is kept).
        let dbg = format!("{:?}", m);
        assert!(!dbg.contains("abandon"));
        assert!(!dbg.contains("pw"));
    }
}
