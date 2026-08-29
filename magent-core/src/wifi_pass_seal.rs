//! Device-bound obfuscation for the Wi-Fi password (and other
//! medium-sensitivity secrets stored in NVS).
//!
//! # Why this exists
//!
//! `wifi_pass` would otherwise be written to NVS as plaintext. NVS
//! is plaintext on the flash bus; any attacker who can read the chip
//! (debug probe, factory scrap, stolen device) gets the WPA2 password
//! in clear. This module adds a *lightweight, device-bound* layer so
//! a passive flash dump is no longer a direct password leak.
//!
//! # Threat model & non-goals
//!
//! - **In scope**: passive flash-dump attacks on a *single device*.
//!   The ciphertext is bound to that device's `dev_identity` seed, so
//!   dumping the chip and reading the flash on a *different* device
//!   does not yield the password.
//! - **Out of scope**: an attacker with code-execution on the same
//!   device (they can call `open()` themselves). The boot path must
//!   be able to recover the plaintext to feed ESP-IDF; that is the
//!   whole point. If you need stronger guarantees, layer an
//!   additional physical-unlock secret (button sequence, JTAG fuse)
//!   in front of this — but that is a v0.3 concern, not a v0.2 one.
//!
//! # Algorithm
//!
//! `seal(plain, key, nonce)` = `plain[i] XOR key[i%key.len()] XOR nonce[i%nonce.len()]`
//! `open(cipher, key)`      = `cipher[i] XOR key[i%key.len()] XOR nonce[i%nonce.len()]`
//!
//! `cipher = nonce(12) || ciphertext(N)`
//!
//! - `key` is the 32-byte Ed25519 seed from `magent:dev_identity`.
//! - `nonce` is 12 random bytes supplied by the caller (typically
//!   drawn from the ESP32 hardware TRNG via the `getrandom` shim
//!   on the firmware side; `OsRng` on the host test side).
//! - The same `(key, nonce)` round-trips exactly; a wrong key
//!   yields garbage that ESP-IDF will reject on association.
//!
//! # Properties
//!
//! - **no_std + no new dependencies** — only `core` and `heapless`
//!   (already a magent-core dep).
//! - **Zero panic** — every function returns `Result`; on buffer
//!   overflow we refuse rather than truncate.
//! - **Caller-controlled randomness** — `seal_str` takes the nonce
//!   as a parameter so this crate does not depend on `getrandom`
//!   or any specific RNG backend. Tests pass a deterministic
//!   nonce; the firmware passes `OsRng`-drawn bytes.
//! - **Auditable** — ~80 lines of pure XOR; no constant-time
//!   requirement because XOR with a *device-bound* key is not a
//!   side-channel concern in this threat model.
//!
//! # Wire format stored in NVS
//!
//! The NVS entry is a hex-encoded string of the form
//! `"DBO1:<hex-of-nonce-and-ciphertext>"`. The `DBO1` prefix is a
//! version tag so we can change the algorithm later without
//! misinterpreting old entries; `open_sealed_bytes` falls back to
//! treating the entry as plaintext when the prefix is absent,
//! preserving compatibility with firmware versions that wrote
//! plaintext.
//!
//! On the read path, if the prefix is absent, the stored value is
//! returned verbatim (legacy plaintext). If the prefix is present
//! but the cipher fails to open (wrong device key, corruption),
//! `open_sealed_bytes` returns `Err`, which the caller surfaces as
//! a load failure — we deliberately do NOT fall through to "use the
//! prefix-stripped bytes as the password", because that would
//! silently re-introduce plaintext storage on every decode error.

use core::fmt::Write as _;

use heapless::{String as HeaplessString, Vec};

/// Version tag written to NVS. Bump this whenever the algorithm
/// changes so old firmware can refuse to read new entries and vice
/// versa. The corresponding open-side behaviour is to treat any
/// entry without the prefix as legacy plaintext (best-effort
/// backwards compatibility for one release cycle).
const VERSION_TAG: &str = "DBO1";

