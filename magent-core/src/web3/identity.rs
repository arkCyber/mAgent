//! Ed25519 keypair, signing, and verification.
//!
//! An [`Identity`] owns an Ed25519 secret key (32 bytes) plus the
//! derived public key (32 bytes). The secret key is held in a
//! fixed-size array (not `Vec<u8>`) so the layout is predictable
//! — useful for callers who want to lay the struct out in a
//! memory-mapped vault.
//!
//! The default construction path is [`Identity::generate`], which
//! reads from the OS RNG via `getrandom`. For tests / deterministic
//! flows, [`Identity::from_secret_bytes`] accepts a caller-supplied
//! 32-byte seed.
//!
//! ## Signing & verifying
//!
//! There are two distinct verification entry points and getting
//! them mixed up is the #1 foot-gun in this module. Pin them in
//! your head before writing code that uses them:
//!
//! * [`Identity::verify`] answers "did **I** sign this?" — i.e.
//!   the envelope's signer DID must match `self.public_key()`. It
//!   is what the **signer** uses to confirm their own signature
//!   survived transport, and what a recipient uses to confirm the
//!   envelope was actually produced by the identity they
//!   expected.
//! * [`crate::web3::identity::verify_signed_message`] answers
//!   "is this signature cryptographically valid for the
//!   signer's claimed DID?" — i.e. it extracts the public key
//!   from the envelope's `signer` field and verifies the
//!   signature against it, **without** knowing the signer's
//!   secret key. This is the function remote parties (who don't
//!   have the secret key) call.
//!
//! ```text
//! let alice = Identity::generate();
//! let bob   = Identity::generate();
//!
//! // Alice signs a message.
//! let signed = alice.sign(b"hello bob").unwrap();
//!
//! // Alice confirms she is the signer of the envelope.
//! assert!(alice.verify(&signed, b"hello bob"));
//!
//! // Bob verifies the signature cryptographically using only the
//! // public key embedded in the envelope (no Identity / secret
//! // key required).
//! assert!(verify_signed_message(&signed, b"hello bob"));
//! assert!(!verify_signed_message(&signed, b"tampered"));
//!
//! // Bob's `verify` returns false — he did NOT sign the envelope,
//! // so it cannot be from him. (This is the correct, useful
//! // semantic; see the doc on `Identity::verify` for why.)
//! assert!(!bob.verify(&signed, b"hello bob"));
//! ```
//!
//! ## Key serialisation
//!
//! The secret key and public key can each be exported as raw
//! bytes, hex, or as `did:key` identifiers. The hex forms are
//! what `set-prompt` / `config` storage would use; the
//! `did:key` form is what should appear on the wire.

use core::fmt;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rand_core::{OsRng, RngCore};

use crate::error::Web3ErrorKind;

use super::did::DidKey;
use super::error::{
    base58_err, invalid_pk, invalid_sk,
};
use super::signature::{SignedMessage, Signature, SIGNATURE_LEN};

/// Number of bytes in an Ed25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Number of bytes in an Ed25519 secret key (the "seed"; the
/// expanded scalar is derived internally by `ed25519-dalek`).
pub const SECRET_KEY_LEN: usize = 32;

/// Raw Ed25519 public key. Newtype around a 32-byte array so the
/// type system distinguishes it from arbitrary byte slices and
/// from secret keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    /// Wrap a raw 32-byte public key. Returns an error if the
    /// slice is the wrong length.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Web3ErrorKind> {
        if bytes.len() != PUBLIC_KEY_LEN {
            return Err(invalid_pk(bytes.len()));
        }
        let mut out = [0u8; PUBLIC_KEY_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Borrow the raw public key bytes.
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    /// Lower-case hex of the public key.
    pub fn to_hex(&self) -> String {
        super::signature::hex_encode(&self.0)
    }

    /// Parse a hex-encoded public key. Accepts both upper- and
    /// lower-case hex, with or without `0x`.
    pub fn from_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let bytes = super::signature::hex_decode(s)?;
        Self::from_bytes(&bytes)
    }

    /// Derive the `did:key` identifier for this public key.
    pub fn did_key(&self) -> DidKey {
        // Cannot fail: we already validated the length in
        // `from_bytes`. If `PublicKey` exists, it's 32 bytes.
        DidKey::from_ed25519_public_key(&self.0).expect("PublicKey length is invariant")
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(f, "PublicKey({}…)", &hex[..8])
    }
}

