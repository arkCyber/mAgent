//! DBO2: stronger successor to `wifi_pass_seal` (DBO1).
//!
//! # Why a successor?
//!
//! DBO1 (`wifi_pass_seal`) is an XOR stream:
//!
//! ```text
//!   plain[i] ^ device_key[i % 32] ^ nonce[i % 12]
//! ```
//!
//! That gives you confidentiality against a flash-dump attacker,
//! but it has known weaknesses worth fixing in v2:
//!
//!   1. **Re-using the same device_key for every entry** means
//!      a known-plaintext attack on one entry gives the attacker
//!      the cipher-stream for every other entry on the same
//!      device (just subtract the plaintext to recover the key
//!      stream). DBO1 partially mitigates by mixing in a fresh
//!      nonce per write, but the device_key portion is constant.
//!   2. **No integrity**: a flipped cipher bit flips exactly one
//!      plaintext bit on open, which can be weaponised for
//!      targeted edits (e.g. flipping one bit of a known-plain
//!      password hash).
//!   3. **No algorithm agility** beyond the wire-format prefix.
//!
//! DBO2 fixes all three:
//!
//!   * **Key stretching**: `cipher_key = HKDF-SHA256(device_key,
//!     salt=nonce, info="dbo2/cipher/v1", L=32)`. Each entry
//!     uses a *per-entry* cipher key derived from the device key
//!     and the nonce. Two entries with the same plaintext but
//!     different nonces get completely independent cipher
//!     streams.
//!   * **Integrity**: `mac = HMAC-SHA256(mac_key, nonce || cipher)`
//!     with `mac_key` derived from a *separate* HKDF output
//!     (`info="dbo2/mac/v1"`). A flipped cipher bit fails the
//!     MAC check on open — there is no silent corruption.
//!   * **Version prefix**: `"DBO2:" || hex(nonce) || hex(cipher)
//!     || hex(mac)` so the dispatcher can dispatch by prefix.
//!
//! # Algorithm IDs (for the AT audit surface)
//!
//! - Cipher:    HKDF-SHA256 → XOR-stream (same construction as
//!              DBO1 but with per-entry cipher key)
//! - MAC:       HMAC-SHA256 truncated to 16 bytes
//! - Stretch:   HKDF info strings `"dbo2/cipher/v1"`, `"dbo2/mac/v1"`
//!
//! # Threat model (carried over from DBO1)
//!
//! | Attacker capability                                       | DBO2 security      |
//! |-----------------------------------------------------------|---------------------|
//! | Dumps NVS only                                            | ✅ Protected       |
//! | Dumps NVS + edits a single byte                           | ✅ MAC rejected    |
//! | Dumps NVS + reads eFuse (physical access)                | ⚠️ Compromised    |
//! | Dumps NVS + reads eFuse + knows algorithm version         | ❌ Defeated       |
//!
//! The "physical access + eFuse read" row is unchanged from
//! DBO1 — protecting against that requires secure-element
//! hardware, which DBO2 still doesn't claim to do.
//!
//! # Migration from DBO1
//!
//! [`open_sealed_v2`] transparently falls back to DBO1 via
//! [`wifi_pass_seal::open_sealed_bytes`] when the stored blob
//! has the DBO1 prefix. Migration happens lazily:
//!
//!   * **Read path**: any entry (DBO1 or DBO2) opens
//!     successfully on the first attempt.
//!   * **Write path**: every new `seal_v2` call writes a DBO2
//!     blob, so a single `AT+CWJAP=` SET after the upgrade
//!     migrates that entry forward.
//!   * **No data loss**: legacy plaintext (no prefix at all)
//!     still opens via DBO1's `LegacyPlaintext` branch, so
//!     pre-DBO1 devices can upgrade in place.
//!
//! # Host testability
//!
//! All functions are pure (deterministic, no FFI, no I/O) and
//! heapless. `cargo test -p magent-core --features web3 --
//! lib wifi_pass_seal_v2` exercises the full algorithm on the
//! host.
//!
//! # Feature gate
//!
//! The HKDF/HMAC primitives need `sha3`. The module is therefore
//! gated on the `web3` feature; calls into it from firmware are
//! compile-time conditional on that feature being enabled
//! (`firmware/esp32-app` does enable it).

#![cfg(feature = "web3")]

use heapless::{String as HeaplessString, Vec};

use digest::{KeyInit, Mac};
use hmac::Hmac;
use sha2::Sha256;

use crate::wifi_pass_seal;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Algorithm version tag, embedded in the wire format.
// The production prefix constant is `DBO2_PREFIX` ("DBO2:"). This
// bare tag is kept for version-introspection/tests; `allow(dead_code)`
// because no hot path formats with it directly.
#[allow(dead_code)]
const VERSION_TAG: &str = "DBO2";

/// Domain-separation strings for HKDF info. Bumping the suffix
/// (e.g. `…/v1` → `…/v2`) causes old sealed blobs to fail MAC
/// checks under the new key schedule — that's the entire
/// purpose of the version suffix.
const HKDF_INFO_CIPHER: &[u8] = b"magent/dbo2/cipher/v1";
const HKDF_INFO_MAC: &[u8] = b"magent/dbo2/mac/v1";

