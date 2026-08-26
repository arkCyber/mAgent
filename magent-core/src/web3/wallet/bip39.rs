//! BIP-39 Mnemonic Word List
//!
//! Provides the standard BIP-39 English wordlist and basic parsing.

#![cfg(feature = "wallet")]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// BIP-39 word list size
pub const WORD_LIST_SIZE: usize = 2048;

/// Maximum words in a mnemonic
pub const MAX_MNEMONIC_WORDS: usize = 24;

/// Entropy sizes supported by BIP-39 (in bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MnemonicType {
    /// 12-word mnemonic (128 bits entropy)
    Words12 = 128,
    /// 24-word mnemonic (256 bits entropy)
    Words24 = 256,
}

impl MnemonicType {
    /// Number of words in the mnemonic
    pub fn word_count(&self) -> usize {
        match self {
            MnemonicType::Words12 => 12,
            MnemonicType::Words24 => 24,
        }
    }

    /// Entropy size in bytes
    pub fn entropy_bytes(&self) -> usize {
        match self {
            MnemonicType::Words12 => 16,
            MnemonicType::Words24 => 32,
        }
    }
}

// The official BIP-39 English wordlist (all 2048 words). Kept in a
// separate module so the giant `const` array doesn't clutter the parser.
// Indexed 0..=2047 — each index is an 11-bit big-endian chunk of the
// mnemonic's `entropy ‖ checksum` bit string.
pub(crate) use super::english::WORDS as WORDS_2048;

/// Get the word at `index`. Returns `None` for indices outside
/// `0..WORD_LIST_SIZE` (i.e. `>= 2048`).
pub fn get_word(index: usize) -> Option<&'static str> {
    WORDS_2048.get(index).copied()
}

/// Find the word index for `word` (case-insensitive). Returns `None` if
/// `word` is not in the official 2048-word BIP-39 English list.
pub fn find_word(word: &str) -> Option<usize> {
    let word_lower = word.to_lowercase();
    for (i, &w) in WORDS_2048.iter().enumerate() {
        if w == word_lower {
            return Some(i);
        }
    }
    None
}

/// The BIP-39 English word list.
pub struct WordList;

impl WordList {
    /// Get the word at the given index (0-2047).
    pub fn get(index: usize) -> Option<&'static str> {
        get_word(index)
    }

    /// Find the index of a word in the list
    pub fn find_word(word: &str) -> Option<usize> {
        find_word(word)
    }

    /// Validate that a word exists in the wordlist
    pub fn is_valid(word: &str) -> bool {
        Self::find_word(word).is_some()
    }
}

/// A BIP-39 mnemonic phrase
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mnemonic {
    indices: [u16; MAX_MNEMONIC_WORDS],
    word_count: usize,
}

impl Mnemonic {
    /// Parse a mnemonic phrase from words
    pub fn from_phrase(phrase: &str) -> crate::web3::wallet::error::WalletResult<Self> {
        let words: Vec<&str> = phrase.split_whitespace().collect();
        let word_count = words.len();

        if word_count != 12 && word_count != 24 {
            return Err(crate::web3::wallet::error::WalletError::InvalidMnemonic(format!(
                "phrase must have 12 or 24 words, got {}",
                word_count
            )));
        }

        let mut indices = [0u16; MAX_MNEMONIC_WORDS];
        for (i, word) in words.iter().enumerate() {
            match find_word(word) {
                Some(idx) => indices[i] = idx as u16,
                None => {
                    return Err(crate::web3::wallet::error::WalletError::InvalidWord((*word).to_string()));
                }
            }
        }

        let mnemonic = Mnemonic { indices, word_count };
        // Aerospace-grade: never accept a phrase whose checksum is wrong —
        // a single typo / corrupted word would otherwise silently derive a
        // different (wrong) key.
        if !mnemonic.has_valid_checksum() {
            return Err(crate::web3::wallet::error::WalletError::InvalidChecksum);
        }

        Ok(mnemonic)
    }

