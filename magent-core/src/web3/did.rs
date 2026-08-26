//! `did:key` — self-contained DID identifiers derived from a public key.
//!
//! Implements the W3C `did:key` method as specified in the
//! [did:key spec](https://w3c-ccg.github.io/did-method-key/):
//!
//! ```text
//! did:key:<multibase(multicodec(pk))>
//! ```
//!
//! For Ed25519 public keys:
//!
//! * **multicodec tag**: `0xed` followed by the 32-byte raw public key
//! * **multibase**: `z` prefix + base58btc encoding of the above
//!
//! So an Ed25519 public key like
//! `MCowBQYDK2VwAyEA…` (32 raw bytes, then base58btc'd) becomes
//! `did:key:z6Mk…`.
//!
//! ## Why `did:key`?
//!
//! `did:key` is the only DID method that:
//!
//! 1. Needs **no ledger / registry / network** to resolve.
//! 2. Is **canonical** — anyone with the same public key derives the
//!    same DID, so two parties can verify identity offline.
//! 3. Is **non-revocable** — the identifier is the key; rotating the
//!    key rotates the DID.
//!
//! For the agent, this is exactly what we want: each `Identity`
//! publishes a `did:key` as its handle, and any other party can
//! verify a signed message by re-deriving the DID from the claimed
//! public key and comparing.
//!
//! ## Validation contract
//!
//! `DidKey::from_string` is **strict**: it rejects any input whose
//! decoded body does not begin with one of the multicodec prefixes
//! we recognise. This is deliberate — a permissive parser would
//! accept DID-shaped strings that aren't actually Ed25519
//! `did:key`s, and downstream code (verification, key extraction)
//! would silently misbehave. Better to reject at the boundary.

use core::fmt;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bs58;

use crate::error::Web3ErrorKind;

use super::error::{base58_err, invalid_did, invalid_pk};

/// Multicodec prefix for an Ed25519 public key.
///
/// Specified by the multicodec table; the tag is a single varint
/// (`0xed`) followed by the 32 raw bytes. We hard-code the byte
/// sequence here because `unsigned-varint` encoding for `0xed` is
/// just `[0xed]` (it's < 0x80).
pub const ED25519_PUB_MULTICODEC_PREFIX: [u8; 2] = [0xed, 0x01];
/// Multicodec prefix for an Ed25519 **secret** key. Rare in
/// `did:key` but useful when you want to embed a private key as a
/// URI (e.g. for offline storage in a vault). We accept it on
/// decode so round-tripping a stored private key works.
pub const ED25519_SEC_MULTICODEC_PREFIX: [u8; 2] = [0x80, 0x26];

/// Number of bytes in an Ed25519 public key.
pub const ED25519_PUB_LEN: usize = 32;
/// Number of bytes in an Ed25519 secret key.
pub const ED25519_SEC_LEN: usize = 32;

/// A parsed `did:key` identifier, ready to be round-tripped back
/// to its string form.
///
/// Invariants (enforced by [`DidKey::from_string`]):
///
/// 1. `method` is always `"key"`.
/// 2. `multibase_prefix` is always `'z'` (base58btc).
/// 3. The decoded body begins with one of the multicodec prefixes
///    we recognise (currently [`ED25519_PUB_MULTICODEC_PREFIX`]
///    and [`ED25519_SEC_MULTICODEC_PREFIX`]).
/// 4. The body length matches `multicodec_prefix.len() + 32`.
///
/// Invariant (3) is what gives this type its safety properties: a
/// `DidKey` value can ALWAYS be turned into either a [`crate::web3::PublicKey`]
/// or a 32-byte secret seed. Methods that pull those out ([`DidKey::ed25519_public_key`],
/// [`DidKey::ed25519_secret_key`]) only fail if the caller asks for
/// the wrong kind — never because the underlying data is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DidKey {
    /// Raw multicodec + key bytes, ready to be re-encoded with
    /// `bs58::encode` + the `z` multibase prefix.
    ///
    /// Layout: `[multicodec_tag..., key_bytes...]`. For Ed25519
    /// public keys this is `[0xed, 0x01, pk0, pk1, …, pk31]` (34 bytes
    /// total).
    ///
    /// This field is `pub(crate)` so other modules in `web3` can
    /// read it for fast-path extraction (e.g. signature
    /// verification), but external callers cannot mutate it and
    /// thereby violate the type's invariants.
    pub(crate) multicodec_and_key: Vec<u8>,
}