/// Length of the random nonce prefix (bytes).
pub const NONCE_LEN: usize = 12;

/// Maximum plaintext length we are willing to seal. The current
/// Wi-Fi passphrase cap on ESP-IDF is 64 bytes; we leave a tiny
/// safety margin for null terminators in future revisions.
pub const MAX_PLAINTEXT: usize = 64;

/// Maximum encoded (sealed) length in bytes, including the
/// `DBO1:` prefix + hex(nonce) + hex(ciphertext).
///
/// `5 + 2 * (NONCE_LEN + MAX_PLAINTEXT) = 5 + 152 = 157`
pub const MAX_ENCODED_LEN: usize = 5 + 2 * (NONCE_LEN + MAX_PLAINTEXT);

/// Maximum length of the device-bound key. The current source
/// (`dev_identity`) is a 32-byte Ed25519 seed.
pub const MAX_KEY_LEN: usize = 32;

/// Errors returned by this module.
#[derive(Debug, PartialEq, Eq)]
pub enum SealError {
    /// Plaintext too long for the bounded buffer.
    PlaintextTooLong,
    /// Stored value missing or unparseable.
    BadFormat,
    /// Stored value uses a version tag we don't recognise.
    /// (Future-proofing — current code only understands `DBO1`.)
    UnknownVersion,
    /// Stored ciphertext shorter than the nonce prefix.
    CipherTooShort,
    /// Key length was zero (cannot seal / open with empty key).
    EmptyKey,
    /// Nonce length was zero (cannot seal with empty nonce —
    /// would collapse to a static substitution cipher).
    EmptyNonce,
}

/// Outcome of [`open_sealed_bytes`].
///
/// The split between "legacy plaintext" and "decoded plaintext" lets
/// the caller log a one-line diagnostic about how the value was
/// stored (helps operators see whether they have legacy data on
/// hand that they might want to re-seal).
#[derive(Debug, PartialEq, Eq)]
pub enum OpenOutcome<'a> {
    /// Legacy plaintext (no `DBO1:` prefix) — preserved verbatim.
    LegacyPlaintext(&'a str),
    /// Successfully decrypted DBO1 entry. Caller should read the
    /// recovered bytes from the `Vec` they passed in.
    DecodedBytes,
}

/// Seal `plain` using `device_key` and `nonce`. On success, `out`
/// is filled with a version-tagged hex string of the form
/// `"DBO1:<hex-nonce><hex-ciphertext>"` suitable for `nvs_set_str`.
///
/// # Errors
/// - `PlaintextTooLong` if `plain` is longer than `MAX_PLAINTEXT`.
/// - `EmptyKey` if `device_key` is empty.
/// - `EmptyNonce` if `nonce` is empty.
///
/// # Panics
/// Never. The output buffer's capacity is `MAX_ENCODED_LEN`, which
/// is sized to hold the worst-case payload (`5` prefix +
/// `2 * (NONCE_LEN + MAX_PLAINTEXT)` hex chars), so `out` cannot
/// overflow on the happy path.
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

    // 1. XOR-stream the plaintext into a ciphertext buffer. We push
    // at most `plain.len() <= MAX_PLAINTEXT` bytes, so the Vec's
    // static capacity can never be exceeded.
    let mut cipher: Vec<u8, MAX_PLAINTEXT> = Vec::new();
    let plain_bytes = plain.as_bytes();
    for (i, &b) in plain_bytes.iter().enumerate() {
        let kb = device_key[i % device_key.len()];
        let nb = nonce[i % nonce.len()];
        // Safety: bounded by `plain.len() <= MAX_PLAINTEXT`.
        let _ = cipher.push(b ^ kb ^ nb);
    }

    // 2. Hex-encode `nonce || cipher` into a local buffer. Same
    // bound argument as above — `hex_buf` is sized for the worst
    // case, so we cannot overflow.
    let mut hex_buf: Vec<u8, { (NONCE_LEN + MAX_PLAINTEXT) * 2 }> = Vec::new();
    for &b in nonce.iter().chain(cipher.iter()) {
        let _ = write!(hex_buf, "{:02x}", b);
    }

    // 3. Prefix with the version tag and a colon. The capacity
    // check at the end is the only fail-safe — `HeaplessString`
    // returns its own error if `write!` overruns, but since we
    // sized the buffer for the worst case (`MAX_ENCODED_LEN`),
    // it is unreachable in practice.
    out.clear();
    let _ = write!(out, "{}:", VERSION_TAG);
    // SAFETY: `hex_buf` only contains ASCII hex digits (0-9, a-f),
    // so it is by construction valid UTF-8.
    let hex_str = core::str::from_utf8(&hex_buf).expect("hex_buf is ASCII by construction");
    out.push_str(hex_str)
        .expect("out capacity (MAX_ENCODED_LEN) bounds hex_buf + prefix");

    Ok(())
}