    /// Validate the BIP-39 checksum: the last `cs` bits of the mnemonic
    /// must equal the first `cs` bits of SHA-256(entropy). Returns `false`
    /// for any internally-inconsistent phrase (typo, corruption, or a
    /// word that isn't the intended one).
    ///
    /// `cs` is 4 bits for 12-word and 8 bits for 24-word phrases. The
    /// word indices are 11-bit big-endian chunks whose concatentation is
    /// `entropy ‖ checksum`.
    pub fn has_valid_checksum(&self) -> bool {
        let cs: usize = match self.word_count {
            12 => 4,
            24 => 8,
            _ => return false,
        };
        let total_bits = self.word_count * 11;
        let entropy_bits = total_bits - cs;

        // Build the full bit string: each index is 11 bits, big-endian.
        let mut bits: Vec<u8> = Vec::with_capacity(total_bits);
        for i in 0..self.word_count {
            let v = self.indices[i];
            for b in (0..11).rev() {
                bits.push(((v >> b) & 1) as u8);
            }
        }

        let entropy = self.entropy_bytes(entropy_bits / 8);

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&entropy);
        let hash = hasher.finalize();

        for k in 0..cs {
            let expected = (hash[k / 8] >> (7 - (k % 8))) & 1;
            let actual = bits[entropy_bits + k];
            if expected != actual {
                return false;
            }
        }
        true
    }

    /// The raw entropy (the checksum-less prefix) of this mnemonic, as
    /// bytes. For a 12-word mnemonic this is 16 bytes (128 bits); for a
    /// 24-word mnemonic 32 bytes (256 bits).
    pub fn entropy(&self) -> Vec<u8> {
        let cs: usize = match self.word_count {
            12 => 4,
            24 => 8,
            _ => 0,
        };
        let total_bits = self.word_count * 11;
        let entropy_bits = total_bits - cs;
        self.entropy_bytes(entropy_bits / 8)
    }

    /// Recover the entropy bytes from the stored indices.
    ///
    /// `entropy_bytes_len` must equal `(word_count * 11 - cs) / 8` for the
    /// supported word counts; the word indices are 11-bit big-endian chunks
    /// whose concatenation is `entropy ‖ checksum`.
    fn entropy_bytes(&self, entropy_bytes_len: usize) -> Vec<u8> {
        let mut entropy = alloc::vec![0u8; entropy_bytes_len];
        for (i, byte) in entropy.iter_mut().enumerate() {
            let bit_start = i * 8;
            let mut acc: u32 = 0;
            for k in 0..8 {
                // 11-bit chunk: bit position within the whole bit string.
                let abs = bit_start + k;
                let chunk = self.indices[abs / 11] as u32;
                let bit = (chunk >> (10 - (abs % 11))) & 1;
                acc = (acc << 1) | bit;
            }
            *byte = acc as u8;
        }
        entropy
    }

    /// Build a mnemonic from raw entropy. `entropy` must be exactly
    /// 16 bytes (12 words) or 32 bytes (24 words); the BIP-39 checksum is
    /// computed and appended automatically.
    ///
    /// This is the inverse of [`Self::entropy`]: for any mnemonic `m`,
    /// `Mnemonic::from_entropy(&m.entropy()).unwrap() == m`.
    pub fn from_entropy(entropy: &[u8]) -> crate::web3::wallet::error::WalletResult<Self> {
        let bit_len = entropy.len() * 8;
        let (word_count, cs): (usize, usize) = match bit_len {
            128 => (12, 4),
            256 => (24, 8),
            n => {
                return Err(crate::web3::wallet::error::WalletError::InvalidEntropyLength(n / 8));
            }
        };
        let total_bits = bit_len + cs;

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(entropy);
        let hash = hasher.finalize();

        // All bits: entropy ‖ first `cs` bits of SHA-256(entropy).
        let mut bits: Vec<bool> = Vec::with_capacity(total_bits);
        for &byte in entropy {
            for b in (0..8).rev() {
                bits.push((byte >> b) & 1 == 1);
            }
        }
        for k in 0..cs {
            let expected = (hash[k / 8] >> (7 - (k % 8))) & 1 == 1;
            bits.push(expected);
        }

        // Pack every 11 bits into a word index (big-endian).
        let mut indices = [0u16; MAX_MNEMONIC_WORDS];
        for w in 0..word_count {
            let mut idx: u32 = 0;
            for k in 0..11 {
                idx = (idx << 1) | (bits[w * 11 + k] as u32);
            }
            indices[w] = idx as u16;
        }

        let mnemonic = Mnemonic { indices, word_count };
        debug_assert!(mnemonic.has_valid_checksum());
        Ok(mnemonic)
    }

    /// Generate a fresh mnemonic with cryptographically secure random
    /// entropy. `kind` selects a 12-word (128-bit) or 24-word (256-bit)
    /// phrase. Two calls produce (astronomically likely) different phrases.
    ///
    /// # Errors
    /// Returns [`WalletError::CryptoError`] if the OS RNG is unavailable.
    pub fn generate(kind: MnemonicType) -> crate::web3::wallet::error::WalletResult<Self> {
        use rand_core::{OsRng, RngCore};
        let mut entropy = alloc::vec![0u8; kind.entropy_bytes()];
        OsRng.fill_bytes(&mut entropy);
        Self::from_entropy(&entropy)
    }


    /// Convert mnemonic to phrase string
    pub fn to_phrase(&self) -> String {
        let mut words = String::with_capacity(self.word_count * 8);
        for i in 0..self.word_count {
            if i > 0 {
                words.push(' ');
            }
            if let Some(w) = get_word(self.indices[i] as usize) {
                words.push_str(w);
            }
        }
        words
    }

    /// Get the word count
    pub fn word_count(&self) -> usize {
        self.word_count
    }

    /// Get word indices
    pub fn indices(&self) -> &[u16] {
        &self.indices[..self.word_count]
    }

    /// Derive seed from mnemonic using BIP-39 PBKDF2-HMAC-SHA512.
    ///
    /// This is the standard BIP-39 seed derivation: 2048 iterations of
    /// PBKDF2 with HMAC-SHA512 as the PRF, password = the mnemonic
    /// phrase, salt = `"mnemonic" || passphrase`. The result is
    /// interoperable with every standard wallet.
    ///
    /// NOTE: this is a hand-rolled PBKDF2 (the crate doesn't depend on
    /// the `pbkdf2` package). It is kept minimal and correct for the
    /// single-Block output case (`dkLen == hLen == 64`), which is the
    /// only configuration BIP-39 needs. The HMAC is computed with the
    /// audited `hmac` crate, not a naive concatenation, so the output
    /// matches standard wallets (see the `seed_matches_bip39_vector`
    /// test).
    pub fn to_seed(&self, passphrase: &str) -> [u8; 64] {
        let salt = format!("mnemonic{}", passphrase);
        let key = self.to_phrase();
        pbkdf2_hmac_sha512(key.as_bytes(), salt.as_bytes(), 2048)
    }
}

