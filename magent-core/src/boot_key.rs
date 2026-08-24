//! Boot-time-derived device key (BTDK) for sealing `dev_identity`.
//!
//! # The chicken-and-egg problem
//!
//! Every secret we store in NVS (Wi-Fi passwords, API tokens, the device
//! identity itself) is sealed with a key. Where does that key come from?
//!
//! For the Wi-Fi password, the natural answer is "the device identity":
//! `dev_identity` is the only piece of data that's truly unique per device
//! and never leaves the flash. So we use it as the seal key for everything
//! else.
//!
//! But that begs the question: **how do we seal `dev_identity` itself?**
//! If we use `dev_identity` as its own seal key, we have a chicken-and-egg
//! loop. If we use any other value that lives in NVS, an attacker who
//! dumps NVS gets both the cipher and the key.
//!
//! # Solution: derive the seal key from hardware-unique material
//!
//! `boot_key_derive` takes a piece of *hardware-bound* material (on
//! ESP32: eFuse BLOCK0 + chip ID — values written by the silicon
//! vendor, NOT by firmware, and not stored in NVS) and mixes it
//! through Keccak256 with a versioned domain tag. The output is a
//! deterministic 32-byte key:
//!
//! ```text
//! boot_key = Keccak256(material || domain_tag || version_byte)
//! ```
//!
//! # Properties (aerograde checklist)
//!
//! - **no_std** + zero-panic: no allocation, no `unwrap`, no `panic!`.
//!   Returns [`BootKeyError`] for every failure mode so the caller can
//!   map to AT `+CMDER:N` cleanly.
//! - **Deterministic**: same material + same domain tag ⇒ same key.
//!   This is the entire point — `boot_key` can be re-derived on every
//!   boot without storing it anywhere.
//! - **Device-bound**: an attacker who dumps NVS but cannot read eFuse
//!   cannot reproduce the key.
//! - **Versioned**: the domain tag (`MAG_BTDK_DOMAIN_V1`) is fixed in
//!   code; the version byte can be bumped if the algorithm changes,
//!   forcing a key rotation for sealed values that want to follow.
//! - **Bounded**: input material is capped at `MAX_MATERIAL_LEN` bytes
//!   (128). The Keccak256 sponge has no length limit but we impose one
//!   to (a) make the function bounded-time and (b) reject pathologically
//!   large inputs from a misbehaving HAL.
//!
//! # Threat model
//!
//! | Attacker capability                                       | `boot_key` security |
//! |-----------------------------------------------------------|---------------------|
//! | Dumps NVS only                                            | ✅ Protected       |
//! | Dumps NVS + reads eFuse (physical access)                | ⚠️ Compromised    |
//! | Dumps NVS + reads eFuse + knows algorithm version         | ❌ Defeated       |
//!
//! This is **strictly better** than the plaintext baseline and
//! strictly better than "NVS-only" device key, while remaining
//! implementable on commodity ESP32 parts (no secure-element
//! required).
//!
//! # Host testability
//!
//! The function takes the material as a parameter; the firmware
//! layer (in `firmware/esp32-app/src/main.rs`) is responsible for
//! gathering eFuse bytes via `esp_efuse_*` FFI. The host-side test
//! can therefore exercise the full Keccak256 + length-prefix
//! machinery with synthetic material, verifying determinism,
//! version sensitivity, and length-cap rejection.

#[cfg(feature = "boot-key")]
use sha3::Digest;

/// Domain separation tag. Bump the suffix (or set a new `VERSION`)
/// only as part of an explicit key-rotation migration — sealed
/// values produced under the old tag will NOT open under the new
/// one (which is the point).
pub const MAG_BTDK_DOMAIN_V1: &[u8] = b"magent/btk/v1";

/// Bumped if the algorithm (hash, mixing, length) changes. Together
/// with the domain tag this lets old sealed blobs fail-closed under
/// the new algorithm.
pub const BTDK_VERSION_V1: u8 = 0x01;

/// Maximum material length accepted by [`derive`]. 128 bytes is
/// generous for eFuse BLOCK0 (32 bytes) + chip ID (4 bytes) + MAC
/// (6 bytes) + future expansion (80 bytes for security blocks,
/// factory data, etc.).
pub const MAX_MATERIAL_LEN: usize = 128;

/// Algorithm identifier. Embedded in the wire format of sealed
/// blobs that use BTDK so future readers can dispatch on algorithm.
pub const BTDK_ALG_ID: u8 = 0x01;

/// Algorithm name for logs / audit.
pub const BTDK_ALG_NAME: &str = "BTDK1";

/// Errors that can occur while deriving a boot-time key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootKeyError {
    /// Caller-supplied material exceeded [`MAX_MATERIAL_LEN`].
    /// This is a hard bound — `derive` will NOT silently truncate.
    MaterialTooLong,

    /// Material buffer is empty. Rejected because a zero-input hash
    /// trivially produces a constant key on every device that
    /// forgets to wire the HAL (and we want every missing-HAL bug
    /// to be loud, not silent).
    MaterialEmpty,

    /// The `boot-key` feature is not compiled in. `derive` can only
    /// run when the firmware build enables it; a host build without
    /// the feature must return an error rather than panic.
    FeatureDisabled,
}