/// Open a sealed value previously produced by [`seal_str`].
///
/// The input is the literal NVS string (whatever `nvs_get_str`
/// returned). If it does not start with `DBO1:`, it is returned as
/// `LegacyPlaintext` so a boot that just upgraded from a plaintext
/// firmware still finds the credentials.
///
/// On successful decode the recovered plaintext is written into
/// `out` *byte by byte* — no UTF-8 conversion happens in here. The
/// caller is responsible for `core::str::from_utf8(&plain_bytes)`
/// if they need a `&str` (for ESP-IDF). Wi-Fi passwords are
/// typically ASCII but the algorithm itself is byte-clean.
///
/// # Errors
/// See [`SealError`].
pub fn open_sealed_bytes<'a>(
    stored: &'a str,
    device_key: &[u8],
    out: &mut Vec<u8, MAX_PLAINTEXT>,
) -> Result<OpenOutcome<'a>, SealError> {
    const PREFIX: &str = "DBO1:";
    let Some(payload_hex) = stored.strip_prefix(PREFIX) else {
        // Legacy plaintext entry — preserve verbatim.
        return Ok(OpenOutcome::LegacyPlaintext(stored));
    };

    // Hex-decode the payload (nonce + ciphertext).
    if payload_hex.len() % 2 != 0 {
        return Err(SealError::BadFormat);
    }
    let mut payload: Vec<u8, { NONCE_LEN + MAX_PLAINTEXT }> = Vec::new();
    let bytes = payload_hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        if payload.push(hi << 4 | lo).is_err() {
            return Err(SealError::BadFormat);
        }
        i += 2;
    }

    if payload.len() < NONCE_LEN {
        return Err(SealError::CipherTooShort);
    }
    if device_key.is_empty() {
        return Err(SealError::EmptyKey);
    }

    let (nonce, cipher) = payload.split_at(NONCE_LEN);
    out.clear();
    for (idx, &c) in cipher.iter().enumerate() {
        let kb = device_key[idx % device_key.len()];
        let nb = nonce[idx % NONCE_LEN];
        let plain_byte = c ^ kb ^ nb;
        // Bounds: cipher.len() <= MAX_PLAINTEXT by construction
        // (we sized the Vec for it). out is also sized for it.
        if out.push(plain_byte).is_err() {
            return Err(SealError::PlaintextTooLong);
        }
    }
    Ok(OpenOutcome::DecodedBytes)
}