/// HMAC-SHA512 over `data` with key `key`.
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

/// PBKDF2 (RFC 2898) with HMAC-SHA512, producing a 64-byte derived key.
///
/// BIP-39 requests `dkLen == 64 == hLen`, so the output is a single
/// block: `T = U_1 XOR U_2 XOR ... XOR U_c`, where `U_1 = PRF(P, S ||
/// INT_MSB(1))` and `U_i = PRF(P, U_{i-1})`.
fn pbkdf2_hmac_sha512(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 64] {
    // INT_MSB(1) as a 4-byte big-endian block counter (the only block
    // we need since dkLen == hLen).
    let mut input = alloc::vec::Vec::with_capacity(salt.len() + 4);
    input.extend_from_slice(salt);
    input.extend_from_slice(&[0u8, 0, 0, 1]);

    let mut u = hmac_sha512(password, &input);
    let mut t = u;
    for _ in 1..iterations {
        u = hmac_sha512(password, &u);
        for i in 0..64 {
            t[i] ^= u[i];
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_phrase() {
        // Official BIP-39 test vector 1 (Trezor) — has a valid checksum.
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let m = Mnemonic::from_phrase(phrase).unwrap();
        assert_eq!(m.word_count(), 12);
        assert!(m.has_valid_checksum());
        // Phrase round-trips.
        assert_eq!(m.to_phrase(), phrase);
    }

    #[test]
    fn rejects_invalid_checksum() {
        // 12×"abandon" is NOT the valid vector (the real one ends in
        // "about"), so its checksum must be rejected — never silently
        // derive a wrong key from a corrupted phrase.
        let m = Mnemonic::from_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon",
        );
        assert!(matches!(m, Err(super::super::error::WalletError::InvalidChecksum)));
    }

    #[test]
    fn rejects_wrong_word_count() {
        assert!(Mnemonic::from_phrase("abandon ability able").is_err());
        assert!(Mnemonic::from_phrase("").is_err());
    }

    #[test]
    fn rejects_word_not_in_list() {
        // "zzzzz" is not a BIP-39 word.
        assert!(Mnemonic::from_phrase("zzzzz ability able about above absent absorb abstract absurd about above absent").is_err());
    }

    #[test]
    fn test_seed_derivation() {
        let phrase =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let m = Mnemonic::from_phrase(phrase).unwrap();
        let _seed = m.to_seed("");
    }

    #[test]
    fn seed_matches_bip39_vector() {
        // Official BIP-39 test vector 1 (Trezor): mnemonic = 12×"abandon"
        // then "about", passphrase = "".
        let m = Mnemonic::from_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let seed = m.to_seed("");
        let expected_hex =
            "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";
        let mut expected = [0u8; 64];
        for i in 0..64 {
            let hi = hex_nibble(expected_hex.as_bytes()[i * 2]);
            let lo = hex_nibble(expected_hex.as_bytes()[i * 2 + 1]);
            expected[i] = (hi << 4) | lo;
        }
        assert_eq!(seed, expected);
    }

    #[test]
    fn seed_changes_with_passphrase() {
        let m = Mnemonic::from_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        // Two different passphrases must produce two different seeds —
        // the salt is `"mnemonic" || passphrase`.
        assert_ne!(m.to_seed(""), m.to_seed("TREZOR"));
    }

    #[test]
    fn wordlist_is_full_and_unique() {
        // The official BIP-39 English list has exactly 2048 unique words.
        assert_eq!(WORD_LIST_SIZE, 2048);
        assert_eq!(WORDS_2048.len(), 2048);
        let mut sorted: Vec<&str> = WORDS_2048.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 2048, "duplicate words present");
    }

    #[test]
    fn get_word_covers_all_indices() {
        for i in 0..2048 {
            assert!(get_word(i).is_some(), "missing word at index {i}");
        }
        assert!(get_word(2048).is_none());
        assert!(get_word(usize::MAX).is_none());
        // Indices round-trip through the parser.
        assert_eq!(find_word(get_word(2047).unwrap()).unwrap(), 2047);
        assert_eq!(find_word("Zoo").unwrap(), 2047);
        assert_eq!(find_word("notaword"), None);
    }

    #[test]
    fn from_entropy_zero_vectors_match_official() {
        // BIP-39 official test vectors: all-zero entropy.
        let m12 = Mnemonic::from_entropy(&[0u8; 16]).unwrap();
        assert_eq!(
            m12.to_phrase(),
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        );
        let m24 = Mnemonic::from_entropy(&[0u8; 32]).unwrap();
        assert_eq!(
            m24.to_phrase(),
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"
        );
    }

    #[test]
    fn entropy_roundtrips_through_from_entropy() {
        // m = from_entropy(m.entropy()) is an identity for both lengths.
        for bytes in [16usize, 32] {
            let entropy: Vec<u8> = (0..bytes).map(|i| (i * 37 + 11) as u8).collect();
            let m = Mnemonic::from_entropy(&entropy).unwrap();
            assert_eq!(m.entropy(), entropy);
            let m2 = Mnemonic::from_entropy(&m.entropy()).unwrap();
            assert_eq!(m, m2);
            assert!(m.has_valid_checksum());
            // And parsing the phrase reproduces the same mnemonic.
            let m3 = Mnemonic::from_phrase(&m.to_phrase()).unwrap();
            assert_eq!(m, m3);
        }
    }

    #[test]
    fn from_entropy_rejects_bad_lengths() {
        use super::super::error::WalletError;
        for n in [0usize, 1, 15, 17, 31, 33, 64] {
            let entropy = alloc::vec![0u8; n];
            assert!(matches!(
                Mnemonic::from_entropy(&entropy),
                Err(WalletError::InvalidEntropyLength(_))
            ));
        }
    }

    #[test]
    fn generate_produces_valid_and_unique_mnemonics() {
        let a = Mnemonic::generate(MnemonicType::Words12).unwrap();
        let b = Mnemonic::generate(MnemonicType::Words12).unwrap();
        let c = Mnemonic::generate(MnemonicType::Words24).unwrap();
        assert_eq!(a.word_count(), 12);
        assert!(a.has_valid_checksum());
        assert_eq!(c.word_count(), 24);
        assert!(c.has_valid_checksum());
        assert_ne!(a.to_phrase(), b.to_phrase());
        assert_ne!(a.to_phrase(), c.to_phrase());
        // Generated phrases always parse back.
        assert!(Mnemonic::from_phrase(&a.to_phrase()).is_ok());
        assert!(Mnemonic::from_phrase(&c.to_phrase()).is_ok());
    }
}

#[cfg(test)]
fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}