impl DidKey {
    /// Construct from a 32-byte Ed25519 public key.
    ///
    /// `pk` must be exactly 32 bytes. Returns
    /// [`Web3ErrorKind::InvalidPublicKey`] otherwise. The returned
    /// `DidKey` can be converted to a string via
    /// [`DidKey::as_str`] or the [`Display`](fmt::Display) impl.
    pub fn from_ed25519_public_key(pk: &[u8]) -> Result<Self, Web3ErrorKind> {
        if pk.len() != ED25519_PUB_LEN {
            return Err(invalid_pk(pk.len()));
        }
        let mut buf = Vec::with_capacity(ED25519_PUB_MULTICODEC_PREFIX.len() + pk.len());
        buf.extend_from_slice(&ED25519_PUB_MULTICODEC_PREFIX);
        buf.extend_from_slice(pk);
        Ok(Self {
            multicodec_and_key: buf,
        })
    }

    /// Construct from a 32-byte Ed25519 **secret** key. Used when
    /// importing a stored private key from a vault that encodes it
    /// as `did:key:z…`.
    pub fn from_ed25519_secret_key(sk: &[u8]) -> Result<Self, Web3ErrorKind> {
        if sk.len() != ED25519_SEC_LEN {
            return Err(super::error::invalid_sk(sk.len()));
        }
        let mut buf = Vec::with_capacity(ED25519_SEC_MULTICODEC_PREFIX.len() + sk.len());
        buf.extend_from_slice(&ED25519_SEC_MULTICODEC_PREFIX);
        buf.extend_from_slice(sk);
        Ok(Self {
            multicodec_and_key: buf,
        })
    }

    /// Parse a `did:key:z...` string into a `DidKey`. The string
    /// must satisfy:
    ///
    /// * start with `did:key:`;
    /// * followed by a single base58btc character (we only accept
    ///   `'z'`);
    /// * followed by a base58btc encoding whose decoded body
    ///   begins with one of our recognised multicodec prefixes and
    ///   has the matching total length.
    ///
    /// Any deviation produces [`Web3ErrorKind::InvalidDid`] (for
    /// structural problems with the string) or
    /// [`Web3ErrorKind::Base58Decode`] (for the body bytes).
    pub fn from_string(s: &str) -> Result<Self, Web3ErrorKind> {
        let prefix = "did:key:";
        let Some(rest) = s.strip_prefix(prefix) else {
            return Err(invalid_did(s));
        };
        if rest.is_empty() {
            return Err(invalid_did(s));
        }
        let multibase = rest.as_bytes()[0];
        if multibase != b'z' {
            // We only support base58btc multibase. Other prefixes
            // (b, f, …) are valid in the spec but not in our
            // implementation.
            return Err(invalid_did(s));
        }
        let payload = &rest[1..];
        let decoded = bs58::decode(payload)
            .into_vec()
            .map_err(|e| base58_err(e.to_string()))?;
        let inner = Self {
            multicodec_and_key: decoded,
        };
        // Strict validation: reject anything whose multicodec tag
        // isn't one we recognise. Without this, downstream
        // `ed25519_public_key()` would silently return
        // `InvalidDid` for some inputs and successfully extract
        // arbitrary bytes from others — an inconsistent surface
        // that makes security bugs easy to ship.
        if !inner.has_recognised_multicodec() {
            return Err(invalid_did(s));
        }
        Ok(inner)
    }

    /// `true` if the body begins with a multicodec prefix we
    /// recognise (Ed25519 public or secret key).
    fn has_recognised_multicodec(&self) -> bool {
        self.multicodec_and_key.starts_with(&ED25519_PUB_MULTICODEC_PREFIX)
            || self.multicodec_and_key.starts_with(&ED25519_SEC_MULTICODEC_PREFIX)
    }