/// Length (bytes) of the truncated HMAC tag.
pub const MAC_LEN: usize = 16;

/// Length (bytes) of the stretched cipher key. Same as DBO1's
/// device-key length for compatibility with the underlying XOR
/// construction.
pub const CIPHER_KEY_LEN: usize = 32;

/// Length of the HMAC key, derived from HKDF.
pub const MAC_KEY_LEN: usize = 32;

/// Maximum plaintext length (same as DBO1).
pub use wifi_pass_seal::MAX_PLAINTEXT;

/// Length (bytes) of the random nonce prefix. Matches DBO1 so a
/// single nonce generator can serve both algorithms.
pub use wifi_pass_seal::NONCE_LEN;

/// Maximum encoded length (DBO2: prefix(5) + 2*NONCE_LEN +
/// 2*MAX_PLAINTEXT + 2*MAC_LEN).
pub const MAX_ENCODED_LEN: usize = 5 + 2 * (NONCE_LEN + MAX_PLAINTEXT + MAC_LEN);

/// Wire-format prefix (incl. trailing colon).
pub const DBO2_PREFIX: &str = "DBO2:";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur while sealing / opening a DBO2-protected secret.
#[derive(Debug, PartialEq, Eq)]
pub enum SealError {
    /// Plaintext exceeded `MAX_PLAINTEXT`.
    PlaintextTooLong,
    /// Device key was empty.
    EmptyKey,
    /// Nonce was empty.
    EmptyNonce,
    /// Stored value's hex decode failed.
    BadHex,
    /// Stored value had wrong length for its declared prefix.
    BadLength,
    /// Stored value's MAC did not match (tampered or wrong key).
    BadMac,
    /// Stored value is missing the version prefix.
    BadPrefix,
    /// Output buffer capacity would be exceeded.
    OutputFull,
}

impl Clone for SealError {
    fn clone(&self) -> Self {
        match self {
            SealError::PlaintextTooLong => SealError::PlaintextTooLong,
            SealError::EmptyKey => SealError::EmptyKey,
            SealError::EmptyNonce => SealError::EmptyNonce,
            SealError::BadHex => SealError::BadHex,
            SealError::BadLength => SealError::BadLength,
            SealError::BadMac => SealError::BadMac,
            SealError::BadPrefix => SealError::BadPrefix,
            SealError::OutputFull => SealError::OutputFull,
        }
    }
}

impl core::fmt::Display for SealError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            SealError::PlaintextTooLong => "plaintext too long",
            SealError::EmptyKey => "empty device key",
            SealError::EmptyNonce => "empty nonce",
            SealError::BadHex => "bad hex encoding",
            SealError::BadLength => "stored value length mismatch",
            SealError::BadMac => "mac mismatch (tampered or wrong key)",
            SealError::BadPrefix => "missing version prefix",
            SealError::OutputFull => "output buffer full",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// HKDF-SHA256 (RFC 5869)
// ---------------------------------------------------------------------------
//
// We implement the "single-shot" HKDF-Extract+Expand pattern by
// hand rather than pulling in the `hkdf` crate — the algorithm is
// short (a few lines) and avoiding a new dep is the right call
// for the embadded target. The implementation follows RFC 5869
// verbatim:
//
//   PRK  = HMAC-SHA256(salt, IKM)
//   OKM  = HMAC-SHA256(PRK, info || 0x01)  (L <= 32 bytes, no T(2))
//
// We support exactly two outputs (cipher_key, mac_key), each at
// most 32 bytes, so the single-block case is always sufficient.

/// HKDF-Extract: returns a 32-byte PRK.
fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(salt).expect("HMAC accepts any key length");
    mac.update(ikm);
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&tag);
    out
}

/// HKDF-Expand: returns up to 32 bytes of OKM for the given
/// info. For L > 32 the caller must call once with 0x01 and
/// once with 0x02 — we don't need that here so the counter is
/// hard-coded to 0x01.
fn hkdf_expand(prk: &[u8; 32], info: &[u8], out: &mut [u8]) {
    debug_assert!(out.len() <= 32);
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(prk).expect("HMAC accepts any key length");
    mac.update(info);
    mac.update(&[0x01]);
    let tag = mac.finalize().into_bytes();
    out.copy_from_slice(&tag[..out.len()]);
}

// ---------------------------------------------------------------------------
// Hex helpers (no_std, no alloc)
// ---------------------------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_encode_byte(out: &mut [u8], byte: u8) -> Result<(), SealError> {
    if out.len() < 2 {
        return Err(SealError::OutputFull);
    }
    out[0] = HEX[(byte >> 4) as usize];
    out[1] = HEX[(byte & 0xf) as usize];
    Ok(())
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_decode_byte(hi: u8, lo: u8) -> Option<u8> {
    Some((hex_nibble(hi)? << 4) | hex_nibble(lo)?)
}

// ---------------------------------------------------------------------------
// MAC computation
// ---------------------------------------------------------------------------

fn mac_bytes(mac_key: &[u8], nonce: &[u8], cipher: &[u8]) -> [u8; MAC_LEN] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(mac_key).expect("HMAC accepts any key length");
    mac.update(nonce);
    mac.update(cipher);
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; MAC_LEN];
    out.copy_from_slice(&tag[..MAC_LEN]);
    out
}