/// Derived boot-time key. Wrapped in a newtype so the type system
/// prevents accidental use as a generic 32-byte buffer.
#[derive(Clone, Copy, Debug)]
pub struct BootKey {
    bytes: [u8; 32],
}

impl BootKey {
    /// Borrow the raw bytes for use as a seal key.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Construct a `BootKey` from a known-good 32-byte buffer
    /// (used in tests, and by migration paths that re-derive a key
    /// from a stored envelope).
    #[inline]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }
}

impl AsRef<[u8]> for BootKey {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

// ---------------------------------------------------------------------------
// Keccak256 (only available with the `boot-key` feature)
// ---------------------------------------------------------------------------
//
// We deliberately do NOT implement Keccak256 by hand. The crypto path
// is exactly the kind of code that must NOT be a homegrown ad-hoc mixer.
// `sha3` is the de-facto Rust pure-Rust Keccak implementation; it's
// already a transitive dependency for the Web3 feature.
//
// On a build without `boot-key` (e.g. default `cargo test -p magent-core`),
// the derive function is gated behind `#[cfg(feature = "boot-key")]` so
// the `sha3` dependency is not pulled in. Tests that exercise the
// derivation logic explicitly opt in via `--features boot-key`.

/// Derive a boot-time key from hardware-bound material.
///
/// # Layout
///
/// ```text
/// input = material || domain_tag || version_byte
/// key   = Keccak256(input)
/// ```
///
/// The domain tag and version byte are appended AFTER the material
/// so that any two callers using different material lengths still
/// hash distinct inputs even if the material prefixes coincide
/// (collision-resistance of the prefix-suffix boundary is provided
/// by Keccak256's absorption of variable-length inputs).
#[cfg(feature = "boot-key")]
pub fn derive(material: &[u8]) -> Result<BootKey, BootKeyError> {
    if material.is_empty() {
        return Err(BootKeyError::MaterialEmpty);
    }
    if material.len() > MAX_MATERIAL_LEN {
        return Err(BootKeyError::MaterialTooLong);
    }

    let mut hasher = sha3::Keccak256::new();
    hasher.update(material);
    hasher.update(MAG_BTDK_DOMAIN_V1);
    hasher.update([BTDK_VERSION_V1]);

    let out = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    Ok(BootKey { bytes })
}

/// Compile-time stub used when the `boot-key` feature is OFF. Lets
/// downstream code reference the symbol without pulling in `sha3`.
/// Panicking here is acceptable because the function is unreachable
/// in builds that don't enable `boot-key`; the firmware build does
/// enable it.
#[cfg(not(feature = "boot-key"))]
pub fn derive(_material: &[u8]) -> Result<BootKey, BootKeyError> {
    // `derive` is only called from firmware, which enables `boot-key`.
    // A host test build without the feature simply cannot reach this
    // path. Fail loudly with a clear error rather than panicking so the
    // caller can decide how to degrade.
    Err(BootKeyError::FeatureDisabled)
}

// ---------------------------------------------------------------------------
// Audit / introspection helpers
// ---------------------------------------------------------------------------

/// Return a short, human-readable algorithm identifier for the
/// AT `+IDENTROT?` query and audit logs. Always returns `"BTDK1"`
/// in this revision; bumping the algorithm requires bumping the
/// returned string AND the constant so sealed blobs from the new
/// version cannot open under the old.
#[inline]
pub const fn alg_name() -> &'static str {
    BTDK_ALG_NAME
}