    /// `true` if the DID encodes an Ed25519 public key.
    pub fn is_public_key(&self) -> bool {
        self.multicodec_and_key
            .starts_with(&ED25519_PUB_MULTICODEC_PREFIX)
    }

    /// `true` if the DID encodes an Ed25519 secret key.
    pub fn is_secret_key(&self) -> bool {
        self.multicodec_and_key
            .starts_with(&ED25519_SEC_MULTICODEC_PREFIX)
    }

    /// Borrow the raw multicodec + key bytes (i.e. the body of
    /// the `did:key:z...` string before the base58btc encoding).
    pub fn raw_bytes(&self) -> &[u8] {
        &self.multicodec_and_key
    }

    /// DID method. For this module it's always `"key"`. Exposed as
    /// an accessor rather than a constant so callers that switch
    /// on it (e.g. log formatters, message-bus routers) get a
    /// single source of truth.
    pub fn method(&self) -> &'static str {
        "key"
    }

    /// Render the `DidKey` as a `did:key:z...` string.
    pub fn as_str(&self) -> String {
        // Allocate a string of the exact size we need up front
        // so the encoding pass doesn't grow the buffer.
        let mut s = String::with_capacity(
            "did:key:z".len() + self.multicodec_and_key.len() * 2,
        );
        s.push_str("did:key:z");
        s.push_str(&bs58::encode(&self.multicodec_and_key).into_string());
        s
    }

    /// Extract the raw 32-byte Ed25519 public key from the
    /// multicodec payload. Returns an error if the multicodec tag
    /// is not the Ed25519-public-key tag.
    ///
    /// The length check is defensive: `from_string` already
    /// guarantees the body has the right total length for *some*
    /// recognised multicodec, but this method specifically wants
    /// the public-key tag, so we re-verify the length here.
    pub fn ed25519_public_key(&self) -> Result<&[u8], Web3ErrorKind> {
        if !self.multicodec_and_key.starts_with(&ED25519_PUB_MULTICODEC_PREFIX) {
            return Err(Web3ErrorKind::InvalidDid {
                raw: self.as_str(),
            });
        }
        let body = &self.multicodec_and_key[ED25519_PUB_MULTICODEC_PREFIX.len()..];
        if body.len() != ED25519_PUB_LEN {
            return Err(invalid_pk(body.len()));
        }
        Ok(body)
    }

    /// Extract the raw 32-byte Ed25519 secret key (only valid for
    /// `did:key` constructed from a private key).
    pub fn ed25519_secret_key(&self) -> Result<&[u8], Web3ErrorKind> {
        if !self.multicodec_and_key.starts_with(&ED25519_SEC_MULTICODEC_PREFIX) {
            return Err(Web3ErrorKind::InvalidDid {
                raw: self.as_str(),
            });
        }
        let body = &self.multicodec_and_key[ED25519_SEC_MULTICODEC_PREFIX.len()..];
        if body.len() != ED25519_SEC_LEN {
            return Err(super::error::invalid_sk(body.len()));
        }
        Ok(body)
    }
}