// ---------------------------------------------------------------------------
// Seal / Open API
// ---------------------------------------------------------------------------

/// Seal `plain` with the device-bound key + a fresh nonce and
/// write the DBO2 wire format into `out`.
///
/// Returns `Ok(())` on success. The output buffer must have
/// capacity at least [`MAX_ENCODED_LEN`].
pub fn seal_str(
    plain: &str,
    device_key: &[u8],
    nonce: &[u8],
    out: &mut HeaplessString<MAX_ENCODED_LEN>,
) -> Result<(), SealError> {
    if device_key.is_empty() {
        return Err(SealError::EmptyKey);
    }
    if nonce.is_empty() {
        return Err(SealError::EmptyNonce);
    }
    if plain.len() > MAX_PLAINTEXT {
        return Err(SealError::PlaintextTooLong);
    }

    // 1. Stretch the device key with HKDF to derive per-entry
    //    cipher and MAC keys.
    let prk = hkdf_extract(nonce, device_key);
    let mut cipher_key = [0u8; CIPHER_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_CIPHER, &mut cipher_key);
    let mut mac_key = [0u8; MAC_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_MAC, &mut mac_key);

    // 2. XOR-stream into a bounded cipher buffer.
    let mut cipher: Vec<u8, MAX_PLAINTEXT> = Vec::new();
    for (i, &b) in plain.as_bytes().iter().enumerate() {
        let kb = cipher_key[i % cipher_key.len()];
        let _ = cipher.push(b ^ kb);
    }

    // 3. Compute the MAC over (nonce || cipher).
    let mac = mac_bytes(&mac_key, nonce, &cipher);

    // 4. Write the wire format: "DBO2:" + hex(nonce) + hex(cipher) + hex(mac)
    //    Use a small intermediate buffer for the hex payload,
    //    then push it into the heapless::String.
    out.clear();
    out.push_str(DBO2_PREFIX)
        .map_err(|_| SealError::OutputFull)?;
    for &b in nonce.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        let s = core::str::from_utf8(&buf).expect("hex is ASCII");
        out.push_str(s).map_err(|_| SealError::OutputFull)?;
    }
    for &b in cipher.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        let s = core::str::from_utf8(&buf).expect("hex is ASCII");
        out.push_str(s).map_err(|_| SealError::OutputFull)?;
    }
    for &b in mac.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        let s = core::str::from_utf8(&buf).expect("hex is ASCII");
        out.push_str(s).map_err(|_| SealError::OutputFull)?;
    }
    Ok(())
}

/// Outcome of `open_sealed_v2`. Distinguishes the three
/// "successful" cases (DBO2 opened, DBO1 opened, raw plaintext)
/// so the caller can decide whether to migrate the entry
/// forward to DBO2.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenOutcome<'a> {
    /// DBO2 entry opened successfully.
    Dbo2Decoded,
    /// DBO1 entry opened successfully (legacy; consider migrating).
    Dbo1Decoded,
    /// Plaintext with no prefix (pre-DBO1 era).
    LegacyPlaintext(&'a str),
}