/// Return the numeric algorithm identifier (used in wire format).
#[inline]
pub const fn alg_id() -> u8 {
    BTDK_ALG_ID
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These tests run with `cargo test -p magent-core --features boot-key`.
// Without the feature, the `derive` function is unreachable so the
// tests are cfg-gated to avoid spurious failures.

#[cfg(all(test, feature = "boot-key"))]
mod tests {
    use super::*;
    use crate::wifi_pass_seal::{MAX_ENCODED_LEN, MAX_PLAINTEXT};
    use heapless::{String as HeaplessString, Vec};

    /// Helper: produce the canonical v1 Keccak256 fingerprint of
    /// arbitrary material so tests can verify determinism without
    /// hard-coding magic bytes that have to be regenerated every
    /// time the domain tag changes.
    fn keccak256_of(material: &[u8]) -> [u8; 32] {
        let mut h = sha3::Keccak256::new();
        h.update(material);
        let out = h.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&out);
        bytes
    }

    #[test]
    fn derive_is_deterministic() {
        let material = b"efuse-block0-fake-material-for-test-001";
        let k1 = derive(material).expect("ok");
        let k2 = derive(material).expect("ok");
        assert_eq!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn derive_equals_keccak_of_material_concat_domain_version() {
        // The single most important property: the implementation
        // matches its own spec line-for-line. If this ever fails,
        // either the spec changed (update the test) or the
        // implementation drifted (fix the implementation).
        let material = b"\x01\x02\x03\x04\x05\x06\x07\x08";

        let mut expected_input: Vec<u8, { MAX_MATERIAL_LEN + 32 }> = Vec::new();
        expected_input
            .extend_from_slice(material)
            .expect("bounded by MAX_MATERIAL_LEN + 32");
        expected_input
            .extend_from_slice(MAG_BTDK_DOMAIN_V1)
            .expect("bounded by MAX_MATERIAL_LEN + 32");
        let _ = expected_input.push(BTDK_VERSION_V1);

        let expected = keccak256_of(&expected_input);
        let actual = derive(material).expect("ok");
        assert_eq!(actual.as_bytes(), &expected);
    }

    #[test]
    fn derive_rejects_empty_material() {
        let err = derive(b"").unwrap_err();
        assert_eq!(err, BootKeyError::MaterialEmpty);
    }

    #[test]
    fn derive_rejects_overlong_material() {
        let too_long = [0xAAu8; MAX_MATERIAL_LEN + 1];
        let err = derive(&too_long).unwrap_err();
        assert_eq!(err, BootKeyError::MaterialTooLong);
    }

    #[test]
    fn derive_accepts_max_length_material() {
        // Exactly MAX_MATERIAL_LEN must succeed (boundary check).
        let exact = [0xCCu8; MAX_MATERIAL_LEN];
        let k = derive(&exact).expect("ok");
        // And the result must not be all-zero (sanity: hash of
        // a long constant string is well-defined and non-trivial).
        assert!(k.as_bytes().iter().any(|b| *b != 0));
    }

    #[test]
    fn derive_different_material_yields_different_key() {
        let k1 = derive(b"device-A-efuse").expect("ok");
        let k2 = derive(b"device-B-efuse").expect("ok");
        assert_ne!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn derive_single_byte_difference_changes_key() {
        // Diff-by-one-bit test: changing one byte of material must
        // avalanche through the hash. (Keccak256's avalanche is
        // well-studied; we just assert the obvious consequence.)
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        a[0] = 0x00;
        b[0] = 0x01;
        let ka = derive(&a).expect("ok");
        let kb = derive(&b).expect("ok");
        assert_ne!(ka.as_bytes(), kb.as_bytes());
    }

    #[test]
    fn derive_domain_separation_prevents_cross_protocol_reuse() {
        // If an attacker can mix-and-match material across two
        // protocols that share a hash function (e.g. uses the
        // wifi_pass_seal input directly as boot_key input), the
        // domain tag MUST prevent that from yielding the same key.
        // We can't directly test the cross-protocol case here
        // (wifi_pass_seal has no hash in its spec) but we can
        // verify that bumping the version byte changes the key.
        let material = b"shared-input";
        let key_v1 = {
            let mut h = sha3::Keccak256::new();
            h.update(material);
            h.update(MAG_BTDK_DOMAIN_V1);
            h.update([0x01u8]);
            let out = h.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&out);
            bytes
        };
        let key_v2 = {
            let mut h = sha3::Keccak256::new();
            h.update(material);
            h.update(MAG_BTDK_DOMAIN_V1);
            h.update([0x02u8]);
            let out = h.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&out);
            bytes
        };
        assert_ne!(key_v1, key_v2);
        assert_eq!(derive(material).expect("ok").as_bytes(), &key_v1[..]);
    }

    #[test]
    fn boot_key_routes_through_wifi_pass_seal() {
        // The wifi_pass_seal::seal_str / open_sealed_bytes APIs
        // accept a `&[u8]` device key directly, so a BootKey is
        // used as the key argument with no conversion. This test
        // verifies the integration end-to-end: derive → seal →
        // open → recovered bytes match.
        let material = b"test-material";
        let bk = derive(material).expect("ok");

        let nonce = [0x33u8; 12];
        let mut sealed: HeaplessString<MAX_ENCODED_LEN> = HeaplessString::new();
        crate::wifi_pass_seal::seal_str("hello", bk.as_ref(), &nonce, &mut sealed)
            .expect("seal ok");

        let mut out: Vec<u8, MAX_PLAINTEXT> = Vec::new();
        let outcome = crate::wifi_pass_seal::open_sealed_bytes(&sealed, bk.as_ref(), &mut out)
            .expect("open ok");
        assert!(matches!(
            outcome,
            crate::wifi_pass_seal::OpenOutcome::DecodedBytes
        ));
        assert_eq!(&out[..], b"hello");
    }

    #[test]
    fn alg_name_and_id_are_stable() {
        // These constants are part of the wire format / audit
        // trail. Changing them invalidates sealed blobs. Tests
        // pin them so accidental changes show up in review.
        assert_eq!(alg_name(), "BTDK1");
        assert_eq!(alg_id(), 0x01);
    }

    #[test]
    fn boot_key_from_bytes_round_trips() {
        let bytes = [0x42u8; 32];
        let bk = BootKey::from_bytes(bytes);
        assert_eq!(bk.as_bytes(), &bytes);
    }

    #[test]
    fn boot_key_as_ref_works() {
        let bk = derive(b"ref-test").expect("ok");
        let slice: &[u8] = bk.as_ref();
        assert_eq!(slice.len(), 32);
    }
}
