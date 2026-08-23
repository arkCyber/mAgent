//! `magent_core::web3` — Web3 identity & asymmetric cryptography.
//!
//! This module provides everything the agent needs to participate in a
//! Web3 identity layer without pulling in any external crate's
//! `reqwest` / `tokio` / chain-client machinery:
//!
//! * **Ed25519 keypairs** ([`identity::Identity`], generated from a
//!   cryptographically secure RNG).
//! * **Detached signatures** ([`signature::Signature`], 64-byte RFC 8032
//!   Ed25519 signatures serialised as raw bytes or hex).
//! * **Self-contained `did:key` identifiers** ([`did::DidKey`], derived
//!   from the Ed25519 public key via the W3C `did:key` method +
//!   multibase `z` prefix + multicodec `0xed` for Ed25519 public keys).
//! * **Signed-message envelopes** ([`signature::SignedMessage`]) that
//!   bind a payload, its signature, and the signer's `did:key` together
//!   for transport over JSON, mailbox, or whatever wire format the
//!   agent speaks.
//!
//! ## Wire-level example
//!
//! ```
//! use magent_core::web3::Identity;
//!
//! let alice = Identity::generate().unwrap();
//! let bob   = Identity::generate().unwrap();
//!
//! let signed = alice.sign(b"hello, bob").unwrap();
//! // `alice.verify` succeeds because the envelope claims to be
//! // from alice (the signer).
//! assert!(alice.verify(&signed, b"hello, bob"));
//! // Anyone with alice's public key can also verify using the
//! // free function — no `Identity` needed.
//! assert!(magent_core::web3::verify_signed_message(&signed, b"hello, bob"));
//! ```
//!
//! ## JSON envelope example
//!
//! For shipping signed records over JSON (mailbox payloads,
//! audit-log entries, summary files, …) the canonical wire form
//! is:
//!
//! ```json
//! {
//!   "signer": "did:key:z6Mk…",
//!   "payload_hex": "68656c6c6f2c20626f62",
//!   "signature_hex": "9a1f…"
//! }
//! ```
//!
//! The raw `payload` `Vec<u8>` is intentionally **NOT** in the
//! JSON — only the hex-encoded `payload_hex` is. This is the
//! same convention used by libp2p / IPFS `did:key`-style
//! envelopes; hex is the lowest-common-denominator encoder that
//! every language can decode without pulling in a base64 crate.
//!
//! ```rust
//! # #[cfg(feature = "web3")]
//! # {
//! use magent_core::web3::{Identity, SignedMessage};
//!
//! let alice = Identity::from_secret_bytes(&[7u8; 32]).unwrap();
//! let audit_record = b"{\"event\":\"delete\",\"path\":\"/tmp/x\"}";
//!
//! // Alice's side: produce a signed envelope and serialise.
//! let signed = alice.sign(audit_record).unwrap();
//! let json = signed.to_json();
//! // e.g. `{"signer":"did:key:z6Mk…","payload_hex":"…","signature_hex":"…"}`
//!
//! // Bob's side: receive the JSON and verify it without ever
//! // touching Alice's secret key. `from_json` decodes
//! // `payload_hex` back into `payload_bytes` for the verifier.
//! let parsed = SignedMessage::from_json(&json).unwrap();
//! assert!(magent_core::web3::verify_signed_message(&parsed, audit_record));
//! # }
//! ```
//!
//! ## Feature flag
//!
//! The module is gated behind `magent-core`'s `web3` feature, which in
//! turn requires `std` (we use `getrandom` for keypair generation).
//! Embedded (`no_std`) builds do NOT pull in `ed25519-dalek` or
//! `bs58` and keep their flash budget.
//!
//! ```text
//! cargo test -p magent-core --features web3,std
//! ```
//!
//! ## What this module deliberately does NOT do
//!
//! * It does not talk to any blockchain. There is no RPC client,
//!   no transaction builder, no wallet keystore. mAgent's identity
//!   layer is purely about *cryptographic identity* — proving "this
//!   message came from this DID" — not about on-chain state.
//!
//! * It does not implement JWS / JWT / X.509. The `SignedMessage`
//!   envelope is bespoke (`{ signer, payload, signature }`) and is
//!   meant to be embedded inside the agent's own JSON envelopes
//!   (mailbox payloads, summary records, audit log entries, …).
//!   If you need JWS, compose it on top of [`identity::Identity`]
//!   and [`signature::Signature`].
//!
//! * It does not store or persist private keys. Generation produces
//!   an in-memory `Identity`; serialisation (`to_hex` / `from_hex`)
//!   is provided so the caller can persist it via whatever
//!   key-storage facility they already use (the agent's secure
//!   element, the host keyring, an HSM, …).

pub mod did;
pub mod error;
pub mod identity;
pub mod signature;

// Verifiable credentials (VC) module — gated on the dedicated
// `verifiable_credentials` Cargo feature so consumers that only want
// raw Ed25519/did:key plumbing don't pay for the VC schema enums.
// The feature pulls in `web3` (it uses Ed25519 `Signature` and
// `web3::Error`) + `std` (for chrono-free ISO-8601 timestamp).
#[cfg(feature = "verifiable_credentials")]
pub mod verifiable_credentials;

// Blockchain integration (gated on web3 or blockchain feature)
#[cfg(any(feature = "web3", feature = "blockchain"))]
pub mod blockchain;

// Public re-exports — the public surface of this module. Everything
// else (internal helpers, raw `ed25519_dalek` types) is not
// re-exported so we can swap the backend without breaking callers.
pub use did::DidKey;
pub use error::Web3ErrorExt;
pub use identity::{
    base58_decode, base58_encode, verify_signature, verify_signature_detailed,
    verify_signed_message, verify_signed_message_detailed, Identity, PublicKey, SecretKey,
};
pub use signature::{Signature, SignedMessage};

// Verifiable Credentials re-exports — keep the most common
// construction helpers within easy reach.
#[cfg(feature = "verifiable_credentials")]
pub use verifiable_credentials::{
    CredentialProof, CredentialSchema, CredentialStatus, CredentialSubject, ProofType,
    VerifiableCredential, VerifiablePresentation,
};