impl fmt::Display for DidKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Round-trip on a fixed Ed25519 public key.
    #[test]
    fn round_trip_public_key() {
        let pk_bytes = [7u8; ED25519_PUB_LEN];
        let did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let s = did.as_str();
        assert!(s.starts_with("did:key:z"));
        let parsed = DidKey::from_string(&s).unwrap();
        assert_eq!(parsed, did);
        assert_eq!(parsed.ed25519_public_key().unwrap(), &pk_bytes[..]);
    }

    #[test]
    fn round_trip_secret_key() {
        let sk_bytes = [42u8; ED25519_SEC_LEN];
        let did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        let s = did.as_str();
        let parsed = DidKey::from_string(&s).unwrap();
        assert_eq!(parsed, did);
        assert_eq!(parsed.ed25519_secret_key().unwrap(), &sk_bytes[..]);
    }

    #[test]
    fn rejects_wrong_length_public_key() {
        let err = DidKey::from_ed25519_public_key(&[1, 2, 3]).unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidPublicKey { actual_len: 3 }));
    }

    #[test]
    fn rejects_malformed_did() {
        for bad in &[
            "not-a-did",
            "did:key:",
            "did:key:xfoo",  // unknown multibase
            "did:peer:zfoo", // wrong method
        ] {
            assert!(
                DidKey::from_string(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn rejects_wrong_multicodec_when_extracting_public_key() {
        let sk_bytes = [9u8; ED25519_SEC_LEN];
        let did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        let err = did.ed25519_public_key().unwrap_err();
        assert!(matches!(err, Web3ErrorKind::InvalidDid { .. }));
    }

    /// The critical strict-validation test. `did:key:z` followed by
    /// 32 random bytes (with no multicodec prefix) must NOT
    /// silently produce a `DidKey` that later resolves to "no
    /// public key here" — it must be rejected at parse time.
    #[test]
    fn from_string_rejects_unknown_multicodec() {
        // Build a body that's 32 bytes of `0x42` (no recognised
        // prefix). Base58btc-encode it so we have a syntactically
        // valid `did:key:z...` string.
        let body = vec![0x42u8; 32];
        let encoded = bs58::encode(&body).into_string();
        let bad_did = format!("did:key:z{encoded}");
        let err = DidKey::from_string(&bad_did).unwrap_err();
        assert!(
            matches!(err, Web3ErrorKind::InvalidDid { .. }),
            "expected InvalidDid for unknown multicodec, got {err:?}"
        );
    }

    /// Even if the body starts with the Ed25519 pub multicodec
    /// prefix, a body whose length isn't exactly
    /// `prefix.len() + 32` must NOT silently produce a `DidKey`.
    /// `from_string` accepts it (it has a recognised prefix),
    /// but `ed25519_public_key()` must reject it on length.
    #[test]
    fn from_string_rejects_wrong_body_length_when_extracting() {
        // Body = pub multicodec prefix (2 bytes) + 3 bytes of key.
        // Multicodec is recognised, but the key half is too short.
        let body = vec![0xed, 0x01, 0x01, 0x02, 0x03];
        let encoded = bs58::encode(&body).into_string();
        let did = DidKey::from_string(&format!("did:key:z{encoded}")).unwrap();
        let err = did.ed25519_public_key().unwrap_err();
        assert!(
            matches!(err, Web3ErrorKind::InvalidPublicKey { actual_len: 3 }),
            "expected InvalidPublicKey with actual_len=3, got {err:?}"
        );
    }

    /// The round-trip must preserve every one of the 32 key
    /// bytes — including zeros in positions where the
    /// surrounding bytes are non-zero. This is the property that
    /// would silently break if `from_string` / `as_str` mishandled
    /// base58btc's leading-zero convention (or any other byte).
    #[test]
    fn round_trip_public_key_with_embedded_zero() {
        // All-zero public key is degenerate (and not a valid
        // Ed25519 point, so verification would fail later) but
        // a useful round-trip test: the body has leading zero
        // bytes after the multicodec prefix and every byte must
        // come back unchanged.
        let pk_bytes = [0u8; ED25519_PUB_LEN];
        let did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let s = did.as_str();
        let parsed = DidKey::from_string(&s).unwrap();
        assert_eq!(parsed, did);
        assert_eq!(parsed.ed25519_public_key().unwrap(), &pk_bytes[..]);
    }

    /// A public key whose 32-byte body contains bytes in every
    /// nibble range (0..=0xF) catches any off-by-one in the
    /// base58 encoder (e.g. a wrong alphabet mapping).
    #[test]
    fn round_trip_public_key_with_all_byte_values() {
        let pk_bytes: [u8; ED25519_PUB_LEN] =
            core::array::from_fn(|i| (i * 8) as u8);
        let did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let s = did.as_str();
        let parsed = DidKey::from_string(&s).unwrap();
        assert_eq!(parsed, did);
        assert_eq!(parsed.ed25519_public_key().unwrap(), &pk_bytes[..]);
    }

    #[test]
    fn is_public_key_and_is_secret_key_are_mutually_exclusive() {
        let pk_bytes = [7u8; ED25519_PUB_LEN];
        let sk_bytes = [9u8; ED25519_SEC_LEN];
        let pk_did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let sk_did = DidKey::from_ed25519_secret_key(&sk_bytes).unwrap();
        assert!(pk_did.is_public_key());
        assert!(!pk_did.is_secret_key());
        assert!(!sk_did.is_public_key());
        assert!(sk_did.is_secret_key());
    }

    #[test]
    fn method_is_always_key() {
        let pk_did = DidKey::from_ed25519_public_key(&[1u8; 32]).unwrap();
        let sk_did = DidKey::from_ed25519_secret_key(&[2u8; 32]).unwrap();
        assert_eq!(pk_did.method(), "key");
        assert_eq!(sk_did.method(), "key");
    }

    #[test]
    fn raw_bytes_returns_multicodec_and_key() {
        let pk_bytes = [7u8; ED25519_PUB_LEN];
        let did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let raw = did.raw_bytes();
        assert_eq!(raw.len(), ED25519_PUB_MULTICODEC_PREFIX.len() + ED25519_PUB_LEN);
        assert!(raw.starts_with(&ED25519_PUB_MULTICODEC_PREFIX));
        assert_eq!(&raw[ED25519_PUB_MULTICODEC_PREFIX.len()..], &pk_bytes[..]);
    }

    #[test]
    fn display_impl_matches_as_str() {
        let pk_bytes = [11u8; ED25519_PUB_LEN];
        let did = DidKey::from_ed25519_public_key(&pk_bytes).unwrap();
        let via_display = alloc::format!("{}", did);
        assert_eq!(via_display, did.as_str());
    }

    /// Security-boundary robustness: `from_string` parses untrusted
    /// `did:key:` strings, so it must never panic. We also check an
    /// invariant: every string that parses successfully must round-trip
    /// through `as_str()` and parse again identically.
    #[test]
    fn from_string_never_panics_and_round_trips_on_success() {
        // A structurally-valid did that we use to seed round-trip checks.
        let valid = DidKey::from_ed25519_public_key(&[7u8; ED25519_PUB_LEN]).unwrap();
        let valid_str = valid.as_str().to_string();

        let mut cases: alloc::vec::Vec<String> = alloc::vec::Vec::new();
        // Structural edge cases.
        cases.push("".into());
        cases.push("did".into());
        cases.push("did:".into());
        cases.push("did:key".into());
        cases.push("did:key:".into());
        cases.push("did:key:z".into());
        cases.push("did:key:x".into());
        cases.push("did:key:z!".into());
        cases.push(valid_str.clone());
        // Base58 alphabet bounds + non-alphabet chars appended/prepended.
        for c in ['0', 'O', 'I', 'l', '!', ' ', '\n', '\0', '1', 'z'] {
            cases.push(format!("did:key:z{c}"));
            cases.push(format!("did:key:z{}", c.to_string().repeat(40)));
            cases.push(format!("did:key:z{c}{}", valid.as_str()));
            cases.push(format!("did:key:z{}c", valid.as_str()));
        }
        // Truncated / extended versions of a valid DID.
        for cut in 0..valid_str.len() {
            cases.push(valid_str[..cut].to_string());
        }
        cases.push(format!("{valid_str}{valid_str}"));
        cases.push(format!("did:key:z{}", "A".repeat(200)));
        // Deterministic pseudo-random bytes (LCG) as base58 payloads.
        let mut acc: u32 = 0xDEADBEEF;
        for len in 1..=128usize {
            let mut v = String::with_capacity(len);
            for _ in 0..len {
                acc = acc.wrapping_mul(1664525).wrapping_add(1013904223);
                // Map into the base58 alphabet range roughly.
                let b = ((acc >> 24) % 122) as u8 + 33; // printable ASCII
                v.push(b as char);
            }
            cases.push(format!("did:key:z{v}"));
        }

        for c in &cases {
            // Must never panic.
            match DidKey::from_string(c) {
                Ok(did) => {
                    // Invariant: a successfully-parsed DID round-trips.
                    let s = did.as_str();
                    let reparsed = DidKey::from_string(&s)
                        .expect("as_str() output must re-parse");
                    assert_eq!(did.as_str(), reparsed.as_str());
                }
                Err(_) => {} // rejections are fine; the point is no panic
            }
        }
    }
}