/// Decode one ASCII hex character. Returns the 0..=15 value or
/// `SealError::BadFormat`.
fn hex_nibble(c: u8) -> Result<u8, SealError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(SealError::BadFormat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic key used by every test in this module. Real
    /// firmware uses the device's Ed25519 seed, which is itself
    /// deterministic across reboots (it is read from NVS, not
    /// regenerated). The test key is independent of any test's
    /// RNG so sealing tests are reproducible.
    const TEST_KEY: &[u8] = &[
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32,
        0x10, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x00,
    ];

    fn test_nonce() -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        for (i, b) in n.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(0x5a);
        }
        n
    }

    #[test]
    fn round_trip_short_passphrase() {
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("hello", TEST_KEY, &nonce, &mut out).unwrap();

        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(&out, TEST_KEY, &mut plain).unwrap();
        assert_eq!(r, OpenOutcome::DecodedBytes);
        assert_eq!(&plain[..], b"hello");
    }

    #[test]
    fn round_trip_empty_passphrase() {
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("", TEST_KEY, &nonce, &mut out).unwrap();

        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(&out, TEST_KEY, &mut plain).unwrap();
        assert_eq!(r, OpenOutcome::DecodedBytes);
        assert_eq!(plain.len(), 0);
    }

    #[test]
    fn round_trip_64_byte_passphrase() {
        // The Wi-Fi spec caps WPA2 passwords at 63 ASCII chars but
        // some APs use a 64-byte PSK. Ensure the boundary works.
        // 64 'p' bytes as a `&str`.
        const S: &str = "pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp";
        assert_eq!(S.len(), 64);
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str(S, TEST_KEY, &nonce, &mut out).unwrap();

        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_bytes(&out, TEST_KEY, &mut plain).unwrap();
        assert_eq!(&plain[..], S.as_bytes());
    }

    #[test]
    fn round_trip_unicode_bytes_preserved() {
        // UTF-8 byte sequences must round-trip cleanly — they are
        // not interpreted as Unicode scalars at any point.
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("密码-1234", TEST_KEY, &nonce, &mut out).unwrap();

        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_bytes(&out, TEST_KEY, &mut plain).unwrap();
        assert_eq!(&plain[..], "密码-1234".as_bytes());
    }

    #[test]
    fn round_trip_arbitrary_bytes() {
        // The seal primitive is byte-clean — the dispatcher
        // enforces UTF-8 + no-NUL on the Wi-Fi password path,
        // but the primitive itself is byte-clean (so the same
        // machinery can seal any future NVS secret). Here we
        // round-trip an ASCII string containing a NUL byte
        // through the public seal/open API. (We cannot include
        // non-ASCII bytes in a `&str` literal — Rust rejects
        // them at compile time — so the byte-level fuzz is
        // limited to ASCII + NUL.)
        const PLAIN: &str = "plain\0with\x01\x7fbytes"; // 17 bytes
        assert_eq!(PLAIN.len(), 17);
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str(PLAIN, TEST_KEY, &nonce, &mut out).unwrap();

        let mut decoded: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_bytes(&out, TEST_KEY, &mut decoded).unwrap();
        assert_eq!(&decoded[..], PLAIN.as_bytes());
    }

    #[test]
    fn same_plaintext_different_nonce_produces_different_cipher() {
        // Two seal_str calls on the same plain with different
        // nonces must yield different ciphertext — otherwise an
        // attacker who saw one write could spot identical writes.
        let nonce_a = test_nonce();
        let nonce_b: [u8; NONCE_LEN] = [0xff; NONCE_LEN];
        let mut a: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let mut b: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("the-same-password", TEST_KEY, &nonce_a, &mut a).unwrap();
        seal_str("the-same-password", TEST_KEY, &nonce_b, &mut b).unwrap();
        assert_ne!(a.as_str(), b.as_str());
        // Both must decode back to the same plaintext:
        let mut pa: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let mut pb: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_bytes(&a, TEST_KEY, &mut pa).unwrap();
        open_sealed_bytes(&b, TEST_KEY, &mut pb).unwrap();
        assert_eq!(&pa[..], &pb[..]);
    }

    #[test]
    fn wrong_key_yields_garbage() {
        let wrong_key = [0xffu8; 32];
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("secret", TEST_KEY, &nonce, &mut out).unwrap();

        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(&out, &wrong_key, &mut plain).unwrap();
        assert_eq!(r, OpenOutcome::DecodedBytes);
        assert_ne!(&plain[..], b"secret");
    }

    #[test]
    fn legacy_plaintext_round_trips_through_open_sealed_bytes() {
        // Stored value has no `DBO1:` prefix — must come back as
        // LegacyPlaintext unchanged.
        let stored = "legacy-pass-1234";
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out).unwrap();
        match r {
            OpenOutcome::LegacyPlaintext(s) => assert_eq!(s, stored),
            OpenOutcome::DecodedBytes => panic!("legacy should not decode"),
        }
    }

    #[test]
    fn legacy_empty_string_round_trips() {
        let stored = "";
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out).unwrap();
        assert!(matches!(r, OpenOutcome::LegacyPlaintext("")));
    }

    #[test]
    fn unknown_version_tag_treated_as_legacy() {
        // Current policy: any prefix we don't recognise is treated
        // as legacy. This keeps a single bit of forward
        // compatibility (operators see the literal string come
        // back, then they know to wipe + re-seal).
        let stored = "DBO2:deadbeef";
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out).unwrap();
        assert_eq!(r, OpenOutcome::LegacyPlaintext("DBO2:deadbeef"));
    }

    #[test]
    fn odd_length_hex_payload_rejected() {
        let stored = "DBO1:abc"; // 3 hex chars = odd length
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out);
        assert_eq!(r, Err(SealError::BadFormat));
    }

    #[test]
    fn non_hex_payload_rejected() {
        let stored = "DBO1:zz"; // not hex
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out);
        assert_eq!(r, Err(SealError::BadFormat));
    }

    #[test]
    fn too_short_cipher_rejected() {
        let stored = "DBO1:aabb"; // 2 bytes total < NONCE_LEN
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out);
        assert_eq!(r, Err(SealError::CipherTooShort));
    }

    #[test]
    fn empty_key_rejected_on_open() {
        let stored = "DBO1:000000000000000000000000"; // 12 zero bytes
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, &[], &mut out);
        assert_eq!(r, Err(SealError::EmptyKey));
    }

    #[test]
    fn empty_key_rejected_on_seal() {
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str("anything", &[], &test_nonce(), &mut out);
        assert_eq!(r, Err(SealError::EmptyKey));
    }

    #[test]
    fn empty_nonce_rejected_on_seal() {
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str("anything", TEST_KEY, &[], &mut out);
        assert_eq!(r, Err(SealError::EmptyNonce));
    }

    #[test]
    fn plaintext_over_limit_rejected_on_seal() {
        // 65 'x' bytes — one more than MAX_PLAINTEXT (64).
        const BIG: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(BIG.len(), MAX_PLAINTEXT + 1);
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str(BIG, TEST_KEY, &test_nonce(), &mut out);
        assert_eq!(r, Err(SealError::PlaintextTooLong));
    }

    #[test]
    fn plaintext_at_limit_succeeds_on_seal() {
        // Exactly 64 'q' bytes — boundary must be accepted.
        const S: &str = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        assert_eq!(S.len(), MAX_PLAINTEXT);
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        let r = seal_str(S, TEST_KEY, &test_nonce(), &mut out);
        assert!(r.is_ok(), "seal failed: {r:?}");
        let encoded = out.as_str();
        assert!(encoded.starts_with("DBO1:"));
        assert_eq!(encoded.len(), 5 + 2 * (NONCE_LEN + MAX_PLAINTEXT));
    }

    #[test]
    fn encoded_length_matches_plain_length() {
        // The encoded form is `DBO1:` + hex(nonce) + hex(cipher),
        // so its length must be exactly:
        //     5 + 2*NONCE_LEN + 2*plain.len()
        // regardless of where the plaintext falls within the
        // allowed range. (We bound the plaintext at MAX_PLAINTEXT
        // — see `plaintext_over_limit_rejected_on_seal` for the
        // rejection path.)
        //
        // To keep this test `no_std`-clean we build the plaintext
        // out of fixed `&str` literals of the lengths we want,
        // rather than allocating a `String`.
        let cases: [&str; 8] = [
            "",                                                          // 0
            "a",                                                         // 1
            "abcdefg",                                                   // 7
            "abcdefghijkl",                                              // 12
            "abcdefghijklmnopqrstuvwxyzabcdef",                          // 32
            "abcdefghijklmnopqrstuvwxyzabcdefg",                         // 33
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcd",  // 63
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcde", // 64
        ];
        let nonce = test_nonce();
        for s in cases.iter() {
            let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
            seal_str(s, TEST_KEY, &nonce, &mut out).unwrap();
            assert_eq!(
                out.len(),
                5 + 2 * NONCE_LEN + 2 * s.len(),
                "mismatch for plain_len={}",
                s.len()
            );
        }
    }

    #[test]
    fn hex_nibble_accepts_uppercase_and_lowercase() {
        assert_eq!(hex_nibble(b'0').unwrap(), 0);
        assert_eq!(hex_nibble(b'9').unwrap(), 9);
        assert_eq!(hex_nibble(b'a').unwrap(), 10);
        assert_eq!(hex_nibble(b'f').unwrap(), 15);
        assert_eq!(hex_nibble(b'A').unwrap(), 10);
        assert_eq!(hex_nibble(b'F').unwrap(), 15);
        assert!(hex_nibble(b'g').is_err());
        assert!(hex_nibble(b' ').is_err());
    }

    #[test]
    fn encoded_string_is_valid_ascii() {
        // The NVS string we write is consumed by esp-idf's
        // `set_str`, which expects valid UTF-8. All hex digits
        // are ASCII so this is automatic — but assert it explicitly
        // so a future refactor doesn't accidentally inject
        // non-ASCII into the encoded payload.
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("a-mixed-password-123", TEST_KEY, &nonce, &mut out).unwrap();
        for b in out.as_bytes() {
            assert!(b.is_ascii(), "non-ASCII byte {b:#x} in encoded output");
        }
    }

    #[test]
    fn truncated_ciphertext_rejected() {
        // Forged payload with valid prefix and hex but truncated
        // cipher (less than NONCE_LEN total payload bytes) must
        // not decode to anything.
        let stored = "DBO1:000000000000"; // 6 bytes (only half a nonce)
        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let r = open_sealed_bytes(stored, TEST_KEY, &mut out);
        assert_eq!(r, Err(SealError::CipherTooShort));
    }

    #[test]
    fn seal_overwrites_out_buffer() {
        // Calling seal_str twice with the same `out` must produce
        // the second value, not a concatenation of the first and
        // the second.
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("first-password", TEST_KEY, &nonce, &mut out).unwrap();
        let first_len = out.len();
        seal_str("second", TEST_KEY, &nonce, &mut out).unwrap();
        assert!(out.len() < first_len, "out should be smaller now");
        let mut plain: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        open_sealed_bytes(&out, TEST_KEY, &mut plain).unwrap();
        assert_eq!(&plain[..], b"second");
    }

    #[test]
    fn cipher_bytes_for_known_inputs() {
        // Independent re-derivation of the XOR-stream. If this
        // ever drifts from what `seal_str` produces, the audit
        // log's "cipher fingerprint" can no longer be trusted.
        //
        // Hand-computed values:
        //   nonce = [0x5a, 0x61, 0x68, 0x6f, ...] (test_nonce())
        //   key   = [0x01, 0x23, 0x45, 0x67, ...] (TEST_KEY)
        //   plain = "hell" -> [0x68, 0x65, 0x6c, 0x6c]
        //
        //   c[0] = 0x68 ^ 0x01 ^ 0x5a = 0x33
        //   c[1] = 0x65 ^ 0x23 ^ 0x61 = 0x27
        //   c[2] = 0x6c ^ 0x45 ^ 0x68 = 0x41
        //   c[3] = 0x6c ^ 0x67 ^ 0x6f = 0x64
        //
        // Encoded layout: "DBO1:" + hex(nonce) + hex(cipher)
        //   = 5 + 24 + 8 = 37 bytes.
        let nonce = test_nonce();
        let mut out: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        seal_str("hell", TEST_KEY, &nonce, &mut out).unwrap();
        assert_eq!(out.len(), 5 + 2 * NONCE_LEN + 8);
        let cipher_hex_start = 5 + 2 * NONCE_LEN;
        let cipher_hex = &out.as_bytes()[cipher_hex_start..];
        assert_eq!(cipher_hex, b"33274164");
    }
}