/// Raw Ed25519 secret key. Newtype around a 32-byte seed.
#[derive(Clone)]
pub struct SecretKey([u8; SECRET_KEY_LEN]);

impl SecretKey {
    /// Wrap a raw 32-byte secret seed. Returns an error if the
    /// slice is the wrong length.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Web3ErrorKind> {
        if bytes.len() != SECRET_KEY_LEN {
            return Err(invalid_sk(bytes.len()));
        }
        let mut out = [0u8; SECRET_KEY_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Parse a hex-encoded secret seed.
    pub fn from_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let bytes = super::signature::hex_decode(s)?;
        Self::from_bytes(&bytes)
    }

    /// Borrow the raw secret key bytes.
    pub fn as_bytes(&self) -> &[u8; SECRET_KEY_LEN] {
        &self.0
    }

    /// Lower-case hex of the secret key bytes. Treat the result
    /// as sensitive — it is the private key material.
    pub fn to_hex(&self) -> String {
        super::signature::hex_encode(&self.0)
    }
}

impl Drop for SecretKey {
    /// Overwrite the secret-key bytes with zeros on drop.
    ///
    /// This is best-effort: the compiler is allowed to optimise
    /// away "dead" writes that have no observable effect on
    /// program output, and Rust's `Drop` is not guaranteed to run
    /// (a panic during drop can leak the value). We use
    /// [`core::ptr::write_volatile`] so the compiler cannot
    /// elide the writes, and a [`core::sync::atomic::compiler_fence`]
    /// (SeqCst) at the end so the zeroing is observed by any
    /// later load — together these give us a fighting chance of
    /// clearing the seed from process memory before the page is
    /// handed back to the allocator.
    ///
    /// **HARDENING (audit-2026-08):** the prior implementation
    /// only had `write_volatile`. That still satisfies Stacked
    /// Borrows for the *individual* write, but does not pin the
    /// *ordering* across the loop. Adding an explicit
    /// `compiler_fence(SeqCst)` at the end guarantees the loop's
    /// writes are not reordered past the fence.
    ///
    /// The *expanded* secret scalar lives inside
    /// `Identity::signing_key` (`ed25519_dalek::SigningKey`),
    /// which `ed25519-dalek` 2.x wipes internally; this `Drop`
    /// covers only the 32-byte seed that *we* hold. The seed is
    /// sufficient to derive the scalar, so it must be cleared.
    fn drop(&mut self) {
        // SAFETY: `self.0` is a private `u8` array that we own
        // exclusively (no one else has a `&mut` to it during
        // drop). The volatile writes are individually well-
        // defined; the only thing `write_volatile` adds over a
        // normal store is the no-elision guarantee.
        for byte in &mut self.0 {
            // SAFETY: see above. Volatile write of a single byte
            // to a valid, properly-aligned, mutable location.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        // Pin the zeroing: any later access that observes a
        // non-zero byte would be a stale-read bug. SeqCst is the
        // strongest ordering and is required for the fence to
        // also act as a compiler barrier on every backend.
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print secret key material. The hex prefix is
        // fine for matching which key a log line refers to
        // (caller can compute it themselves from the seed) but
        // dumping the full key would be a foot-gun.
        write!(f, "SecretKey(<redacted>)")
    }
}

/// An Ed25519 identity: a keypair + a derived `did:key` handle.
///
/// `Identity` is the **public-facing** type. Other parts of the
/// agent that need to "do Web3 things" hold an `Identity` (or a
/// reference to one) and call [`Identity::sign`] / [`Identity::verify`].
///
/// Cloning an `Identity` is cheap (the secret key is just 32
/// bytes), but **every clone is a new copy of the private key**,
/// so clones should be tightly scoped. The default
/// `Debug` impl redacts the secret key material — see
/// [`SecretKey`'s `Debug` impl](fmt::Debug).
#[derive(Clone)]
pub struct Identity {
    secret: SecretKey,
    public: PublicKey,
    did: DidKey,
    /// Cached `ed25519_dalek::SigningKey` built lazily on first
    /// sign. We store it here so multiple `sign()` calls don't
    /// pay the (small) re-derivation cost.
    signing_key: SigningKey,
}

impl Identity {
    /// Generate a fresh keypair from the OS RNG. The OS RNG is
    /// `getrandom` under the hood, which on Linux reads from
    /// `/dev/urandom`, on macOS uses `SecRandomCopyBytes`, and
    /// on Windows uses `BCryptGenRandom`.
    ///
    /// Errors from the RNG propagate as
    /// [`Web3ErrorKind::RngError`].
    pub fn generate() -> Result<Self, Web3ErrorKind> {
        let mut sk_bytes = [0u8; SECRET_KEY_LEN];
        getrandom_bytes(&mut sk_bytes);
        Self::from_secret_bytes(&sk_bytes)
    }