/// Open a stored NVS value. Tries DBO2 first, then falls back to
/// DBO1 via [`wifi_pass_seal::open_sealed_bytes`]. The caller
/// receives a typed [`OpenOutcome`] so it can decide whether to
/// migrate the entry forward.
pub fn open_sealed_v2<'a>(
    stored: &'a str,
    device_key: &[u8],
    out: &mut Vec<u8, MAX_PLAINTEXT>,
) -> Result<OpenOutcome<'a>, SealError> {
    if let Some(payload) = stored.strip_prefix(DBO2_PREFIX) {
        // DBO2 path: payload = hex(nonce) + hex(cipher) + hex(mac)
        let expected = 2 * (NONCE_LEN + MAX_PLAINTEXT + MAC_LEN);
        if payload.len() != expected {
            // Could be shorter (plaintext was < MAX_PLAINTEXT)
            // or could be invalid. Validate the trailing MAC
            // portion length.
            if payload.len() < 2 * (NONCE_LEN + MAC_LEN)
                || !(payload.len() - 2 * MAC_LEN).is_multiple_of(2)
            {
                return Err(SealError::BadLength);
            }
        }

        // Decode nonce (always NONCE_LEN hex chars).
        let nonce_hex = &payload.as_bytes()[..2 * NONCE_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        for i in 0..NONCE_LEN {
            nonce[i] =
                hex_decode_byte(nonce_hex[i * 2], nonce_hex[i * 2 + 1]).ok_or(SealError::BadHex)?;
        }

        // Decode MAC (last MAC_LEN hex chars).
        let mac_hex = &payload.as_bytes()[payload.len() - 2 * MAC_LEN..];
        let mut stored_mac = [0u8; MAC_LEN];
        for i in 0..MAC_LEN {
            stored_mac[i] =
                hex_decode_byte(mac_hex[i * 2], mac_hex[i * 2 + 1]).ok_or(SealError::BadHex)?;
        }

        // Decode cipher (middle portion).
        let cipher_hex = &payload.as_bytes()[2 * NONCE_LEN..payload.len() - 2 * MAC_LEN];
        if cipher_hex.len() % 2 != 0 {
            return Err(SealError::BadHex);
        }
        let cipher_len = cipher_hex.len() / 2;
        if cipher_len > MAX_PLAINTEXT {
            return Err(SealError::PlaintextTooLong);
        }
        let mut cipher: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        for i in 0..cipher_len {
            let b = hex_decode_byte(cipher_hex[i * 2], cipher_hex[i * 2 + 1])
                .ok_or(SealError::BadHex)?;
            cipher.push(b).map_err(|_| SealError::OutputFull)?;
        }

        // Re-derive keys and check MAC.
        let prk = hkdf_extract(&nonce, device_key);
        let mut mac_key = [0u8; MAC_KEY_LEN];
        hkdf_expand(&prk, HKDF_INFO_MAC, &mut mac_key);
        let computed = mac_bytes(&mac_key, &nonce, &cipher);
        // Constant-time compare to avoid leaking MAC bytes via
        // timing (the key is per-entry so the timing channel is
        // narrow, but CT is the right default).
        let mut diff: u8 = 0;
        for i in 0..MAC_LEN {
            diff |= computed[i] ^ stored_mac[i];
        }
        if diff != 0 {
            return Err(SealError::BadMac);
        }

        // MAC OK — now decrypt.
        let mut cipher_key = [0u8; CIPHER_KEY_LEN];
        hkdf_expand(&prk, HKDF_INFO_CIPHER, &mut cipher_key);
        out.clear();
        for (i, &b) in cipher.iter().enumerate() {
            out.push(b ^ cipher_key[i % cipher_key.len()])
                .map_err(|_| SealError::OutputFull)?;
        }
        Ok(OpenOutcome::Dbo2Decoded)
    } else {
        // Fall back to DBO1 / legacy.
        match wifi_pass_seal::open_sealed_bytes(stored, device_key, out) {
            Ok(wifi_pass_seal::OpenOutcome::DecodedBytes) => Ok(OpenOutcome::Dbo1Decoded),
            Ok(wifi_pass_seal::OpenOutcome::LegacyPlaintext(s)) => {
                Ok(OpenOutcome::LegacyPlaintext(s))
            }
            Err(e) => Err(match e {
                wifi_pass_seal::SealError::PlaintextTooLong => SealError::PlaintextTooLong,
                wifi_pass_seal::SealError::EmptyKey => SealError::EmptyKey,
                wifi_pass_seal::SealError::EmptyNonce => SealError::EmptyNonce,
                wifi_pass_seal::SealError::BadFormat => SealError::BadHex,
                wifi_pass_seal::SealError::UnknownVersion => SealError::BadPrefix,
                wifi_pass_seal::SealError::CipherTooShort => SealError::BadLength,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Generic secret seal / open (for non-Wi-Fi credentials)
// ---------------------------------------------------------------------------
//
// `seal_str` / `open_sealed_v2` are capped at `MAX_PLAINTEXT` (64 bytes),
// which is right for a Wi-Fi password but too small for the other secrets
// this device stores in NVS (the DeepSeek/LLM API key is allowed up to 128
// bytes by `at_validate::LLM_API_KEY_MAX`, and OAuth tokens are similar).
// These two functions generalise the exact same HKDF-XOR-HMAC-DBO2
// construction to a 256-byte plaintext cap. They are **additive**: the
// Wi-Fi path is untouched, and the wire format + key schedule are identical,
// so a blob written by `seal_secret` can be opened by `open_secret` and vice
// versa. The firmware LLM-config path should call these instead of writing
// the API key to NVS in the clear (see SECURITY_ASSESSMENT_2026_08_25.md).

/// Maximum plaintext size for [`seal_secret`] / [`open_secret`].
///
/// Decoupled from the Wi-Fi password cap ([`MAX_PLAINTEXT`] = 64) so long
/// credentials (LLM API keys ≤128 bytes, OAuth/refresh tokens, MQTT/email
/// passwords) fit. Bounds the wire format so callers can size their output
/// buffer once.
pub const MAX_SECRET_PLAINTEXT: usize = 256;

/// Maximum encoded length for a blob produced by [`seal_secret`]:
/// `DBO2:`(5) + 2*NONCE_LEN + 2*MAX_SECRET_PLAINTEXT + 2*MAC_LEN.
pub const MAX_SECRET_ENCODED_LEN: usize = 5 + 2 * (NONCE_LEN + MAX_SECRET_PLAINTEXT + MAC_LEN);

/// Outcome of [`open_secret`].
#[derive(Debug, PartialEq, Eq)]
pub enum SecretOpenOutcome {
    /// The stored value was a `DBO2:` blob and opened cleanly.
    Dbo2Decoded,
    /// The stored value had no `DBO2:` prefix. Treat it as legacy
    /// plaintext and use it verbatim — the migration shim for
    /// entries written before sealing was introduced, so an
    /// in-the-field device upgrades in place without losing its
    /// credential.
    LegacyPlaintext,
}

/// Seal an arbitrary-size secret `plain` (bytes) with the device-bound
/// `device_key` + a fresh `nonce`, writing the DBO2 wire format into `out`.
///
/// Same construction as [`seal_str`] (HKDF-SHA256 key stretch → XOR stream +
/// truncated HMAC-SHA256 integrity, constant-time MAC check on open) but
/// with a 256-byte plaintext cap so long credentials fit. `out` must have
/// capacity at least [`MAX_SECRET_ENCODED_LEN`].
pub fn seal_secret(
    plain: &[u8],
    device_key: &[u8],
    nonce: &[u8],
    out: &mut HeaplessString<MAX_SECRET_ENCODED_LEN>,
) -> Result<(), SealError> {
    if device_key.is_empty() {
        return Err(SealError::EmptyKey);
    }
    if nonce.is_empty() {
        return Err(SealError::EmptyNonce);
    }
    if plain.len() > MAX_SECRET_PLAINTEXT {
        return Err(SealError::PlaintextTooLong);
    }

    // 1. Stretch the device key with HKDF to derive per-entry
    //    cipher and MAC keys (same schedule as `seal_str`).
    let prk = hkdf_extract(nonce, device_key);
    let mut cipher_key = [0u8; CIPHER_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_CIPHER, &mut cipher_key);
    let mut mac_key = [0u8; MAC_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_MAC, &mut mac_key);

    // 2. XOR-stream into a bounded cipher buffer.
    let mut cipher: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
    for (i, &b) in plain.iter().enumerate() {
        let kb = cipher_key[i % cipher_key.len()];
        let _ = cipher.push(b ^ kb);
    }

    // 3. Compute the MAC over (nonce || cipher).
    let mac = mac_bytes(&mac_key, nonce, &cipher);

    // 4. Write the wire format: "DBO2:" + hex(nonce) + hex(cipher) + hex(mac).
    out.clear();
    out.push_str(DBO2_PREFIX)
        .map_err(|_| SealError::OutputFull)?;
    for &b in nonce.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        out.push_str(core::str::from_utf8(&buf).expect("hex is ASCII"))
            .map_err(|_| SealError::OutputFull)?;
    }
    for &b in cipher.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        out.push_str(core::str::from_utf8(&buf).expect("hex is ASCII"))
            .map_err(|_| SealError::OutputFull)?;
    }
    for &b in mac.iter() {
        let mut buf = [0u8; 2];
        hex_encode_byte(&mut buf, b)?;
        out.push_str(core::str::from_utf8(&buf).expect("hex is ASCII"))
            .map_err(|_| SealError::OutputFull)?;
    }
    Ok(())
}

/// Open a secret stored by [`seal_secret`].
///
/// * `DBO2:` prefix present → decode, MAC-verify (constant time), decrypt
///   into `out`, return [`SecretOpenOutcome::Dbo2Decoded`].
/// * Otherwise → return [`SecretOpenOutcome::LegacyPlaintext`] so the
///   caller can use the stored value verbatim (entries written before
///   sealing existed). The caller is expected to re-seal such an entry on
///   the next successful open, mirroring the Wi-Fi `AT+WIFIPASSUPGRADE`
///   migration pattern.
pub fn open_secret(
    stored: &str,
    device_key: &[u8],
    out: &mut Vec<u8, MAX_SECRET_PLAINTEXT>,
) -> Result<SecretOpenOutcome, SealError> {
    let Some(payload) = stored.strip_prefix(DBO2_PREFIX) else {
        return Ok(SecretOpenOutcome::LegacyPlaintext);
    };

    // Must at least hold nonce + MAC; cipher length must be even (hex).
    if payload.len() < 2 * (NONCE_LEN + MAC_LEN) || !(payload.len() - 2 * MAC_LEN).is_multiple_of(2)
    {
        return Err(SealError::BadLength);
    }

    // Decode nonce (always NONCE_LEN hex chars).
    let nonce_hex = &payload.as_bytes()[..2 * NONCE_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    for i in 0..NONCE_LEN {
        nonce[i] =
            hex_decode_byte(nonce_hex[i * 2], nonce_hex[i * 2 + 1]).ok_or(SealError::BadHex)?;
    }

    // Decode MAC (last MAC_LEN hex chars).
    let mac_hex = &payload.as_bytes()[payload.len() - 2 * MAC_LEN..];
    let mut stored_mac = [0u8; MAC_LEN];
    for i in 0..MAC_LEN {
        stored_mac[i] =
            hex_decode_byte(mac_hex[i * 2], mac_hex[i * 2 + 1]).ok_or(SealError::BadHex)?;
    }

    // Decode cipher (middle portion).
    let cipher_hex = &payload.as_bytes()[2 * NONCE_LEN..payload.len() - 2 * MAC_LEN];
    if cipher_hex.len() % 2 != 0 {
        return Err(SealError::BadHex);
    }
    let cipher_len = cipher_hex.len() / 2;
    if cipher_len > MAX_SECRET_PLAINTEXT {
        return Err(SealError::PlaintextTooLong);
    }
    let mut cipher: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
    for i in 0..cipher_len {
        let b =
            hex_decode_byte(cipher_hex[i * 2], cipher_hex[i * 2 + 1]).ok_or(SealError::BadHex)?;
        cipher.push(b).map_err(|_| SealError::OutputFull)?;
    }

    // Re-derive keys and check the MAC (constant-time compare).
    let prk = hkdf_extract(&nonce, device_key);
    let mut mac_key = [0u8; MAC_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_MAC, &mut mac_key);
    let computed = mac_bytes(&mac_key, &nonce, &cipher);
    let mut diff: u8 = 0;
    for i in 0..MAC_LEN {
        diff |= computed[i] ^ stored_mac[i];
    }
    if diff != 0 {
        return Err(SealError::BadMac);
    }

    // MAC OK — decrypt.
    let mut cipher_key = [0u8; CIPHER_KEY_LEN];
    hkdf_expand(&prk, HKDF_INFO_CIPHER, &mut cipher_key);
    out.clear();
    for (i, &b) in cipher.iter().enumerate() {
        out.push(b ^ cipher_key[i % cipher_key.len()])
            .map_err(|_| SealError::OutputFull)?;
    }
    Ok(SecretOpenOutcome::Dbo2Decoded)
}

// ---------------------------------------------------------------------------
// Algorithm introspection
// ---------------------------------------------------------------------------

/// Algorithm name used in `+WIFIPASSUPGRADE=?` audit replies.
pub const ALG_NAME: &str = "DBO2";

/// Helper for callers that want to write a `+CWJAP:` Query
/// hint line when the stored entry is DBO1 / legacy plaintext.
#[inline]
pub fn is_legacy(stored: &str) -> bool {
    !stored.starts_with(DBO2_PREFIX)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    fn test_nonce() -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        for (i, b) in n.iter_mut().enumerate() {
            *b = i as u8;
        }
        n
    }

    #[test]
    fn seal_then_open_round_trips() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("hello", TEST_KEY, &nonce, &mut sealed).unwrap();

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome = open_sealed_v2(&sealed, TEST_KEY, &mut out).unwrap();
        assert_eq!(outcome, OpenOutcome::Dbo2Decoded);
        assert_eq!(&out[..], b"hello");
    }

    #[test]
    fn sealed_wire_format_starts_with_dbo2_prefix() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("hi", TEST_KEY, &nonce, &mut sealed).unwrap();
        assert!(sealed.starts_with(DBO2_PREFIX));
        // Length should be exactly:
        // 5 (prefix) + 2*NONCE_LEN (nonce hex) + 2*2 (cipher hex) + 2*MAC_LEN (mac hex)
        let expected = 5 + 2 * NONCE_LEN + 2 * 2 + 2 * MAC_LEN;
        assert_eq!(sealed.len(), expected);
    }

    #[test]
    fn empty_plaintext_round_trips() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("", TEST_KEY, &nonce, &mut sealed).unwrap();

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome = open_sealed_v2(&sealed, TEST_KEY, &mut out).unwrap();
        assert_eq!(outcome, OpenOutcome::Dbo2Decoded);
        assert!(out.is_empty());
    }

    #[test]
    fn max_plaintext_round_trips() {
        const PLAIN: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(PLAIN.len(), MAX_PLAINTEXT);
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str(PLAIN, TEST_KEY, &nonce, &mut sealed).unwrap();

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_v2(&sealed, TEST_KEY, &mut out).unwrap();
        assert_eq!(&out[..], PLAIN.as_bytes());
    }

    #[test]
    fn different_nonces_yield_different_ciphertexts() {
        let nonce_a = test_nonce();
        let mut nonce_b = nonce_a;
        nonce_b[0] ^= 0xff;
        let mut a: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let mut b: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("the-same-password", TEST_KEY, &nonce_a, &mut a).unwrap();
        seal_str("the-same-password", TEST_KEY, &nonce_b, &mut b).unwrap();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn different_keys_yield_different_ciphertexts() {
        let nonce = test_nonce();
        let key_a = TEST_KEY;
        let mut key_b = *TEST_KEY;
        key_b[0] ^= 0xff;
        let mut a: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let mut b: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("secret", key_a, &nonce, &mut a).unwrap();
        seal_str("secret", &key_b, &nonce, &mut b).unwrap();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn tampered_ciphertext_fails_mac() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("secret", TEST_KEY, &nonce, &mut sealed).unwrap();

        // Flip one nibble in the cipher portion. The MAC should
        // not match.
        let mut tampered: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        // Cipher portion begins after "DBO2:" (5) + nonce_hex (2*NONCE_LEN).
        let cipher_offset = 5 + 2 * NONCE_LEN;
        let original_bytes = sealed.as_bytes();
        // Flip the first hex char in the cipher portion.
        let new_char = if original_bytes[cipher_offset] == b'0' {
            b'1'
        } else {
            b'0'
        };
        for (i, &b) in original_bytes.iter().enumerate() {
            if i == cipher_offset {
                let _ = tampered.push(new_char as char);
            } else {
                let _ = tampered.push(b as char);
            }
        }

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let err = open_sealed_v2(&tampered, TEST_KEY, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadMac);
    }

    #[test]
    fn wrong_key_fails_mac() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("secret", TEST_KEY, &nonce, &mut sealed).unwrap();

        let wrong_key = [0xffu8; 32];
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let err = open_sealed_v2(&sealed, &wrong_key, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadMac);
    }

    #[test]
    fn non_hex_payload_fails_bad_hex() {
        // "DBO2:" + zzz*4 — long enough to skip BadLength but
        // not hex. Should fail with BadHex.
        let mut bad: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let _ = bad.push_str("DBO2:");
        for _ in 0..NONCE_LEN {
            let _ = bad.push_str("zz");
        }
        for _ in 0..MAX_PLAINTEXT {
            let _ = bad.push_str("zz");
        }
        for _ in 0..MAC_LEN {
            let _ = bad.push_str("zz");
        }
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let err = open_sealed_v2(&bad, TEST_KEY, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadHex);
    }

    #[test]
    fn dbo1_blob_falls_back_to_legacy_decoded() {
        // Build a DBO1 blob via the existing module and confirm
        // open_sealed_v2 transparently opens it.
        let nonce = test_nonce();
        let mut dbo1: HeaplessString<{ wifi_pass_seal::MAX_ENCODED_LEN }> = HeaplessString::new();
        wifi_pass_seal::seal_str("legacy-pw", TEST_KEY, &nonce, &mut dbo1).unwrap();

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome = open_sealed_v2(&dbo1, TEST_KEY, &mut out).unwrap();
        assert_eq!(outcome, OpenOutcome::Dbo1Decoded);
        assert_eq!(&out[..], b"legacy-pw");
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        // No prefix at all -> legacy plaintext.
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome = open_sealed_v2("no-seal-here", TEST_KEY, &mut out).unwrap();
        match outcome {
            OpenOutcome::LegacyPlaintext(s) => assert_eq!(s, "no-seal-here"),
            _ => panic!("expected LegacyPlaintext"),
        }
    }

    /// Full DBO1 → DBO2 migration round-trip:
    ///
    /// 1. Seal under DBO1 (legacy path).
    /// 2. Open via `open_sealed_v2` (transparent DBO1 fallback).
    /// 3. Re-seal the recovered plaintext under DBO2.
    /// 4. Open via `open_sealed_v2` (DBO2 path).
    /// 5. Assert plaintexts are byte-identical.
    ///
    /// This is the exact algorithm `AT+WIFIPASSUPGRADE=1`
    /// executes in firmware; it's here as a host-side regression
    /// so the dispatcher and the migration tool can never diverge.
    #[test]
    fn dbo1_to_dbo2_migration_round_trip() {
        let original = b"hunter2-correct-horse-battery-staple";
        // Step 1: legacy seal under DBO1.
        let legacy_nonce = test_nonce();
        let mut dbo1_blob: HeaplessString<{ wifi_pass_seal::MAX_ENCODED_LEN }> =
            HeaplessString::new();
        wifi_pass_seal::seal_str(
            core::str::from_utf8(original).unwrap(),
            TEST_KEY,
            &legacy_nonce,
            &mut dbo1_blob,
        )
        .unwrap();

        // Step 2: open via open_sealed_v2 (must transparently fall
        // back to DBO1 and report OpenOutcome::Dbo1Decoded).
        let mut recovered: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome1 = open_sealed_v2(&dbo1_blob, TEST_KEY, &mut recovered).unwrap();
        assert_eq!(outcome1, OpenOutcome::Dbo1Decoded);
        assert_eq!(&recovered[..], original);

        // Step 3: re-seal under DBO2 with a fresh nonce.
        let dbo2_nonce = test_nonce();
        let mut dbo2_blob: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let plain_str = core::str::from_utf8(&recovered).unwrap();
        seal_str(plain_str, TEST_KEY, &dbo2_nonce, &mut dbo2_blob).unwrap();

        // Step 4: open the DBO2 blob.
        let mut recovered_again: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome2 = open_sealed_v2(&dbo2_blob, TEST_KEY, &mut recovered_again).unwrap();
        assert_eq!(outcome2, OpenOutcome::Dbo2Decoded);

        // Step 5: byte-identical.
        assert_eq!(recovered_again.len(), recovered.len());
        assert_eq!(&recovered_again[..], &recovered[..]);
        assert_eq!(&recovered_again[..], original);

        // Sanity: the two blobs differ (fresh nonce on the DBO2
        // re-seal means the ciphertext and MAC must both change).
        assert_ne!(
            dbo1_blob.as_str(),
            dbo2_blob.as_str(),
            "DBO1 and DBO2 blobs must not be byte-equal"
        );
        assert!(dbo2_blob.starts_with("DBO2:"));
        assert!(dbo1_blob.starts_with("DBO1:"));
    }

    #[test]
    fn seal_rejects_empty_key() {
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str("anything", &[], &nonce, &mut sealed);
        assert_eq!(r, Err(SealError::EmptyKey));
    }

    #[test]
    fn seal_rejects_empty_nonce() {
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str("anything", TEST_KEY, &[], &mut sealed);
        assert_eq!(r, Err(SealError::EmptyNonce));
    }

    #[test]
    fn seal_rejects_oversized_plaintext() {
        const BIG: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(BIG.len(), MAX_PLAINTEXT + 1);
        let nonce = test_nonce();
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str(BIG, TEST_KEY, &nonce, &mut sealed);
        assert_eq!(r, Err(SealError::PlaintextTooLong));
    }

    #[test]
    fn seal_rejects_short_prefix_payload() {
        // "DBO2:zz" — too short to contain nonce + MAC.
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let err = open_sealed_v2("DBO2:zz", TEST_KEY, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadLength);
    }

    #[test]
    fn alg_name_is_stable() {
        // Pin the algorithm name (used in audit logs and AT
        // replies). Bumping the algorithm requires bumping
        // both `VERSION_TAG`, the HKDF info suffixes, and this
        // constant — three coordinated edits that any future
        // change will see in review.
        assert_eq!(ALG_NAME, "DBO2");
        assert_eq!(VERSION_TAG, "DBO2");
    }

    #[test]
    fn is_legacy_detects_non_dbo2_prefix() {
        assert!(is_legacy("plain-text"));
        assert!(is_legacy("DBO1:abcd"));
        assert!(!is_legacy("DBO2:abcd"));
        // Empty string has no prefix, so technically "legacy" —
        // but is_legacy is only meaningfully called on stored
        // values that the caller already knows are non-empty.
        // Document the empty-string behaviour as a degenerate case.
        assert!(is_legacy(""));
    }

    // ------------------------------------------------------------------
    // Generic secret seal / open (seal_secret / open_secret)
    // ------------------------------------------------------------------

    #[test]
    fn secret_round_trips_with_long_plaintext() {
        // A 128-byte secret (the max LLM API-key length) must survive
        // a seal -> open round trip — this is the case the Wi-Fi-only
        // `seal_str` (cap 64) cannot handle.
        let secret: [u8; 128] = {
            let mut s = [0u8; 128];
            for (i, b) in s.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(7).wrapping_add(3);
            }
            s
        };
        let nonce = test_nonce();
        let mut blob: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        seal_secret(&secret, TEST_KEY, &nonce, &mut blob).unwrap();
        assert!(blob.starts_with(DBO2_PREFIX));

        let mut out: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
        let outcome = open_secret(&blob, TEST_KEY, &mut out).unwrap();
        assert_eq!(outcome, SecretOpenOutcome::Dbo2Decoded);
        assert_eq!(&out[..], &secret[..]);
    }

    #[test]
    fn secret_tamper_is_detected() {
        let nonce = test_nonce();
        let mut blob: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        seal_secret(
            b"sk-abcdefghijklmnopqrstuvwxyz",
            TEST_KEY,
            &nonce,
            &mut blob,
        )
        .unwrap();
        // Flip one hex character in the middle of the blob by rebuilding
        // it with a single nibble changed.
        let mid = blob.len() / 2;
        let bytes = blob.as_bytes();
        let mut tampered: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        for (i, &b) in bytes.iter().enumerate() {
            let c = if i == mid {
                match b {
                    b'a' => b'b',
                    b'f' => b'e',
                    other => other ^ 0x01,
                }
            } else {
                b
            };
            let _ = tampered.push(c as char);
        }
        let mut out: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
        let err = open_secret(&tampered, TEST_KEY, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadMac);
    }

    #[test]
    fn secret_wrong_key_fails_mac() {
        let nonce = test_nonce();
        let mut blob: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        seal_secret(b"sk-secret-token-value", TEST_KEY, &nonce, &mut blob).unwrap();
        let wrong = [0xabu8; 32];
        let mut out: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
        let err = open_secret(&blob, &wrong, &mut out).unwrap_err();
        assert_eq!(err, SealError::BadMac);
    }

    #[test]
    fn secret_legacy_plaintext_is_reported_not_opened() {
        // A value with no DBO2: prefix must surface as legacy
        // plaintext so the caller can use it verbatim (migration shim).
        let mut out: Vec<u8, MAX_SECRET_PLAINTEXT> = Vec::new();
        let outcome = open_secret("sk-legacy-key", TEST_KEY, &mut out).unwrap();
        assert_eq!(outcome, SecretOpenOutcome::LegacyPlaintext);
        assert!(out.is_empty());
    }

    #[test]
    fn secret_rejects_empty_key_and_nonce() {
        let nonce = test_nonce();
        let mut blob: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        assert_eq!(
            seal_secret(b"x", &[], &nonce, &mut blob),
            Err(SealError::EmptyKey)
        );
        assert_eq!(
            seal_secret(b"x", TEST_KEY, &[], &mut blob),
            Err(SealError::EmptyNonce)
        );
    }

    #[test]
    fn secret_rejects_oversized_plaintext() {
        let big = [0x55u8; MAX_SECRET_PLAINTEXT + 1];
        let nonce = test_nonce();
        let mut blob: HeaplessString<MAX_SECRET_ENCODED_LEN> = HeaplessString::new();
        assert_eq!(
            seal_secret(&big, TEST_KEY, &nonce, &mut blob),
            Err(SealError::PlaintextTooLong)
        );
    }

    // Reference the symbols so a future contributor who removes
    // the only actual usage gets a compile error rather than a
    // silent broken build. (The helper itself is not called;
    // it's a compile-time tripwire.)
    #[allow(dead_code)]
    fn _algo_tripwire() {
        let _ = VERSION_TAG;
        let _ = ALG_NAME;
    }
}