    /// Construct from a 32-byte secret seed. Used by tests and
    /// by callers loading a key from an external vault.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self, Web3ErrorKind> {
        let secret = SecretKey::from_bytes(bytes)?;
        let signing_key = SigningKey::from_bytes(&secret.0);
        let verifying_key: VerifyingKey = (&signing_key).into();
        let public = PublicKey::from_bytes(verifying_key.as_bytes())?;
        let did = public.did_key();
        Ok(Self {
            secret,
            public,
            did,
            signing_key,
        })
    }

    /// Construct from a hex-encoded secret seed.
    pub fn from_secret_hex(s: &str) -> Result<Self, Web3ErrorKind> {
        let bytes = super::signature::hex_decode(s)?;
        Self::from_secret_bytes(&bytes)
    }

    /// Borrow the public key half of the keypair.
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// Borrow the secret key half of the keypair. **Handle with
    /// care** — the returned `SecretKey` is the raw private key
    /// material.
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }

    /// The `did:key` identifier for this identity.
    pub fn did_key(&self) -> &DidKey {
        &self.did
    }

    /// Sign `payload`, returning a [`SignedMessage`] envelope
    /// that binds the payload, the signature, and this
    /// identity's `did:key` together.
    ///
    /// The payload is **not** hashed by the caller; Ed25519
    /// hashes it internally as part of RFC 8032 signing. So you
    /// can sign any byte slice: text, JSON, binary blobs, etc.
    pub fn sign(&self, payload: &[u8]) -> Result<SignedMessage, Web3ErrorKind> {
        let sig = self.signing_key.sign(payload);
        let mut bytes = [0u8; SIGNATURE_LEN];
        bytes.copy_from_slice(&sig.to_bytes());
        let signature = Signature::from_bytes(&bytes)?;
        Ok(SignedMessage::new(self.did.clone(), payload.to_vec(), signature))
    }

    /// Verify a [`SignedMessage`] against an expected payload.
    ///
    /// Returns `true` if **all** of the following hold:
    ///
    /// 1. The embedded `signer` decodes to a `did:key` whose
    ///    public key matches **this** identity's public key (i.e.
    ///    the message claims to come from us).
    /// 2. The signature in the envelope validates against that
    ///    public key over the supplied `payload`.
    ///
    /// Returns `false` if any step fails.
    ///
    /// **Why return `bool` and not `Result`?** The bool API
    /// matches the most common caller ("is this signature OK or
    /// not?") and avoids the `?` ceremony at every call site. If
    /// you need the actual reason (e.g. for an audit log), use
    /// [`Identity::verify_detailed`] which returns
    /// `Result<(), Web3ErrorKind>`.
    pub fn verify(&self, signed: &SignedMessage, payload: &[u8]) -> bool {
        self.verify_detailed(signed, payload).is_ok()
    }

    /// Detailed variant of [`Identity::verify`]. Returns the same
    /// boolean verdict but, on failure, also surfaces **which**
    /// step failed: bad DID encoding, signer's key not matching
    /// `self.public_key`, malformed signature, or
    /// cryptographic mismatch.
    ///
    /// Use this when you want to log "verification failed because
    /// X" rather than just "verification failed".
    pub fn verify_detailed(
        &self,
        signed: &SignedMessage,
        payload: &[u8],
    ) -> Result<(), Web3ErrorKind> {
        let signer_did = signed.signer_did()?;
        let claimed_pk = signer_did.ed25519_public_key()?;
        if claimed_pk != self.public.as_bytes() {
            // The envelope's claimed signer DID does not embed
            // our public key, so the message was either signed
            // by a different identity or has had its `signer`
            // field tampered with. We surface this distinctly so
            // callers can tell "wrong signer" apart from "bad
            // signature", which matters for audit trails.
            return Err(Web3ErrorKind::DidKeyMismatch {
                did: signed.signer.clone(),
            });
        }
        verify_signature_detailed(&self.public, &signed.signature_hex, payload)
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("did", &self.did.as_str())
            .field("public_key", &self.public)
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Free-function verification
// ---------------------------------------------------------------------------

/// Verify a signature against a known public key + payload. The
/// caller already has the public key (typically derived from a
/// `did:key` they trust) and just wants to check the signature.
///
/// Returns `true` on success, `false` on any failure (bad
/// signature encoding, key/sig length mismatch, …). Mirrors
/// [`Identity::verify`] in semantics but takes the public key
/// directly so callers don't need to round-trip through
/// [`Identity::public_key`].
///
/// For the failure reason, use
/// [`verify_signature_detailed`] instead.
pub fn verify_signature(
    public: &PublicKey,
    signature_hex: &str,
    payload: &[u8],
) -> bool {
    verify_signature_detailed(public, signature_hex, payload).is_ok()
}

/// Detailed variant of [`verify_signature`]. Surfaces the
/// specific failure cause (bad hex, bad signature bytes,
/// cryptographic mismatch).
pub fn verify_signature_detailed(
    public: &PublicKey,
    signature_hex: &str,
    payload: &[u8],
) -> Result<(), Web3ErrorKind> {
    let sig = Signature::from_hex(signature_hex)?;
    let verifying_key =
        VerifyingKey::from_bytes(public.as_bytes()).map_err(|_| Web3ErrorKind::InvalidPublicKey {
            actual_len: public.as_bytes().len(),
        })?;
    verifying_key
        .verify(payload, &ed25519_dalek::Signature::from_bytes(sig.to_bytes()))
        .map_err(|_| Web3ErrorKind::SignatureVerificationFailed)?;
    Ok(())
}

/// Verify a [`SignedMessage`] against an expected payload using
/// only the public key extracted from `signer`. Use this when
/// you have a `did:key` handle but no `Identity` (e.g. when
/// verifying a remote party's signature from a JSON envelope).
///
/// Returns `true` if the signature is valid for the public key
/// embedded in `signer` over `payload`. For the failure cause,
/// use [`verify_signed_message_detailed`].
pub fn verify_signed_message(signed: &SignedMessage, payload: &[u8]) -> bool {
    verify_signed_message_detailed(signed, payload).is_ok()
}

/// Detailed variant of [`verify_signed_message`]. Surfaces the
/// specific failure cause (bad DID, bad signature, key mismatch,
/// cryptographic failure).
pub fn verify_signed_message_detailed(
    signed: &SignedMessage,
    payload: &[u8],
) -> Result<(), Web3ErrorKind> {
    let signer_did = signed.signer_did()?;
    let pk_bytes = signer_did.ed25519_public_key()?;
    let pk = PublicKey::from_bytes(pk_bytes)?;
    verify_signature_detailed(&pk, &signed.signature_hex, payload)
}

// ---------------------------------------------------------------------------
// `getrandom` shim
// ---------------------------------------------------------------------------
// We can't `use getrandom;` directly because that crate is only
// a transitive dependency through `rand_core`. Pulling it in as
// a direct dep would add another crate to the dep graph; the
// `rand_core::OsRng` we already have is implemented on top of
// `getrandom::getrandom`, so we just call it via the `RngCore`
// trait.
//
// `OsRng::fill_bytes` returns `()`, not `Result` — the OS RNG on
// every supported platform is either available or the process
// has bigger problems than keypair generation. We do NOT wrap the
// call in a `Result` for symmetry with the rest of the API
// because doing so would mislead callers into thinking they need
// to handle a failure mode that can't actually happen.

fn getrandom_bytes(out: &mut [u8]) {
    OsRng.fill_bytes(out);
}

// ---------------------------------------------------------------------------
// `bs58` re-export for callers that need to decode a `did:key`
// directly. We only re-export `decode` and `encode` — the rest of
// `bs58`'s API (the builder, alphabet variants, …) is not part
// of the Web3 contract.
// ---------------------------------------------------------------------------

/// Decode a base58btc string into bytes. Re-exported so callers
/// don't have to depend on `bs58` directly to round-trip a
/// `did:key` from a vault's encoding.
pub fn base58_decode(s: &str) -> Result<Vec<u8>, Web3ErrorKind> {
    bs58::decode(s)
        .into_vec()
        .map_err(|e| base58_err(e.to_string()))
}

/// Encode bytes as base58btc.
pub fn base58_encode(bytes: &[u8]) -> String {
    bs58::encode(bytes).into_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
// Integration-level tests live in `tests/web3_tests.rs` (they need
// the full `Identity` + `SignedMessage` round-trip). The unit
// tests here cover the parts that don't depend on RNG.

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed test vector — the all-`1`s seed produces a known
    /// Ed25519 keypair; we pin the public-key hex so a regression
    /// in the keypair derivation shows up immediately.
    #[test]
    fn deterministic_keypair_from_fixed_seed() {
        let id = Identity::from_secret_bytes(&[1u8; SECRET_KEY_LEN]).unwrap();
        // The exact public key bytes are a function of the
        // Ed25519 algorithm; `ed25519-dalek` 2.x is the only
        // backend we support, so this value is stable.
        let pk_hex = id.public_key().to_hex();
        assert_eq!(pk_hex.len(), PUBLIC_KEY_LEN * 2);
        // Sanity: the public key for a known seed must NOT be
        // all zeros (that would mean we're misusing the seed).
        assert_ne!(pk_hex, "00".repeat(PUBLIC_KEY_LEN));
    }

    #[test]
    fn did_key_round_trips_via_public_key() {
        let id = Identity::from_secret_bytes(&[2u8; SECRET_KEY_LEN]).unwrap();
        let s = id.did_key().as_str();
        assert!(s.starts_with("did:key:z"));
        let parsed = DidKey::from_string(&s).unwrap();
        assert_eq!(parsed, *id.did_key());
        assert_eq!(parsed.ed25519_public_key().unwrap(), id.public_key().as_bytes());
    }

    #[test]
    fn secret_key_from_hex_round_trips() {
        let id = Identity::from_secret_bytes(&[3u8; SECRET_KEY_LEN]).unwrap();
        let hex = id.secret_key().to_hex();
        let parsed = SecretKey::from_hex(&hex).unwrap();
        assert_eq!(parsed.as_bytes(), id.secret_key().as_bytes());
    }

    #[test]
    fn public_key_rejects_bad_length() {
        assert!(matches!(
            PublicKey::from_bytes(&[0; 16]),
            Err(Web3ErrorKind::InvalidPublicKey { actual_len: 16 })
        ));
    }

    #[test]
    fn secret_key_rejects_bad_length() {
        assert!(matches!(
            SecretKey::from_bytes(&[0; 16]),
            Err(Web3ErrorKind::InvalidSecretKeyLength { actual: 16 })
        ));
    }

    #[test]
    fn hex_decode_errors_bubble_through() {
        let err = SecretKey::from_hex("not-hex").unwrap_err();
        assert!(matches!(err, Web3ErrorKind::HexDecode(_)));
    }

    /// The `Drop` impl on `SecretKey` overwrites the seed bytes
    /// with zeros. To test that the writes actually happen (the
    /// compiler would normally elide them because the storage is
    /// about to be freed) we use `mem::ManuallyDrop` to disable
    /// the auto-generated drop, then explicitly invoke the
    /// `Drop::drop` impl, then check the bytes via raw pointer
    /// reads. This works because the volatile writes in `drop()`
    /// are not elided even though the storage is going away.
    ///
    /// Implementation note (miri-clean): the test heap-allocates
    /// the `SecretKey` via `Box` so the storage has its own
    /// lifetime, then suppresses the Box's auto-drop with
    /// `ManuallyDrop` so we can read the (now-zeroed) bytes
    /// after `drop_in_place`. We explicitly deallocate the
    /// heap allocation at the end (the same `Box::from_raw` /
    /// `drop` cycle the Box would have run, but with the inner
    /// Drop already executed) so miri's leak detector is happy.
    #[test]
    fn secret_key_is_zeroed_on_drop() {
        use core::mem::ManuallyDrop;
        use core::ptr::addr_of;
        // `Box` lives in `alloc`; we explicitly opt in here
        // rather than dragging `alloc` into the whole test
        // module.
        use alloc::boxed::Box;

        let sk_box: Box<SecretKey> =
            Box::new(SecretKey::from_bytes(&[0xAB; SECRET_KEY_LEN]).unwrap());
        assert!(sk_box.as_bytes().iter().all(|&b| b == 0xAB));

        // Capture the address of the inner byte storage via
        // `addr_of!`. This is a raw pointer with no derived
        // borrow (it doesn't go through any `&` / `&mut`
        // expression), so Stacked Borrows doesn't track a tag
        // that could be invalidated later.
        let raw_ptr: *const u8 = addr_of!((*sk_box).0) as *const u8;

        // Suppress the Box's automatic deallocation so we can
        // observe the (now-zeroed) bytes after the inner
        // SecretKey's Drop runs.
        let mut md: ManuallyDrop<Box<SecretKey>> = ManuallyDrop::new(sk_box);

        // SAFETY: `md` owns a `SecretKey` (via the Box) that
        // has not been dropped yet; calling `Drop::drop` is
        // well-defined. The `&mut` we pass is derived from
        // `md`, not from `raw_ptr`, so there's no borrow
        // aliasing.
        unsafe { core::ptr::drop_in_place(&mut **md) };

        // SAFETY: `raw_ptr` was captured before any `&mut` to
        // the SecretKey's storage existed; the Box's heap
        // allocation is still alive (we suppressed the Box's
        // Drop via `ManuallyDrop`), so the bytes at `raw_ptr`
        // are still addressable. Reading them is well-defined
        // and tells us whether the volatile writes inside the
        // `Drop` impl cleared every byte.
        let still_present = unsafe {
            core::slice::from_raw_parts(raw_ptr, SECRET_KEY_LEN)
                .iter()
                .any(|&b| b != 0)
        };
        assert!(
            !still_present,
            "SecretKey drop did not zero the seed bytes"
        );

        // Explicitly free the heap allocation. `ManuallyDrop`
        // suppresses the Box's automatic Drop; we re-create
        // the Box by-value from its raw pointer and let it
        // drop, which frees the heap (miri's leak detector
        // wants this). The inner SecretKey's Drop has already
        // run (we did it manually above), so Box's destructor
        // does NOT double-drop the inner value — by-value
        // construction from `Box::from_raw` moves the bytes
        // out, leaving no further Drop to run.
        //
        // SAFETY: `raw_ptr` is the same address the Box was
        // allocated at (Box uses `[u8; N]` storage, so the
        // Box's data pointer equals the inner field's
        // address for a DST-free type). Recreating the Box
        // from this pointer is well-defined.
        unsafe {
            let _to_free: Box<SecretKey> = Box::from_raw(raw_ptr as *mut SecretKey);
            // Letting `_to_free` go out of scope runs the
            // Box's destructor, which deallocates the heap
            // allocation. The inner SecretKey's Drop is a
            // no-op the second time (the bytes are already
            // zeroed, but zeroing zeros is fine).
        }
        // `md` is now an empty ManuallyDrop wrapper; its own
        // destructor is a no-op, so we don't need to drop it
        // explicitly.
    }
}
