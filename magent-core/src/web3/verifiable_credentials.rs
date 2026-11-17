//! Verifiable Credentials (VC) Module.
//!
//! This module implements W3C Verifiable Credentials support for mAgent,
//! enabling the agent to:
//!
//! - **Issue Credentials**: Create signed credentials from DID identities
//! - **Present Credentials**: Create selective-disclosure presentations
//! - **Verify Credentials**: Validate credential proofs and signatures
//! - **Credential Schemas**: Support for JSON Schema-based credential types
//!
//! ## W3C VC Data Model
//!
//! The implementation follows the W3C VC Data Model 2.0 specification:
//! <https://www.w3.org/TR/vc-data-model-2.0/>
//!
//! ## Credential Structure
//!
//! ```json
//! {
//!   "@context": ["https://www.w3.org/ns/credentials/v2"],
//!   "id": "urn:uuid:...",
//!   "type": ["VerifiableCredential", "ExampleCredential"],
//!   "issuer": "did:key:z6Mk...",
//!   "validFrom": "2024-01-01T00:00:00Z",
//!   "credentialStatus": { "id": "...", "type": "StatusList2021Entry" },
//!   "credentialSubject": { "id": "did:key:z6Mk...", ... },
//!   "proof": { ... }
//! }
//! ```
//!
//! ## Security
//!
//! - Credentials are signed using Ed25519 (via did:key)
//! - Presentations use BBS+ signatures for selective disclosure (future)
//! - LD-Suites for proof types

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::error::Web3ErrorKind;
use crate::web3::Signature;

/// Credential context URLs.
pub const CREDENTIALS_V2_CONTEXT: &str = "https://www.w3.org/ns/credentials/v2";
/// Credential context URL for the W3C VC Data Model 1.1.
pub const CREDENTIALS_V1_CONTEXT: &str = "https://www.w3.org/2018/credentials/v1";

/// Verifiable Credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiableCredential {
    /// JSON-LD context.
    #[serde(rename = "@context")]
    pub context: Vec<String>,
    /// Credential ID.
    pub id: String,
    /// Credential types.
    #[serde(rename = "type")]
    pub credential_type: Vec<String>,
    /// Issuer DID.
    pub issuer: String,
    /// Issuance timestamp (ISO 8601).
    #[serde(rename = "validFrom")]
    pub valid_from: String,
    /// Expiration timestamp (optional, ISO 8601).
    #[serde(rename = "validUntil", skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Credential status.
    #[serde(rename = "credentialStatus", skip_serializing_if = "Option::is_none")]
    pub credential_status: Option<CredentialStatus>,
    /// Credential subject.
    #[serde(rename = "credentialSubject")]
    pub credential_subject: CredentialSubject,
    /// Proof(s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<CredentialProof>,
}

impl VerifiableCredential {
    /// Create a new credential.
    pub fn new(
        id: impl Into<String>,
        issuer: impl Into<String>,
        credential_type: Vec<String>,
        credential_subject: CredentialSubject,
    ) -> Self {
        Self {
            context: vec![
                CREDENTIALS_V1_CONTEXT.to_string(),
                CREDENTIALS_V2_CONTEXT.to_string(),
            ],
            id: id.into(),
            credential_type,
            issuer: issuer.into(),
            valid_from: iso8601_timestamp(),
            valid_until: None,
            credential_status: None,
            credential_subject,
            proof: None,
        }
    }

    /// Set the expiration time.
    pub fn with_expiry(mut self, timestamp: impl Into<String>) -> Self {
        self.valid_until = Some(timestamp.into());
        self
    }

    /// Set the credential status.
    pub fn with_status(mut self, status: CredentialStatus) -> Self {
        self.credential_status = Some(status);
        self
    }

    /// Sign the credential.
    pub fn sign(&mut self, proof: CredentialProof) {
        self.proof = Some(proof);
    }

    /// Validate the credential structure.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if self.id.is_empty() {
            return Err(Web3ErrorKind::BlockchainError(
                "credential ID cannot be empty".to_string(),
            ));
        }
        if !self.issuer.starts_with("did:") {
            return Err(Web3ErrorKind::BlockchainError(
                "issuer must be a DID".to_string(),
            ));
        }
        if self.credential_type.is_empty() {
            return Err(Web3ErrorKind::BlockchainError(
                "credential type cannot be empty".to_string(),
            ));
        }
        if !self
            .credential_type
            .contains(&"VerifiableCredential".to_string())
        {
            return Err(Web3ErrorKind::BlockchainError(
                "credential type must include VerifiableCredential".to_string(),
            ));
        }
        Ok(())
    }

    /// Check if the credential is expired.
    ///
    /// Compares *instants*, not strings. `validUntil` is caller-supplied and
    /// ISO-8601 permits a timezone offset, so a lexical `until < now` is not
    /// time order: `2026-11-15T20:00:00+14:00` (06:00Z, already expired) sorts
    /// after `2026-11-15T07:31:00Z` and read as valid, while
    /// `…07:31:04.9Z` sorts after `…07:31:04Z` and read as expired. Both
    /// directions are wrong, and the first one accepts an expired credential.
    ///
    /// An unparseable `validUntil` is treated as **expired**: the field is a
    /// validity bound, and one that cannot be evaluated cannot be relied on.
    pub fn is_expired(&self) -> bool {
        match self.valid_until.as_deref() {
            None => false,
            Some(until) => match parse_iso8601_instant(until) {
                Some(at) => at < current_unix_secs(),
                None => true,
            },
        }
    }

    /// Check whether the credential is not yet valid.
    ///
    /// `validFrom` defaulted to the issue time and was never checked, so a
    /// credential stamped `validFrom: 2999-…` verified as if it were in force.
    /// An unparseable `validFrom` is likewise treated as **not yet valid**.
    pub fn is_not_yet_valid(&self) -> bool {
        match parse_iso8601_instant(&self.valid_from) {
            Some(at) => at > current_unix_secs(),
            // The default is the issue timestamp; a value that cannot be
            // parsed cannot establish that the credential is in force.
            None => true,
        }
    }

    /// Verify the credential's Ed25519 proof.
    ///
    /// `validate()` only checks the document's structure; this checks the
    /// cryptographic proof. The issuer is a `did:key`, so the public key is
    /// embedded in `proof.verification_method` — no external key lookup is
    /// needed. The verification method's DID must also equal `issuer`, so a
    /// credential cannot claim one issuer while carrying another key.
    pub fn verify_proof(&self) -> Result<(), Web3ErrorKind> {
        let proof = self
            .proof
            .as_ref()
            .ok_or_else(|| Web3ErrorKind::BlockchainError("credential has no proof".to_string()))?;
        match proof.proof_type {
            ProofType::Ed25519Signature2020 | ProofType::Ed25519Signature2018 => {}
            other => {
                return Err(Web3ErrorKind::BlockchainError(format!(
                    "unsupported credential proof type: {other:?}"
                )))
            }
        }
        let vm_did = proof
            .verification_method
            .split('#')
            .next()
            .unwrap_or("")
            .trim();
        if vm_did != self.issuer {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "proof verification method {:?} does not match issuer {:?}",
                vm_did, self.issuer
            )));
        }
        let did = crate::web3::DidKey::from_string(vm_did)?;
        let public_key = did.ed25519_public_key()?;
        let pk_bytes: [u8; 32] = public_key.try_into().map_err(|_| {
            Web3ErrorKind::BlockchainError("issuer public key is not 32 bytes".to_string())
        })?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid issuer key: {e}")))?;
        // A body-only signature covers the body, not the proof options. A
        // `challenge` or `domain` carried by such a proof is therefore
        // unsigned: it can be added or rewritten after issuance and the
        // signature still checks out. Refuse it rather than report a bound
        // challenge that is not bound. `signingScheme` is attacker-controlled
        // JSON, so this guard sits on the *verification* side where the
        // attacker cannot benefit from lying: claiming `options-v1` makes the
        // options part of the hash and a rewrite breaks the signature;
        // claiming `body-only` with a challenge present is rejected here.
        let scheme = Self::resolved_scheme(proof);
        if scheme == SigningScheme::BodyOnly {
            if proof.challenge.is_some() || proof.domain.is_some() {
                return Err(Web3ErrorKind::BlockchainError(
                    "credential proof carries a challenge/domain but was signed body-only, \
                     which does not cover them"
                        .to_string(),
                ));
            }
            // `purpose` is also outside a body-only signature, so any value
            // an attacker writes there is unauthenticated. Nothing consults
            // `purpose` today — which is exactly why accepting an arbitrary
            // one would be a trap for whoever adds such a check later. Only
            // the canonical issuer purpose is meaningful for an assertion
            // credential, so require it.
            //
            // `created` is likewise unsigned, and is treated as advisory
            // (nothing reads it for a decision). `verificationMethod` is
            // unsigned past its `#`fragment, but the DID before the `#` is
            // part of the signed body via `issuer`, and the fragment selects
            // nothing.
            if proof.purpose != "AssertionMethod" {
                return Err(Web3ErrorKind::BlockchainError(format!(
                    "body-only credential proof carries an unauthenticated purpose {:?}; \
                     only \"AssertionMethod\" is accepted",
                    proof.purpose
                )));
            }
        }
        let signature = Signature::from_hex(&proof.proof_value)?;
        let dalek_sig = ed25519_dalek::Signature::from_bytes(signature.to_bytes());
        let signed_bytes = self.encoded_for_verification()?;
        // `verify_strict`, not `verify`: the lenient verifier accepts
        // small-order public keys, which makes universal forgery trivial. A
        // `did:key` whose key is the identity point (0x01 ‖ 0…) combined with
        // a signature `R = identity, S = 0` verified for **any** credential —
        // attacker-chosen issuer, subject and claims, no private key. Measured
        // against the pre-fix code: `verify()` returned `Ok(())`.
        // `verify_strict` rejects small-order `A` and `R`, closing it.
        verifying_key
            .verify_strict(&signed_bytes, &dalek_sig)
            .map_err(|_| {
                Web3ErrorKind::BlockchainError(
                    "credential proof signature does not verify".to_string(),
                )
            })
    }

    /// Structural validation plus proof verification and expiry.
    pub fn verify(&self) -> Result<(), Web3ErrorKind> {
        self.validate()?;
        if self.is_not_yet_valid() {
            return Err(Web3ErrorKind::BlockchainError(
                "credential is not yet valid (validFrom is in the future or unparseable)"
                    .to_string(),
            ));
        }
        if self.is_expired() {
            return Err(Web3ErrorKind::BlockchainError(
                "credential has expired (validUntil is in the past or unparseable)".to_string(),
            ));
        }
        self.verify_proof()
    }

    /// The bytes the **already attached** proof's signature covers.
    ///
    /// The credential body is serialised with `proof` dropped, and the proof
    /// *options* are then appended: `created`, `verificationMethod`,
    /// `purpose`, `challenge` and `domain`.
    ///
    /// SECURITY: dropping the proof entirely — as this did before — satisfied
    /// the "same bytes regardless of the attached proof" rule but left the
    /// options unprotected. [`CredentialProof`] carries `challenge` and
    /// `domain`; a credential whose proof names a challenge must have that
    /// challenge covered by the signature, or the field can be rewritten after
    /// signing and still verify. The options are length- and presence-tagged
    /// so no two different option sets can collide.
    ///
    /// # Signing
    ///
    /// This is **not** a signing API. A credential with no proof attached has
    /// no options to cover, so this returns the body alone — and signing that
    /// produces a signature which the (options-covering) verifier rejects.
    /// Use [`Self::signing_input`], which takes the proof being built and
    /// returns exactly the bytes verification will check. The two used to be
    /// documented as one and the same, so the old
    /// `sign(self.encode_for_signing())` pattern silently produced credentials
    /// that never verified.
    pub fn encode_for_signing(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        // Dispatch on the attached proof's scheme (body-only for a proof that
        // predates the field), so this agrees with what verification computes.
        self.encoded_for_verification()
    }

    /// The bytes to sign for `proof`, before it is attached.
    ///
    /// The sign-then-attach half of [`Self::encode_for_signing`]. It builds
    /// the bytes by attaching `proof` to a clone and running the *same*
    /// function verification uses, so the two cannot disagree about what is
    /// covered. Resolving the scheme independently here — as an earlier
    /// version did — produced signatures the verifier could not check
    /// whenever the two defaults differed.
    pub fn signing_input(&self, proof: &CredentialProof) -> Result<Vec<u8>, Web3ErrorKind> {
        let mut with_proof = self.clone();
        with_proof.proof = Some(proof.clone());
        with_proof.encoded_for_verification()
    }

    /// The scheme a credential proof is verified under.
    ///
    /// An absent field means a proof from before it existed, which signed the
    /// body alone. Used by both signing and verification.
    fn resolved_scheme(proof: &CredentialProof) -> SigningScheme {
        proof.signing_scheme.unwrap_or(SigningScheme::BodyOnly)
    }

    /// The bytes a stored proof's signature covers, following the scheme
    /// recorded in the proof itself.
    ///
    /// Returns an error rather than an empty buffer if the body cannot be
    /// serialised. A signing input that silently became `b""` (via
    /// `unwrap_or_default`) would produce a signature over nothing, and the
    /// verifier computes the same empty body, so the two would agree and the
    /// corruption would be invisible. Today no field can make `to_vec` fail;
    /// the `Result` keeps that true if one is added.
    fn encoded_for_verification(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        let mut clone = self.clone();
        let scheme = clone
            .proof
            .as_ref()
            .map(Self::resolved_scheme)
            .unwrap_or(SigningScheme::ProofOptions);
        clone.proof = None;
        let mut out = serde_json::to_vec(&clone).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!(
                "when_signing_input: credential body is not serialisable: {e}"
            ))
        })?;
        if scheme == SigningScheme::ProofOptions {
            if let Some(proof) = self.proof.as_ref() {
                out.extend_from_slice(&Self::proof_options_bytes(proof));
            }
        }
        Ok(out)
    }

    /// Length- and presence-tagged proof options, excluding `proofValue`.
    fn proof_options_bytes(proof: &CredentialProof) -> Vec<u8> {
        encode_proof_options(
            &proof.created,
            &proof.verification_method,
            &proof.purpose,
            proof.challenge.as_deref(),
            proof.domain.as_deref(),
        )
    }

    /// Canonicalise for storage / transmission (proof attached).
    pub fn encode_canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Credential subject (the entity the credential describes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialSubject {
    /// Subject's DID.
    pub id: String,
    /// Additional claims.
    #[serde(flatten)]
    pub claims: serde_json::Map<String, serde_json::Value>,
}

impl CredentialSubject {
    /// Create a new credential subject.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            claims: serde_json::Map::new(),
        }
    }

    /// The claim names [`Self`] serialises as real fields.
    ///
    /// `claims` is `#[serde(flatten)]`ed, and serde does not reject a map key
    /// that collides with a struct field — it emits the key twice. So
    /// `with_claim("id", …)` produced
    /// `"credentialSubject":{"id":"<real>","id":"<claim>"}`, which this
    /// crate's own `Deserialize` rejects as `duplicate field 'id'` (a
    /// credential that cannot be read back) while serde-based consumers in
    /// other languages resolve last-wins (so the *claim* becomes the subject
    /// id, in a document whose signature covers those bytes). Either reading
    /// is unacceptable, so the collision is refused at the constructor.
    pub const RESERVED_CLAIM_KEYS: [&'static str; 1] = ["id"];

    /// Whether `key` would collide with a serialised field of [`Self`].
    pub fn is_reserved_claim_key(key: &str) -> bool {
        Self::RESERVED_CLAIM_KEYS.contains(&key)
    }

    /// Add a claim.
    ///
    /// A key that collides with a real field of this struct (`id`) is
    /// **dropped**, because storing it would either make the credential
    /// unreadable or change which value a consumer sees for that field — see
    /// [`Self::is_reserved_claim_key`]. Callers that need to refuse the input
    /// outright should check the key themselves; `vc issue` does, so the
    /// operator gets an error instead of a silently missing claim.
    pub fn with_claim<T: Serialize>(mut self, key: impl Into<String>, value: &T) -> Self {
        let key = key.into();
        if Self::is_reserved_claim_key(&key) {
            return self;
        }
        if let Ok(val) = serde_json::to_value(value) {
            self.claims.insert(key, val);
        }
        self
    }

    /// Add a claim, reporting a reserved key instead of dropping it.
    ///
    /// Use this at an input boundary, where a silently missing claim is a lie
    /// about what was issued.
    pub fn try_with_claim<T: Serialize>(
        mut self,
        key: impl Into<String>,
        value: &T,
    ) -> Result<Self, Web3ErrorKind> {
        let key = key.into();
        if Self::is_reserved_claim_key(&key) {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "claim name {key:?} collides with a credentialSubject field"
            )));
        }
        match serde_json::to_value(value) {
            Ok(val) => {
                self.claims.insert(key, val);
                Ok(self)
            }
            Err(e) => Err(Web3ErrorKind::BlockchainError(format!(
                "claim {key:?} is not serialisable: {e}"
            ))),
        }
    }

    /// Get a claim value.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.claims.get(key)
    }
}

/// Credential status (for revocation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialStatus {
    /// Status ID.
    pub id: String,
    /// Status type.
    #[serde(rename = "type")]
    pub status_type: String,
    /// Status purpose.
    pub purpose: String,
}

impl CredentialStatus {
    /// Create a new credential status.
    pub fn new(id: impl Into<String>, status_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status_type: status_type.into(),
            purpose: "revocation".to_string(),
        }
    }

    /// Create a StatusList2021 entry.
    pub fn status_list_2021(
        id: impl Into<String>,
        issuer: impl Into<String>,
        status_list_index: u64,
    ) -> Self {
        Self {
            id: id.into(),
            status_type: "StatusList2021Entry".to_string(),
            purpose: format!(
                "urn:iso:std:iso:22739:2023#revocation&issuer={}&index={}",
                issuer.into(),
                status_list_index
            ),
        }
    }
}

/// Proof on a credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialProof {
    /// Proof type.
    #[serde(rename = "type")]
    pub proof_type: ProofType,
    /// Proof created timestamp.
    pub created: String,
    /// Verification method (DID URL).
    #[serde(rename = "verificationMethod")]
    pub verification_method: String,
    /// Proof purpose.
    pub purpose: String,
    /// Proof value (signature).
    pub proof_value: String,
    /// Challenge (for Holder binding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    /// Domain (for Holder binding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Which bytes the signature covers.
    ///
    /// Absent on credentials issued before the proof options became part of
    /// the signing input; those signed the body alone. The field lets a
    /// verifier apply the rule the signature was actually computed under
    /// instead of rejecting every older credential as if it were tampered
    /// with — the two are otherwise indistinguishable, since both fail the
    /// same signature check.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signing_scheme: Option<SigningScheme>,
}

/// What a credential or presentation proof's signature covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SigningScheme {
    /// Signature over the document body **and** the proof options
    /// (`created`, `verificationMethod`, `purpose`, `challenge`, `domain`).
    /// This is what every proof created now uses.
    #[default]
    #[serde(rename = "options-v1")]
    ProofOptions,
    /// Signature over the document body only. Produced by builds before the
    /// options were bound; accepted only so those credentials keep verifying.
    #[serde(rename = "body-only")]
    BodyOnly,
}

impl CredentialProof {
    /// Create a new proof.
    pub fn new(
        proof_type: ProofType,
        verification_method: impl Into<String>,
        signature: Signature,
    ) -> Self {
        Self {
            proof_type,
            created: iso8601_timestamp(),
            verification_method: verification_method.into(),
            purpose: "AssertionMethod".to_string(),
            proof_value: signature.to_hex(),
            challenge: None,
            domain: None,
            signing_scheme: Some(SigningScheme::ProofOptions),
        }
    }

    /// Set the challenge.
    pub fn with_challenge(mut self, challenge: impl Into<String>) -> Self {
        self.challenge = Some(challenge.into());
        self
    }

    /// Set the domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }
}

/// Proof types supported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofType {
    /// Ed25519 Signature 2020.
    #[default]
    #[serde(rename = "Ed25519Signature2020")]
    Ed25519Signature2020,
    /// Ed25519 Signature 2018.
    #[serde(rename = "Ed25519Signature2018")]
    Ed25519Signature2018,
    /// Data Integrity Proof.
    #[serde(rename = "DataIntegrityProof")]
    DataIntegrityProof,
    /// JSON Web Signature (JWS).
    #[serde(rename = "JsonWebSignature2020")]
    JsonWebSignature2020,
}

// ============================================================================
// Proof-option encoding (shared by credentials and presentations)
// ============================================================================

/// Encode the proof options that are covered by the signature.
///
/// One encoder for both proof types, so the two cannot drift: a field added
/// for one would otherwise be silently omitted for the other, and the
/// signature it produces would not be checkable by the matching verifier.
///
/// Every string is written as `(u64 big-endian length, bytes)` and every
/// optional value is prefixed with a presence byte, so no two distinct option
/// sets encode to the same byte string — a value containing a separator
/// cannot impersonate two fields, and `None` cannot collide with `Some("")`.
/// `proofValue` is deliberately excluded: it is the signature over these
/// bytes.
fn encode_proof_options(
    created: &str,
    verification_method: &str,
    purpose: &str,
    challenge: Option<&str>,
    domain: Option<&str>,
) -> Vec<u8> {
    fn value(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    fn optional(out: &mut Vec<u8>, opt: Option<&str>) {
        match opt {
            Some(v) => {
                out.push(1);
                value(out, v.as_bytes());
            }
            None => out.push(0),
        }
    }

    let mut out = Vec::new();
    value(&mut out, created.as_bytes());
    value(&mut out, verification_method.as_bytes());
    value(&mut out, purpose.as_bytes());
    optional(&mut out, challenge);
    optional(&mut out, domain);
    out
}

// ============================================================================
// Verifiable Presentation
// ============================================================================

/// A Verifiable Presentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiablePresentation {
    /// JSON-LD context.
    #[serde(rename = "@context")]
    pub context: Vec<String>,
    /// Presentation ID.
    pub id: String,
    /// Presentation types.
    #[serde(rename = "type")]
    pub presentation_type: Vec<String>,
    /// Holder (presenter's DID).
    pub holder: String,
    /// Verifiable credentials (presented).
    pub verifiable_credential: Vec<VerifiableCredential>,
    /// Presentation created timestamp.
    pub created: String,
    /// Proof(s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<PresentationProof>,
}

impl VerifiablePresentation {
    /// Create a new presentation.
    pub fn new(holder: impl Into<String>, credentials: Vec<VerifiableCredential>) -> Self {
        Self {
            context: vec![
                CREDENTIALS_V1_CONTEXT.to_string(),
                CREDENTIALS_V2_CONTEXT.to_string(),
            ],
            id: format!("urn:uuid:{}", uuid_v4()),
            presentation_type: vec!["VerifiablePresentation".to_string()],
            holder: holder.into(),
            verifiable_credential: credentials,
            created: iso8601_timestamp(),
            proof: None,
        }
    }

    /// Add a proof.
    pub fn sign(&mut self, proof: PresentationProof) {
        self.proof = Some(proof);
    }

    /// The bytes that a presentation proof signs.
    ///
    /// The document body is serialised with `proof` dropped, as
    /// [`VerifiableCredential::encode_for_signing`] does, **and then the proof
    /// options are appended**: `created`, `verificationMethod`, `purpose`,
    /// `challenge` and `domain`.
    ///
    /// Dropping the proof entirely — the first version of this function —
    /// left the challenge unsigned. A challenge exists precisely to be bound
    /// to the signature (it is what makes a presentation non-replayable: the
    /// verifier issues a fresh one and checks the signature covers *that*
    /// value). With it outside the signed bytes, an attacker could take a
    /// valid presentation and rewrite `challenge` to any value and the proof
    /// still verified. `domain` and the verification method had the same
    /// hole; the method is checked separately against `holder`, but only
    /// after the fact.
    ///
    /// This mirrors W3C Data Integrity, where the proof options are part of
    /// the hash input. The options are appended in a fixed order rather than
    /// serialised as a struct so that adding a field later cannot silently
    /// change how existing signatures are computed: a new field must be
    /// appended explicitly.
    /// # Signing
    ///
    /// As with [`VerifiableCredential::encode_for_signing`], this is not a
    /// signing API: a proof-less presentation yields proof-less bytes, and a
    /// presentation proof signed that way can never pass `verify_proof` (which
    /// requires the options scheme). Use [`Self::signing_input`].
    pub fn encode_for_signing(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        // Dispatch on the attached proof's scheme, matching verification.
        self.encoded_for_verification()
    }

    /// The bytes a stored proof's signature covers, following the scheme
    /// recorded in the proof itself.
    ///
    /// `options-v1` only — anything else is refused by `verify_proof` before
    /// this runs. The hashing itself still follows the recorded scheme so
    /// `signing_input` can nevertheless produce body-only bytes when a test
    /// or migration asks for them; acceptance is decided by the caller.
    fn encoded_for_verification(&self) -> Result<Vec<u8>, Web3ErrorKind> {
        let scheme = self
            .proof
            .as_ref()
            .and_then(|p| p.signing_scheme)
            .unwrap_or(SigningScheme::ProofOptions);
        let mut clone = self.clone();
        clone.proof = None;
        let mut out = serde_json::to_vec(&clone).map_err(|e| {
            Web3ErrorKind::BlockchainError(format!("presentation body is not serialisable: {e}"))
        })?;
        if scheme == SigningScheme::ProofOptions {
            if let Some(proof) = self.proof.as_ref() {
                out.extend_from_slice(&Self::proof_options_bytes(proof));
            }
        }
        Ok(out)
    }

    /// The bytes to sign for `proof`, before it is attached.
    ///
    /// This is the sign-then-attach half of [`Self::encode_for_signing`]: the
    /// signer builds the proof (with no `proofValue` yet), hashes the body
    /// plus that proof's options through this function, signs the result, and
    /// then sets the signature. Computing the hash from a *proof-less* clone
    /// instead — as `vc present` did — produces a signature that does not
    /// cover the challenge, so verification (which does include the options)
    /// fails, or worse, succeeds for a presentation whose challenge was
    /// swapped afterwards.
    pub fn signing_input(&self, proof: &PresentationProof) -> Result<Vec<u8>, Web3ErrorKind> {
        // Attach the proof and run the verification path, so signing and
        // verification are the same computation by construction.
        let mut with_proof = self.clone();
        with_proof.proof = Some(proof.clone());
        with_proof.encoded_for_verification()
    }

    /// `created`, `verificationMethod`, `purpose`, `challenge` and `domain`,
    /// each length-prefixed so a value containing the separator cannot make
    /// two different option sets hash identically. `proofValue` is excluded —
    /// it is the signature over these bytes.
    ///
    /// `domain` carries a presence tag before its length. Encoding a missing
    /// domain as an empty string made `domain: None` and `domain: Some("")`
    /// hash identically, so an absent domain could be *added* to a signed
    /// presentation as an empty string without invalidating the signature —
    /// signature malleability. The tag makes "absent" and "present but empty"
    /// distinct inputs.
    fn proof_options_bytes(proof: &PresentationProof) -> Vec<u8> {
        encode_proof_options(
            &proof.created,
            &proof.verification_method,
            &proof.purpose,
            // A presentation proof always carries a challenge; `Some` keeps
            // the presence byte consistent with the credential encoding.
            Some(proof.challenge.as_str()),
            proof.domain.as_deref(),
        )
    }

    /// Verify the presentation's Ed25519 proof.
    ///
    /// The holder is a `did:key`, so the public key is embedded in the proof's
    /// verification method. It must also match the `holder` field, otherwise a
    /// presentation could claim one holder while carrying another's key —
    /// the same check `VerifiableCredential::verify_proof` applies to the
    /// issuer.
    pub fn verify_proof(&self) -> Result<(), Web3ErrorKind> {
        let proof = self.proof.as_ref().ok_or_else(|| {
            Web3ErrorKind::BlockchainError("presentation has no proof".to_string())
        })?;
        // A body-only signature leaves `challenge` unsigned, so it must never
        // be accepted for a presentation — otherwise the challenge can be
        // rewritten and the presentation replayed to another verifier. An
        // absent field is an old-build proof and is also refused (failing
        // closed costs nothing here: presentations are not stored).
        if proof.signing_scheme != Some(SigningScheme::ProofOptions) {
            return Err(Web3ErrorKind::BlockchainError(
                "presentation proof must use the options-v1 signing scheme; \
                 a body-only signature does not cover the challenge"
                    .to_string(),
            ));
        }
        match proof.proof_type {
            ProofType::Ed25519Signature2020 | ProofType::Ed25519Signature2018 => {}
            other => {
                return Err(Web3ErrorKind::BlockchainError(format!(
                    "unsupported presentation proof type: {other:?}"
                )))
            }
        }
        let vm_did = proof
            .verification_method
            .split('#')
            .next()
            .unwrap_or("")
            .trim();
        if vm_did != self.holder {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "proof verification method {:?} does not match holder {:?}",
                vm_did, self.holder
            )));
        }
        let did = crate::web3::DidKey::from_string(vm_did)?;
        let public_key = did.ed25519_public_key()?;
        let pk_bytes: [u8; 32] = public_key.try_into().map_err(|_| {
            Web3ErrorKind::BlockchainError("holder public key is not 32 bytes".to_string())
        })?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|e| Web3ErrorKind::BlockchainError(format!("invalid holder key: {e}")))?;
        let signature = Signature::from_hex(&proof.proof_value)?;
        let dalek_sig = ed25519_dalek::Signature::from_bytes(signature.to_bytes());
        let signed_bytes = self.encode_for_signing()?;
        // `verify_strict` for the same reason as `verify_proof`: the lenient
        // verifier accepts small-order keys, and a presentation signed that
        // way verifies for any content.
        verifying_key
            .verify_strict(&signed_bytes, &dalek_sig)
            .map_err(|_| {
                Web3ErrorKind::BlockchainError(
                    "presentation proof signature does not verify".to_string(),
                )
            })
    }

    /// Validate the presentation, then verify its proof **and every contained
    /// credential**.
    ///
    /// A presentation's own proof only says "this holder assembled these
    /// items". It says nothing about whether the credentials inside are
    /// genuine: a holder can mint a presentation around any document at all.
    /// Checking only the outer proof accepted a presentation whose single
    /// credential named a fake issuer and carried no signature — the
    /// verifier reported a valid presentation of a forged claim.
    ///
    /// Each credential therefore goes through the same path
    /// [`VerifiableCredential::verify`] uses: structural validation, expiry,
    /// and its own Ed25519 proof against its own issuer key.
    pub fn verify(&self) -> Result<(), Web3ErrorKind> {
        self.validate()?;
        self.verify_proof()?;
        for (index, credential) in self.verifiable_credential.iter().enumerate() {
            credential.verify().map_err(|e| {
                Web3ErrorKind::BlockchainError(format!(
                    "presented credential #{index} is not valid: {e}"
                ))
            })?;
        }
        Ok(())
    }

    /// Verify a presentation against a challenge the verifier issued.
    ///
    /// A signature that covers the challenge is only half the check: the
    /// verifier must also confirm the presentation carries *its* challenge,
    /// otherwise an attacker can replay a presentation the holder signed for
    /// some other verifier. `expected_challenge` is compared byte-for-byte
    /// before the proof is verified.
    ///
    /// Everything else [`Self::verify`] does still applies.
    pub fn verify_with_challenge(&self, expected_challenge: &str) -> Result<(), Web3ErrorKind> {
        let proof = self.proof.as_ref().ok_or_else(|| {
            Web3ErrorKind::BlockchainError("presentation has no proof".to_string())
        })?;
        if proof.challenge != expected_challenge {
            return Err(Web3ErrorKind::BlockchainError(format!(
                "presentation challenge {:?} does not match the expected challenge",
                proof.challenge
            )));
        }
        self.verify()
    }

    /// Validate the presentation.
    pub fn validate(&self) -> Result<(), Web3ErrorKind> {
        if !self.holder.starts_with("did:") {
            return Err(Web3ErrorKind::BlockchainError(
                "holder must be a DID".to_string(),
            ));
        }
        if self.verifiable_credential.is_empty() {
            return Err(Web3ErrorKind::BlockchainError(
                "presentation must contain at least one credential".to_string(),
            ));
        }
        // Validate each credential
        for vc in &self.verifiable_credential {
            vc.validate()?;
        }
        Ok(())
    }
}

/// Presentation proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentationProof {
    /// Proof type.
    #[serde(rename = "type")]
    pub proof_type: ProofType,
    /// Proof created timestamp.
    pub created: String,
    /// Verification method.
    #[serde(rename = "verificationMethod")]
    pub verification_method: String,
    /// Proof purpose.
    pub purpose: String,
    /// Proof value.
    pub proof_value: String,
    /// Challenge.
    pub challenge: String,
    /// Domain.
    pub domain: Option<String>,
    /// Which bytes the signature covers. See [`SigningScheme`].
    ///
    /// Presentations must be `options-v1`: a body-only signature leaves
    /// `challenge` unsigned, so it could be rewritten after signing and the
    /// presentation replayed to another verifier. `verify_proof` rejects both
    /// an explicit `body-only` and an absent field (a proof from a build
    /// before this field existed). Presentations are verified as they are
    /// presented, so there is no stored corpus that failing closed would
    /// strand.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signing_scheme: Option<SigningScheme>,
}

impl PresentationProof {
    /// Create a new presentation proof.
    pub fn new(
        proof_type: ProofType,
        verification_method: impl Into<String>,
        challenge: impl Into<String>,
        signature: Signature,
    ) -> Self {
        Self {
            proof_type,
            created: iso8601_timestamp(),
            verification_method: verification_method.into(),
            purpose: "Authentication".to_string(),
            proof_value: signature.to_hex(),
            challenge: challenge.into(),
            domain: None,
            signing_scheme: Some(SigningScheme::ProofOptions),
        }
    }

    /// Set the domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }
}

// ============================================================================
// Credential Schema
// ============================================================================

/// A credential schema definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialSchema {
    /// Schema ID.
    pub id: String,
    /// Schema type.
    #[serde(rename = "type")]
    pub schema_type: String,
    /// Schema name.
    pub name: String,
    /// Schema version.
    pub version: String,
    /// JSON Schema.
    pub schema: serde_json::Value,
}

impl CredentialSchema {
    /// Create a new credential schema.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        schema: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            schema_type: "JsonSchema2023".to_string(),
            name: name.into(),
            version: version.into(),
            schema,
        }
    }

    /// Validate a credential against this schema.
    ///
    /// Implements the `required` keyword of the JSON Schema dialect the schema
    /// declares: an *array* of property names, each of which must be present in
    /// the credential subject. A name of `id` refers to the subject's DID, which
    /// is a dedicated field rather than a claim.
    ///
    /// A `required` that is neither absent nor a well-formed array of strings is
    /// reported as an error rather than skipped: silently ignoring a schema this
    /// function cannot interpret is how a validator ends up always returning
    /// `Ok`, which would let an unvalidated credential look conformant.
    pub fn validate(&self, credential: &VerifiableCredential) -> Result<(), Web3ErrorKind> {
        let subject = &credential.credential_subject;

        let required = match self.schema.get("required") {
            Some(value) => value.as_array().ok_or_else(|| {
                Web3ErrorKind::BlockchainError(
                    "schema `required` must be an array of property names".to_string(),
                )
            })?,
            None => return Ok(()),
        };

        for entry in required {
            let field = entry.as_str().ok_or_else(|| {
                Web3ErrorKind::BlockchainError(
                    "schema `required` entries must be strings".to_string(),
                )
            })?;
            let present = if field == "id" {
                !subject.id.is_empty()
            } else {
                subject.claims.contains_key(field)
            };
            if !present {
                return Err(Web3ErrorKind::BlockchainError(format!(
                    "missing required field: {field}"
                )));
            }
        }

        Ok(())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Generate a UUID v4 string backed by the OS RNG.
///
/// **Security note (audit-2026-08):** the previous implementation used a
/// fixed-seed Linear Congruential Generator with a `static mut` state.
/// That made every issued credential ID *predictable* to an attacker who
/// knew the seed, and the `static mut` access was also UB if reached
/// from a multi-threaded context. We now use `getrandom` (which the
/// `web3` feature already pulls in via the workspace) so that:
///   * IDs are cryptographically random,
///   * no global mutable state is touched (UB-free),
///   * the implementation works on `no_std` as long as the target's
///     `getrandom` backend is registered (true on macOS/Linux/ESP32).
fn uuid_v4() -> String {
    // Pull from the OS RNG. 16 bytes for a UUID v4.
    let mut bytes = [0u8; 16];
    // `getrandom` returns `Err` only when no backend is registered or the
    // backend itself fails. In either case we fall back to a clearly-tagged
    // dummy UUID rather than panicking; the credential issuer logs the
    // failure so it remains visible.
    if getrandom::getrandom(&mut bytes).is_err() {
        log_random_failure("uuid_v4");
        // RFC 4122 §4.4 "Nil UUID" — distinctive, easy to grep in logs.
        bytes = [0u8; 16];
    }
    // Set the RFC 4122 v4 version and variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Best-effort, side-effect-free logger for RNG failures. In `no_std` /
/// `defmt` builds we still want a trace; in `std` builds we use the
/// standard `log` crate.
#[cfg(feature = "std")]
fn log_random_failure(where_: &str) {
    log::error!(
        "[web3/verifiable_credentials] {}: RNG unavailable; using nil UUID",
        where_
    );
}

#[cfg(not(feature = "std"))]
fn log_random_failure(where_: &str) {
    // On `no_std` builds without a logger registered we have no choice but
    // to drop the message. Callers that need visibility should register a
    // `defmt` logger.
    let _ = where_;
}

/// Get current ISO 8601 timestamp.
/// For no_std environments, returns a placeholder timestamp.
/// Caller should override with actual time in production.
#[cfg(feature = "std")]
fn iso8601_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Manual ISO-8601 formatting (UTC) to avoid a hard dependency
    // on `chrono`. Seconds resolution is sufficient for VC validity
    // windows, which are typically 30+ seconds.
    format_unix_to_iso8601(now)
}

#[cfg(not(feature = "std"))]
fn iso8601_timestamp() -> String {
    // Simplified timestamp for no_std - caller should override
    "1970-01-01T00:00:00Z".to_string()
}

/// The current instant, in Unix seconds, for validity checks.
///
/// A `no_std` build has no clock here, so it reports 0 — the same epoch the
/// `no_std` `iso8601_timestamp` reports. That keeps `is_expired` and
/// `is_not_yet_valid` consistent with each other instead of comparing a real
/// `validUntil` against a fabricated "now".
fn current_unix_secs() -> i64 {
    #[cfg(feature = "std")]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            // A clock before 1970 is not a time we can evaluate a bound
            // against; 0 makes every real `validFrom` "not yet valid" and
            // every real `validUntil` "expired", which fails closed.
            .unwrap_or(0)
    }
    #[cfg(not(feature = "std"))]
    {
        0
    }
}

/// Whether `text` is a `validFrom` / `validUntil` bound this module can
/// evaluate.
///
/// Issuers should reject a bound this returns `false` for: `is_expired` and
/// `is_not_yet_valid` treat an unparseable bound as "already expired" / "not
/// yet valid", so a credential signed with one is dead on arrival. Exposing
/// the predicate lets `vc issue` refuse it instead of printing a success the
/// verifier will later contradict.
pub fn is_valid_instant_bound(text: &str) -> bool {
    parse_iso8601_instant(text).is_some()
}

/// Parse the subset of ISO-8601 / RFC 3339 that a `validFrom` / `validUntil`
/// can carry into a Unix second count, or `None` if it is not a valid instant.
///
/// Accepted: `YYYY-MM-DDTHH:MM:SS` with an optional fractional part (truncated
/// — second resolution is what the bound is compared at) and an optional
/// `Z` / `z` / `±HH:MM` / `±HHMM` zone. A missing zone is read as UTC, which is
/// what a W3C VC means by a bare `dateTime`.
///
/// Rejects: impossible dates (`2026-02-30`), out-of-range clock fields,
/// trailing garbage, and a zone offset past `±23:59`. Every rejection makes a
/// caller fail closed, so a lenient parse would be the wrong default.
fn parse_iso8601_instant(text: &str) -> Option<i64> {
    const SECS_PER_DAY: i64 = 86_400;
    /// Widest instant this module's own formatter can produce, with room for a
    /// timezone offset either side. Beyond it the day arithmetic could
    /// overflow, and no real credential window needs it.
    const MAX_ABS_SECS: i64 = 253_402_300_800; // 9999-12-31T23:59:59Z

    let bytes = text.as_bytes();
    // The shortest legal form is `YYYY-MM-DDTHH:MM:SS` (19 bytes).
    if bytes.len() < 19 {
        return None;
    }
    let digits = |from: usize, len: usize| -> Option<i64> {
        let slice = bytes.get(from..from + len)?;
        let mut value: i64 = 0;
        for &b in slice {
            if !b.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(b - b'0');
        }
        Some(value)
    };

    // `YYYY-MM-DD`
    let year = digits(0, 4)?;
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    // The date/time separator may be `T` (RFC 3339) or a space.
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return None;
    }
    let hour = digits(11, 2)?;
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // `hour` 24 is only `24:00:00`; treat anything above 23 as invalid, which
    // also keeps seconds-of-day in range.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // A leap second (60) is folded to :59 so it cannot land on the next day.
    let second = second.min(59);
    if day > days_in_month(year, month as u32) as i64 {
        return None;
    }

    let mut rest = 19;
    // Optional fractional seconds; truncated, not rounded, so the bound is
    // never moved *up* past the instant it names.
    if bytes.get(rest) == Some(&b'.') {
        rest += 1;
        let start = rest;
        while bytes.get(rest).is_some_and(u8::is_ascii_digit) {
            rest += 1;
        }
        if rest == start {
            return None;
        }
    }

    // Optional zone. Absent means UTC.
    let offset_secs: i64 = match bytes.get(rest) {
        None => 0,
        Some(b'Z') | Some(b'z') if rest + 1 == bytes.len() => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = digits(rest + 1, 2)?;
            // `+HH:MM` or `+HHMM`.
            let (om, end) = match bytes.get(rest + 3) {
                Some(b':') => (digits(rest + 4, 2)?, rest + 6),
                _ => (digits(rest + 3, 2)?, rest + 5),
            };
            if end != bytes.len() || oh > 23 || om > 59 {
                return None;
            }
            let magnitude = oh * 3600 + om * 60;
            if sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        Some(_) => return None,
    };

    let days = days_from_civil(year, month as u32, day as u32);
    let secs = days
        .checked_mul(SECS_PER_DAY)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        // A local reading west of UTC is later in absolute terms.
        .checked_sub(offset_secs)?;
    if secs.abs() > MAX_ABS_SECS.saturating_mul(2) {
        return None;
    }
    Some(secs)
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date.
///
/// Howard Hinnant's `days_from_civil`, the inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    const DAYS_PER_CYCLE: i64 = 146_097;
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 {
        month as i64 - 3
    } else {
        month as i64 + 9
    }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * DAYS_PER_CYCLE + doe - 719_468
}

/// Days in `month` of `year`, proleptic Gregorian.
fn days_in_month(year: i64, month: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

/// Format a Unix timestamp (seconds) as ISO-8601 UTC
/// (`YYYY-MM-DDTHH:MM:SSZ`) without pulling in the `chrono` crate.
///
/// Implementation notes:
/// - Uses Howard Hinnant's `days_from_civil` algorithm for civil -> days
///   conversion (no leap-second table needed for our 1970-2099 range).
/// - Year/month/day are derived from the day count; hour/minute/second
///   from the seconds-of-day remainder.
fn format_unix_to_iso8601(unix_secs: u64) -> String {
    const SECS_PER_DAY: u64 = 86_400;

    let days = (unix_secs / SECS_PER_DAY) as i64;
    let secs_of_day = unix_secs % SECS_PER_DAY;
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;
    let second = (secs_of_day % 60) as u32;

    // Howard Hinnant's algorithm: convert year/month/day -> days since
    // 1970-01-01. Then derive year/month/day from that day count.
    let (year, month, day) = civil_from_days(days);

    let mut out = String::with_capacity(20);
    write_iso8601(&mut out, year, month, day, hour, minute, second);
    out
}

/// Push a `YYYY-MM-DDTHH:MM:SSZ` string into `out` without using
/// `core::fmt::write!` (small enough that doing it manually is
/// shorter and avoids surprises).
fn write_iso8601(
    out: &mut alloc::string::String,
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) {
    use alloc::string::ToString;
    let pad4 = |n: i64| -> alloc::string::String {
        let mut s = n.to_string();
        while s.len() < 4 {
            s = alloc::format!("0{}", s);
        }
        s
    };
    out.push_str(&pad4(year));
    out.push('-');
    if month < 10 {
        out.push('0');
    }
    out.push_str(&month.to_string());
    out.push('-');
    if day < 10 {
        out.push('0');
    }
    out.push_str(&day.to_string());
    out.push('T');
    if hour < 10 {
        out.push('0');
    }
    out.push_str(&hour.to_string());
    out.push(':');
    if minute < 10 {
        out.push('0');
    }
    out.push_str(&minute.to_string());
    out.push(':');
    if second < 10 {
        out.push('0');
    }
    out.push_str(&second.to_string());
    out.push('Z');
}

/// Howard Hinnant's `civil_from_days`: convert a count of days since
/// 1970-01-01 (Unix epoch) into (year, month, day) in the proleptic
/// Gregorian calendar. Reference:
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Shift epoch from 1970-01-01 to 0000-03-01 so leap-day handling
    // becomes uniform.
    const DAYS_PER_CYCLE: i64 = 146_097; // 400-year Gregorian cycle

    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - (DAYS_PER_CYCLE - 1) } / DAYS_PER_CYCLE;
    let doe = (z - era * DAYS_PER_CYCLE) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod time_tests {
    use alloc::string::ToString;

    use super::{
        days_in_month, format_unix_to_iso8601, parse_iso8601_instant, VerifiableCredential,
    };

    /// Parsing must invert formatting for every instant the module produces.
    #[test]
    fn parse_round_trips_the_formatter() {
        for secs in [
            0i64,
            1,
            86_399,
            86_400,
            1_705_323_930,
            1_582_934_400,
            4_102_444_799,
        ] {
            let text = format_unix_to_iso8601(secs as u64);
            assert_eq!(
                parse_iso8601_instant(&text),
                Some(secs),
                "round trip for {text}"
            );
        }
    }

    /// A timezone offset must be applied, not read lexically.
    ///
    /// This is the defect: `validUntil = "2026-11-15T20:00:00+14:00"` is
    /// `06:00Z`, already past when "now" is `07:31Z`, yet the string sorts
    /// *after* `2026-11-15T07:31:00Z` and the old `until < now` comparison
    /// called the credential valid.
    #[test]
    fn a_zone_offset_is_applied_to_the_instant() {
        let utc = parse_iso8601_instant("2026-11-15T06:00:00Z").unwrap();
        assert_eq!(
            parse_iso8601_instant("2026-11-15T20:00:00+14:00"),
            Some(utc)
        );
        assert_eq!(
            parse_iso8601_instant("2026-11-15T14:00:00+08:00"),
            Some(utc)
        );
        assert_eq!(
            parse_iso8601_instant("2026-11-14T22:00:00-08:00"),
            Some(utc)
        );
        // `+HHMM` without the colon is the same instant.
        assert_eq!(parse_iso8601_instant("2026-11-15T14:00:00+0800"), Some(utc));
        // Lower-case `z` and the RFC 3339 space separator.
        assert_eq!(parse_iso8601_instant("2026-11-15t06:00:00z"), Some(utc));
        assert_eq!(parse_iso8601_instant("2026-11-15 06:00:00Z"), Some(utc));
    }

    /// Fractional seconds must truncate, so `…:04.9Z` is not "after :04Z" in a
    /// way that flips a bound. The old comparison said it was.
    #[test]
    fn fractional_seconds_are_truncated_not_lexically_ordered() {
        let base = parse_iso8601_instant("2026-11-15T07:31:04Z").unwrap();
        assert_eq!(parse_iso8601_instant("2026-11-15T07:31:04.9Z"), Some(base));
        assert_eq!(
            parse_iso8601_instant("2026-11-15T07:31:04.000001Z"),
            Some(base)
        );
    }

    /// Malformed or impossible bounds must be `None`, so every caller fails
    /// closed rather than comparing a garbage string.
    #[test]
    fn impossible_instants_are_rejected() {
        for bad in [
            "",
            "2026-11-15",
            "2026-02-30T00:00:00Z",
            "2025-02-29T00:00:00Z", // 2025 is not a leap year
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-11-32T00:00:00Z",
            "2026-11-15T24:00:00Z",
            "2026-11-15T00:60:00Z",
            "2026-11-15T00:00:61Z",
            "2026-11-15T00:00:00+24:00",
            "2026-11-15T00:00:00+01:60",
            "2026-11-15T00:00:00.Z",
            "2026-11-15T00:00:00Zjunk",
            "2026-11-15T00:00:00+01:00x",
            "20261115T000000Z",
        ] {
            assert_eq!(parse_iso8601_instant(bad), None, "{bad:?} must not parse");
        }
    }

    /// `days_in_month` must know the Gregorian rules it is asked about.
    #[test]
    fn days_in_month_handles_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    /// An unparseable `validUntil` must count as expired, and an unparseable
    /// `validFrom` as not-yet-valid: neither can be relied on.
    #[test]
    fn unparseable_bounds_fail_closed() {
        let mut vc = VerifiableCredential::new(
            "urn:uuid:x",
            "did:key:z6Mk",
            vec!["VerifiableCredential".to_string()],
            super::CredentialSubject::new("did:key:z6Mk"),
        );
        vc.valid_until = Some("not-a-date".to_string());
        assert!(vc.is_expired());
        vc.valid_until = None;
        vc.valid_from = "2999-01-01T00:00:00Z".to_string();
        assert!(vc.is_not_yet_valid());
    }

    /// A far-future `validUntil` and a past `validFrom` are not flagged.
    #[test]
    fn normal_windows_are_not_flagged() {
        let mut vc = VerifiableCredential::new(
            "urn:uuid:x",
            "did:key:z6Mk",
            vec!["VerifiableCredential".to_string()],
            super::CredentialSubject::new("did:key:z6Mk"),
        );
        vc.valid_until = Some("2999-12-31T23:59:59Z".to_string());
        vc.valid_from = "2001-01-01T00:00:00Z".to_string();
        assert!(!vc.is_expired());
        assert!(!vc.is_not_yet_valid());
    }

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(format_unix_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn one_day_later() {
        assert_eq!(format_unix_to_iso8601(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn arbitrary_unix_matches_chrono_known_vector() {
        // 2024-01-15T13:05:30Z = 1705323930 — independent reference
        // from Python: `datetime.fromtimestamp(1705323930, tz=timezone.utc)`.
        assert_eq!(
            format_unix_to_iso8601(1_705_323_930),
            "2024-01-15T13:05:30Z"
        );
    }

    #[test]
    fn leap_year_handling() {
        // 2020-02-29T00:00:00Z = 1582934400.
        assert_eq!(
            format_unix_to_iso8601(1_582_934_400),
            "2020-02-29T00:00:00Z"
        );
    }

    #[test]
    fn century_handling() {
        // 2000-03-01T00:00:00Z = 951_868_800 (century leap year).
        assert_eq!(format_unix_to_iso8601(951_868_800), "2000-03-01T00:00:00Z");
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web3::identity::Identity;

    /// A small-order `did:key` plus an identity-point signature must not
    /// verify.
    ///
    /// `VerifyingKey::from_bytes` only checks that the point decompresses; it
    /// accepts the eight small-order points. With `VerifyingKey::verify` (the
    /// lenient verifier) a key of the identity point and a signature
    /// `R = identity, S = 0` verified for **any** credential — an attacker
    /// could mint a "valid" credential for an arbitrary issuer, subject and
    /// claims with no private key. `verify_strict` rejects small-order `A`
    /// and `R`; this pins the fix at the credential, presentation and
    /// `SignedMessage` layers.
    #[test]
    fn a_small_order_key_cannot_forge_a_credential() {
        // The identity point, as a `did:key` public key.
        let mut pk = [0u8; 32];
        pk[0] = 1;
        let did = crate::web3::DidKey::from_ed25519_public_key(&pk)
            .expect("the identity encoding is a valid curve point")
            .as_str()
            .to_string();

        // R = identity point, S = 0.
        let mut sig = [0u8; 64];
        sig[0] = 1;

        let mut vc = VerifiableCredential::new(
            "urn:uuid:forged",
            did.clone(),
            vec!["VerifiableCredential".to_string(), "Degree".to_string()],
            CredentialSubject::new("did:key:zVictim").with_claim("role", &"admin"),
        );
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{did}#key"),
            Signature::from_bytes(&sig).expect("64 bytes"),
        );
        proof.verification_method = format!("{did}#key");
        vc.sign(proof);

        assert!(
            vc.verify_proof().is_err(),
            "a small-order issuer key must not verify a credential"
        );
        assert!(vc.verify().is_err());
    }

    /// The same forgery must fail against a `SignedMessage` envelope.
    #[test]
    fn a_small_order_key_cannot_forge_a_signed_message() {
        use crate::web3::identity::{verify_signature_detailed, PublicKey};

        let mut pk = [0u8; 32];
        pk[0] = 1;
        // R = identity, S = 0.
        let mut sig = [0u8; 64];
        sig[0] = 1;
        let public = PublicKey::from_bytes(&pk).expect("32 bytes is a PublicKey");
        assert!(
            verify_signature_detailed(&public, &hex_of(&sig), b"attacker payload").is_err(),
            "a small-order signer key must not authenticate any payload"
        );
    }

    fn hex_of(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// A claim named `id` must never reach the wire form.
    ///
    /// `credentialSubject` is `#[serde(flatten)]`ed claims over a struct with
    /// an `id` field, and serde emits both without complaint:
    /// `{"id":"<subject>","id":"<claim>"}`. This crate's own `Deserialize`
    /// then rejects the document as `duplicate field 'id'` — a credential that
    /// was just signed cannot be read back — while last-wins parsers in other
    /// languages resolve the *claim* as the subject id. `with_claim` drops
    /// the key; `try_with_claim` (the input-boundary path used by `vc issue`)
    /// reports it.
    #[test]
    fn a_claim_named_id_cannot_shadow_the_subject_id() {
        let subject = CredentialSubject::new("did:key:z6MkReal")
            .with_claim("id", &"did:key:z6MkAttacker")
            .with_claim("role", &"admin");

        // Check the *streamed* JSON, not just `to_value`: a `serde_json::Map`
        // would collapse a duplicate key, which is exactly the behaviour this
        // test must not rely on. Serde streams a flattened map's keys
        // verbatim, so `to_string` is where the duplicate shows up.
        let wire = serde_json::to_string(&subject).expect("serialise");
        assert_eq!(
            wire.matches("\"id\"").count(),
            1,
            "credentialSubject must carry exactly one `id`, got {wire}"
        );
        assert!(wire.contains("did:key:z6MkReal"), "{wire}");
        assert!(!wire.contains("z6MkAttacker"), "{wire}");
        assert!(wire.contains("\"role\":\"admin\""), "{wire}");

        // And the whole credential round-trips, which it did not before.
        let vc = VerifiableCredential::new(
            "urn:uuid:claims",
            "did:key:z6MkIssuer",
            vec!["VerifiableCredential".to_string()],
            subject,
        );
        let text = String::from_utf8(vc.encode_canonical()).expect("utf8");
        let parsed: VerifiableCredential =
            serde_json::from_str(&text).expect("a credential we produced must parse back");
        assert_eq!(parsed.credential_subject.id, "did:key:z6MkReal");
    }

    /// The input-boundary constructor refuses the collision instead of
    /// dropping it, so an operator is told the claim was not issued.
    #[test]
    fn try_with_claim_reports_a_reserved_key() {
        let subject = CredentialSubject::new("did:key:z6MkReal");
        let err = subject
            .try_with_claim("id", &"did:key:z6MkAttacker")
            .expect_err("a reserved key must be an error");
        match err {
            Web3ErrorKind::BlockchainError(msg) => assert!(msg.contains("collides")),
            other => panic!("expected BlockchainError, got {other:?}"),
        }

        // A non-reserved key still works.
        let ok = CredentialSubject::new("did:key:z6MkReal")
            .try_with_claim("role", &"admin")
            .expect("ordinary claim");
        assert_eq!(ok.get("role").and_then(|v| v.as_str()), Some("admin"));
    }

    #[test]
    fn test_credential_creation() {
        let subject =
            CredentialSubject::new("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK")
                .with_claim("name", &"Alice")
                .with_claim("age", &30);

        let vc = VerifiableCredential::new(
            "urn:uuid:12345678-1234-1234-1234-123456789012",
            "did:key:z6MkissuerKey123456789012345678901234567890",
            vec![
                "VerifiableCredential".to_string(),
                "PersonCredential".to_string(),
            ],
            subject,
        )
        // Use a far-future date so the test isn't tied to "now". Previously
        // this was `2025-01-01`, which became stale in 2025.
        .with_expiry("2999-12-31T23:59:59Z");

        assert!(vc.validate().is_ok());
        assert!(!vc.is_expired());
    }

    #[test]
    fn test_credential_validation() {
        let subject = CredentialSubject::new("did:key:z6Mktest123");
        let vc = VerifiableCredential::new(
            "urn:uuid:test",
            "did:key:z6Mkissuer",
            vec!["VerifiableCredential".to_string()],
            subject,
        );

        // Should fail without VerifiableCredential type
        let mut invalid = vc.clone();
        invalid.credential_type = vec!["InvalidType".to_string()];
        assert!(invalid.validate().is_err());

        // Should fail with non-DID issuer
        let mut invalid2 = vc.clone();
        invalid2.issuer = "not-a-did".to_string();
        assert!(invalid2.validate().is_err());
    }

    #[test]
    fn test_presentation_creation() {
        let subject = CredentialSubject::new("did:key:z6Mkholder123");
        let vc = VerifiableCredential::new(
            "urn:uuid:123",
            "did:key:z6Mkissuer",
            vec!["VerifiableCredential".to_string()],
            subject,
        );

        let vp = VerifiablePresentation::new("did:key:z6Mkholder", vec![vc]);

        assert!(vp.validate().is_ok());
    }

    #[test]
    fn test_credential_subject_claims() {
        let subject = CredentialSubject::new("did:key:z6Mktest")
            .with_claim("email", &"alice@example.com")
            .with_claim("verified", &true);

        assert_eq!(
            subject.get("email"),
            Some(&serde_json::Value::String("alice@example.com".to_string()))
        );
        assert_eq!(
            subject.get("verified"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    /// JSON Schema defines `required` as an *array* of property names. The check
    /// originally matched only `Value::Object`, so a spec-shaped schema took the
    /// "not required" branch and `validate` returned `Ok(())` for every input —
    /// a schema check that could never reject anything.
    #[test]
    fn schema_validate_enforces_required_fields() {
        let schema = CredentialSchema::new(
            "urn:schema:person",
            "Person",
            "1.0",
            serde_json::json!({ "required": ["name", "degree"] }),
        );

        let vc = |subject: CredentialSubject| {
            VerifiableCredential::new(
                "urn:uuid:schema-test",
                "did:key:z6Mkissuer",
                vec!["VerifiableCredential".to_string()],
                subject,
            )
        };

        // Missing `degree`: must be rejected.
        let missing =
            vc(CredentialSubject::new("did:key:z6Mksubject").with_claim("name", &"Alice"));
        let err = schema
            .validate(&missing)
            .expect_err("a missing required claim must fail validation");
        assert!(
            format!("{err:?}").contains("degree"),
            "error should name the missing field: {err:?}"
        );

        // All required claims present: accepted.
        let complete = vc(CredentialSubject::new("did:key:z6Mksubject")
            .with_claim("name", &"Alice")
            .with_claim("degree", &"MSc"));
        assert!(schema.validate(&complete).is_ok());

        // `id` resolves to the subject DID, not to a claim.
        let id_schema = CredentialSchema::new(
            "urn:schema:with-id",
            "WithId",
            "1.0",
            serde_json::json!({ "required": ["id"] }),
        );
        assert!(id_schema
            .validate(&vc(CredentialSubject::new("did:key:z6Mksubject")))
            .is_ok());
        assert!(id_schema.validate(&vc(CredentialSubject::new(""))).is_err());

        // A `required` this function cannot interpret is an error, not a pass.
        let malformed = CredentialSchema::new(
            "urn:schema:bad",
            "Bad",
            "1.0",
            serde_json::json!({ "required": "name" }),
        );
        assert!(malformed.validate(&complete).is_err());

        // No `required` keyword at all: nothing to check.
        let empty = CredentialSchema::new(
            "urn:schema:empty",
            "Empty",
            "1.0",
            serde_json::json!({ "type": "object" }),
        );
        assert!(empty.validate(&missing).is_ok());
    }

    #[test]
    fn test_credential_status() {
        let status = CredentialStatus::new("https://example.com/status/123", "RevocationList2023");

        assert_eq!(status.purpose, "revocation");

        let status_list = CredentialStatus::status_list_2021(
            "https://example.com/status/456",
            "did:key:z6Mkissuer",
            42,
        );

        assert_eq!(status_list.status_type, "StatusList2021Entry");
        assert!(status_list.purpose.contains("index=42"));
    }

    #[test]
    fn test_uuid_generation() {
        // uuid_v4() returns the bare UUID; VerifiablePresentation::new
        // adds the `urn:uuid:` prefix when constructing an ID.
        let bare = uuid_v4();
        assert!(!bare.starts_with("urn:uuid:"));
        assert_eq!(bare.len(), "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".len());

        let with_prefix = format!("urn:uuid:{}", bare);
        assert!(with_prefix.starts_with("urn:uuid:"));
    }

    /// Audit-2026-08 C2: the previous `uuid_v4` used a fixed-seed LCG,
    /// making every issued credential ID predictable to anyone who
    /// knew the seed. After the fix we source bytes from the OS RNG,
    /// so two consecutive UUIDs must differ. We also assert the
    /// RFC-4122 version-4 and variant-1 bits are set so the IDs are
    /// still recognised as v4 by standards-compliant parsers.
    #[test]
    fn uuid_v4_is_random_and_rfc4122_compliant() {
        let a = uuid_v4();
        let b = uuid_v4();
        let c = uuid_v4();

        // Uniqueness: 3 distinct v4 IDs are vanishingly unlikely to
        // collide (p < 2^-120). If this assertion fires we either
        // regressed to the deterministic LCG or the RNG backend is
        // returning a constant.
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);

        // RFC 4122 §4.4 — version field is the high nibble of byte 6.
        // "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
        //   idx  0  2  4  6  8 10 12 14 16 18 20 22 24 26 28 30 32 34
        let v4_byte_idx = 14; // position of the version nibble
        let variant_byte_idx = 19; // position of the variant nibble
        for id in [&a, &b, &c] {
            let v4_char = id.as_bytes()[v4_byte_idx];
            let variant_char = id.as_bytes()[variant_byte_idx];
            assert_eq!(v4_char, b'4', "expected version-4 in {id}");
            let variant_top_two_bits = matches!(variant_char, b'8'..=b'b');
            assert!(
                variant_top_two_bits,
                "expected RFC 4122 variant-1 (8/9/a/b) in {id}, got '{variant_char}'"
            );
        }
    }

    /// Sign a presentation with a real holder key and verify it.
    fn signed_presentation() -> (Identity, VerifiablePresentation) {
        let identity = Identity::from_secret_bytes(&[7u8; 32]).expect("identity");
        let holder_did = identity.did_key().as_str().to_string();
        let mut presentation =
            VerifiablePresentation::new(holder_did.clone(), vec![signed_credential(&identity)]);
        sign_presentation(&identity, &mut presentation);
        (identity, presentation)
    }

    /// A credential issued and signed by `identity`.
    fn signed_credential(identity: &Identity) -> VerifiableCredential {
        let issuer_did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:presented",
            issuer_did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(issuer_did.clone()),
        );
        // Build the proof first so its options are part of the signed bytes.
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{issuer_did}#{}", issuer_did.trim_start_matches("did:key:")),
            Signature::from_bytes(&[0u8; 64]).expect("placeholder signature"),
        );
        let signed = identity
            .sign(&vc.signing_input(&proof).expect("signing input"))
            .expect("sign credential");
        proof.proof_value = Signature::from_hex(&signed.signature_hex)
            .expect("signature")
            .to_hex();
        vc.sign(proof);
        vc
    }

    /// Attach a fresh holder proof over the presentation's current body.
    fn sign_presentation(identity: &Identity, presentation: &mut VerifiablePresentation) {
        let holder_did = identity.did_key().as_str().to_string();
        // Build the proof first so its options (challenge, domain, …) are
        // part of the signed bytes, then attach the signature.
        let mut proof = PresentationProof::new(
            ProofType::Ed25519Signature2020,
            format!("{holder_did}#{}", holder_did.trim_start_matches("did:key:")),
            "challenge-1",
            Signature::from_bytes(&[0u8; 64]).expect("placeholder signature"),
        );
        let payload = presentation.signing_input(&proof).expect("signing input");
        let signed = identity.sign(&payload).expect("sign presentation");
        proof.proof_value = Signature::from_hex(&signed.signature_hex)
            .expect("signature")
            .to_hex();
        presentation.sign(proof);
    }

    /// A credential proof's challenge is covered by the signature too.
    ///
    /// `CredentialProof` carries `challenge` and `domain`; before they were
    /// appended to the signing input, a credential could be re-challenged
    /// after issuance and still verify.
    #[test]
    fn a_credential_challenge_is_covered_by_the_signature() {
        let identity = Identity::from_secret_bytes(&[11u8; 32]).expect("identity");
        let issuer_did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:challenged",
            issuer_did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(issuer_did.clone()),
        );
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{issuer_did}#{}", issuer_did.trim_start_matches("did:key:")),
            Signature::from_bytes(&[0u8; 64]).expect("placeholder"),
        )
        .with_challenge("credential-challenge-1");
        let signed = identity
            .sign(&vc.signing_input(&proof).expect("signing input"))
            .expect("sign credential");
        proof.proof_value = Signature::from_hex(&signed.signature_hex)
            .expect("signature")
            .to_hex();
        vc.sign(proof);
        vc.verify().expect("a freshly signed credential verifies");

        // Rewriting the challenge must invalidate the proof.
        if let Some(p) = vc.proof.as_mut() {
            p.challenge = Some("different-challenge".to_string());
        }
        assert!(
            vc.verify_proof().is_err(),
            "a credential challenge rewritten after signing must not verify"
        );
    }

    /// An absent credential challenge and an empty one must differ.
    #[test]
    fn an_absent_credential_challenge_is_not_an_empty_one() {
        let identity = Identity::from_secret_bytes(&[12u8; 32]).expect("identity");
        let issuer_did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:nochallenge",
            issuer_did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(issuer_did.clone()),
        );
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{issuer_did}#{}", issuer_did.trim_start_matches("did:key:")),
            Signature::from_bytes(&[0u8; 64]).expect("placeholder"),
        );
        let signed = identity
            .sign(&vc.signing_input(&proof).expect("signing input"))
            .expect("sign credential");
        proof.proof_value = Signature::from_hex(&signed.signature_hex)
            .expect("signature")
            .to_hex();
        vc.sign(proof);
        assert!(vc.verify_proof().is_ok());

        if let Some(p) = vc.proof.as_mut() {
            p.challenge = Some(String::new());
        }
        assert!(
            vc.verify_proof().is_err(),
            "adding an empty challenge must change the signed bytes"
        );
    }

    /// A presentation must not be verifiable as body-only.
    ///
    /// Round 15 let a proof whose `signingScheme` was absent fall back to
    /// `body-only` for compatibility. On a *presentation* that reintroduced
    /// the replay hole round 13 closed: a body-only signature does not cover
    /// `challenge`, and `signingScheme` is attacker-controlled JSON, so a
    /// replayer could blank it, rewrite the challenge to the next verifier's
    /// nonce, and pass. Probe confirmed it before the fix.
    #[test]
    fn a_presentation_cannot_be_downgraded_to_body_only() {
        let (identity, mut presentation) = signed_presentation();
        // Strip the scheme and re-sign over the body alone, as a pre-scheme
        // build did.
        presentation.proof.as_mut().expect("proof").signing_scheme = None;
        let mut body = presentation.clone();
        body.proof = None;
        let bytes = serde_json::to_vec(&body).expect("serialise");
        let signed = identity.sign(&bytes).expect("sign");
        presentation.proof.as_mut().expect("proof").proof_value = signed.signature_hex.clone();

        // Rewriting the challenge must still fail: there is no body-only
        // escape hatch for presentations.
        presentation.proof.as_mut().expect("proof").challenge = "verifier-nonce-xyz".to_string();
        assert!(
            presentation
                .verify_with_challenge("verifier-nonce-xyz")
                .is_err(),
            "a body-only presentation must not be accepted"
        );
    }

    /// `signing_input` and verification must agree for every scheme value.
    ///
    /// They resolved the scheme with different defaults (signing used
    /// `unwrap_or_default()`, which is `ProofOptions`; verification treated
    /// absent as `BodyOnly`), so a credential signed through the public
    /// helper with the field cleared could not be verified. `signing_input`
    /// now shares the verification path, so this pins the invariant for both
    /// explicit values and the absent one.
    #[test]
    fn signing_input_matches_verification_for_every_scheme() {
        for scheme in [
            None,
            Some(SigningScheme::BodyOnly),
            Some(SigningScheme::ProofOptions),
        ] {
            let identity = Identity::from_secret_bytes(&[32u8; 32]).expect("identity");
            let did = identity.did_key().as_str().to_string();
            let mut vc = VerifiableCredential::new(
                "urn:uuid:scheme",
                did.clone(),
                vec!["VerifiableCredential".to_string()],
                CredentialSubject::new(did.clone()),
            );
            let mut proof = CredentialProof::new(
                ProofType::Ed25519Signature2020,
                format!("{did}#{}", did.trim_start_matches("did:key:")),
                Signature::from_bytes(&[0u8; 64]).expect("placeholder"),
            );
            proof.signing_scheme = scheme;
            let signed = identity
                .sign(&vc.signing_input(&proof).expect("signing input"))
                .expect("sign via the public helper");
            proof.proof_value = signed.signature_hex.clone();
            vc.sign(proof);
            assert!(
                vc.verify_proof().is_ok(),
                "a credential signed through signing_input must verify (scheme {scheme:?})"
            );
        }
    }

    /// A presentation verifies only with the `options-v1` scheme.
    ///
    /// Every *other* value — an explicit `body-only`, or an absent field from
    /// an older build — leaves `challenge` unsigned and is rejected. An
    /// explicit `body-only` slipped through when the check only looked at
    /// absent values, which let a replayer rewrite the challenge.
    #[test]
    fn a_presentation_verifies_only_with_the_options_scheme() {
        let identity = Identity::from_secret_bytes(&[33u8; 32]).expect("identity");
        let holder_did = identity.did_key().as_str().to_string();

        for scheme in [
            None,
            Some(SigningScheme::BodyOnly),
            Some(SigningScheme::ProofOptions),
        ] {
            let mut presentation =
                VerifiablePresentation::new(holder_did.clone(), vec![signed_credential(&identity)]);
            let mut proof = PresentationProof::new(
                ProofType::Ed25519Signature2020,
                format!("{holder_did}#{}", holder_did.trim_start_matches("did:key:")),
                "challenge-1",
                Signature::from_bytes(&[0u8; 64]).expect("placeholder"),
            );
            proof.signing_scheme = scheme;
            let signed = identity
                .sign(&presentation.signing_input(&proof).expect("signing input"))
                .expect("sign");
            proof.proof_value = signed.signature_hex.clone();
            presentation.sign(proof);

            let expected_ok = scheme == Some(SigningScheme::ProofOptions);
            assert_eq!(
                presentation.verify_proof().is_ok(),
                expected_ok,
                "scheme {scheme:?}: only options-v1 may verify"
            );
        }
    }

    /// A body-only credential that carries a challenge is rejected.
    ///
    /// Its signature does not cover the challenge, so the field could have
    /// been added after issuance. Reporting it as bound would be a lie.
    #[test]
    fn a_body_only_credential_with_a_challenge_is_rejected() {
        let identity = Identity::from_secret_bytes(&[23u8; 32]).expect("identity");
        let did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:smuggled",
            did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(did.clone()),
        );
        let mut body = vc.clone();
        body.proof = None;
        let bytes = serde_json::to_vec(&body).expect("serialise");
        let signed = identity.sign(&bytes).expect("sign");
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{did}#{}", did.trim_start_matches("did:key:")),
            Signature::from_hex(&signed.signature_hex).expect("signature"),
        );
        proof.signing_scheme = None;
        proof.challenge = Some("added-after-signing".to_string());
        vc.sign(proof);

        let err = vc
            .verify_proof()
            .expect_err("an unsigned challenge must not be reported as bound");
        assert!(err.to_string().contains("does not cover"), "got {err}");
    }

    /// A credential signed by an earlier build — body only, no
    /// `signingScheme` — must still verify.
    ///
    /// When the proof options became part of the signing input, a credential
    /// issued before that change began failing with the *same* error a
    /// tampered one produces. Both are "signature does not verify", so the
    /// verifier could not tell a legitimate old credential from a forged one
    /// and would reject valid, signed evidence. The stored proof now records
    /// which rule it was signed under.
    ///
    /// A body-only credential with no challenge still verifies — the
    /// compatibility path round 15 added must keep working.
    #[test]
    fn a_legacy_body_only_credential_still_verifies() {
        let identity = Identity::from_secret_bytes(&[21u8; 32]).expect("identity");
        let did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:legacy",
            did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(did.clone()),
        );
        // The exact bytes the old implementation hashed: the body alone.
        let mut body = vc.clone();
        body.proof = None;
        let legacy_bytes = serde_json::to_vec(&body).expect("serialise");
        let signed = identity.sign(&legacy_bytes).expect("sign");
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{did}#{}", did.trim_start_matches("did:key:")),
            Signature::from_hex(&signed.signature_hex).expect("signature"),
        );
        // Simulate a credential written before the field existed: absent.
        proof.signing_scheme = None;
        vc.sign(proof);

        // Round-trip through JSON as a stored credential would be.
        let stored = serde_json::to_string(&vc).expect("serialise credential");
        let reloaded: VerifiableCredential = serde_json::from_str(&stored).expect("parse");
        reloaded
            .verify()
            .expect("a body-only credential must still verify");

        // Tampering with the body still fails.
        let mut tampered = reloaded.clone();
        tampered.issuer = "did:key:z6MkOther".to_string();
        assert!(tampered.verify_proof().is_err());
    }

    /// A body-only proof's `purpose` is outside the signature, so an
    /// arbitrary value there must not be accepted.
    ///
    /// The compatibility path signs the body alone, which leaves `created`,
    /// `purpose` and the `#`fragment of `verificationMethod` unauthenticated.
    /// Nothing consults them today — and that is precisely why accepting an
    /// arbitrary `purpose` is a trap: the first future check that reads it
    /// would read attacker-controlled data on an otherwise valid credential.
    /// Only the canonical issuer purpose is accepted.
    #[test]
    fn a_body_only_proof_cannot_smuggle_a_purpose() {
        let identity = Identity::from_secret_bytes(&[24u8; 32]).expect("identity");
        let did = identity.did_key().as_str().to_string();
        let mut vc = VerifiableCredential::new(
            "urn:uuid:purpose",
            did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(did.clone()),
        );
        let mut body = vc.clone();
        body.proof = None;
        let signed = identity
            .sign(&serde_json::to_vec(&body).expect("serialise"))
            .expect("sign");
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{did}#{}", did.trim_start_matches("did:key:")),
            Signature::from_hex(&signed.signature_hex).expect("signature"),
        );
        proof.signing_scheme = None; // legacy: body-only

        // The canonical purpose is accepted on the compatibility path.
        vc.sign(proof.clone());
        vc.verify_proof()
            .expect("body-only credential with AssertionMethod");

        // Any other purpose is refused, even though the signature itself
        // still checks out — that is the point.
        proof.purpose = "Authentication".to_string();
        vc.sign(proof);
        let err = vc
            .verify_proof()
            .expect_err("an unsigned purpose must not be accepted");
        assert!(
            err.to_string().contains("unauthenticated purpose"),
            "got {err}"
        );
    }

    /// A new credential records its scheme and is verified under it.
    #[test]
    fn a_new_credential_records_the_proof_options_scheme() {
        let identity = Identity::from_secret_bytes(&[22u8; 32]).expect("identity");
        let vc = signed_credential(&identity);
        assert_eq!(
            vc.proof.as_ref().and_then(|p| p.signing_scheme),
            Some(SigningScheme::ProofOptions)
        );
        let stored = serde_json::to_string(&vc).expect("serialise");
        assert!(
            stored.contains("options-v1"),
            "the scheme must survive serialisation: {stored}"
        );
        vc.verify().expect("verifies");
    }

    #[test]
    fn presentation_proof_round_trips() {
        let (_id, presentation) = signed_presentation();
        presentation
            .verify()
            .expect("a freshly signed presentation must verify");
    }

    /// Tampering with the body after signing must be detected. Without a
    /// verification path at all, `vc present` produced presentations nothing
    /// could check.
    #[test]
    fn presentation_proof_detects_a_tampered_body() {
        let (_id, mut presentation) = signed_presentation();
        presentation.holder = "did:key:z6MkSomebodyElse".to_string();
        assert!(
            presentation.verify_proof().is_err(),
            "a changed holder must not verify"
        );

        let (_id2, mut other) = signed_presentation();
        other.created = "1999-01-01T00:00:00Z".to_string();
        assert!(
            other.verify_proof().is_err(),
            "a changed created timestamp must not verify"
        );
    }

    /// The challenge must be covered by the signature.
    ///
    /// A challenge exists to bind the proof to one verifier's fresh nonce.
    /// When `encode_for_signing` dropped the whole proof, the challenge was
    /// outside the signed bytes: an attacker could rewrite it to any value
    /// and the proof still verified — defeating replay protection entirely.
    #[test]
    fn the_challenge_is_covered_by_the_signature() {
        let (_id, mut presentation) = signed_presentation();
        assert!(presentation.verify_proof().is_ok());

        presentation.proof.as_mut().expect("proof").challenge = "attacker-chosen".to_string();
        assert!(
            presentation.verify_proof().is_err(),
            "a challenge changed after signing must invalidate the proof"
        );
    }

    /// The domain is bound the same way.
    #[test]
    fn the_domain_is_covered_by_the_signature() {
        let (_id, mut presentation) = signed_presentation();
        presentation.proof.as_mut().expect("proof").domain = Some("attacker.example".to_string());
        assert!(
            presentation.verify_proof().is_err(),
            "a domain added after signing must invalidate the proof"
        );
    }

    /// An absent domain and an empty one must not hash identically.
    ///
    /// Encoding `None` as `""` allowed an attacker to *add* `domain: ""` to a
    /// signed presentation without invalidating it — signature malleability:
    /// two different documents sharing one signature.
    #[test]
    fn an_absent_domain_is_not_the_same_as_an_empty_one() {
        let (_id, mut presentation) = signed_presentation();
        assert!(
            presentation.proof.as_ref().expect("proof").domain.is_none(),
            "the helper signs without a domain"
        );
        presentation.proof.as_mut().expect("proof").domain = Some(String::new());
        assert!(
            presentation.verify_proof().is_err(),
            "adding an empty domain must change the signed bytes"
        );
    }

    /// A verifier checks its *own* challenge, not merely that the proof is
    /// internally consistent: replaying a presentation signed for a different
    /// verifier must fail.
    #[test]
    fn verify_with_challenge_rejects_a_foreign_challenge() {
        let (_id, presentation) = signed_presentation();
        presentation
            .verify_with_challenge("challenge-1")
            .expect("the presentation carries this challenge");
        let err = presentation
            .verify_with_challenge("someone-elses-nonce")
            .expect_err("a different verifier's challenge must be rejected");
        assert!(
            err.to_string()
                .contains("does not match the expected challenge"),
            "got {err}"
        );
    }

    /// A proof whose verification method names a different DID than the
    /// holder must be rejected — otherwise a presentation could carry
    /// somebody else's key.
    #[test]
    fn a_presentation_rejects_a_mismatched_verification_method() {
        let (_id, mut presentation) = signed_presentation();
        if let Some(proof) = presentation.proof.as_mut() {
            proof.verification_method = "did:key:z6MkAttacker#key".to_string();
        }
        let err = presentation
            .verify_proof()
            .expect_err("a foreign verification method must be rejected");
        assert!(
            err.to_string().contains("does not match holder"),
            "got {err}"
        );
    }

    /// Every field of a signed presentation is covered by the signature.
    ///
    /// Rounds 11–18 each fixed one *particular* field that turned out to be
    /// unprotected (the challenge, then the domain, then the scheme's absent
    /// value, then its explicit value). Each fix was found by thinking of the
    /// next attack. This test enumerates the fields exhaustively instead: for
    /// every mutable field, mutate it and require verification to fail. A new
    /// field added to `PresentationProof` must be added to this table, and the
    /// table is the place to check whether it is covered.
    #[test]
    fn every_presentation_field_is_covered_by_the_signature() {
        type Mutation = (&'static str, fn(&mut PresentationProof));

        let mutations: &[Mutation] = &[
            ("created", |p| p.created = "1999-01-01T00:00:00Z".into()),
            ("verificationMethod", |p| {
                p.verification_method = "did:key:z6MkAttacker#key".into()
            }),
            ("purpose", |p| p.purpose = "assertionMethod".into()),
            ("challenge", |p| p.challenge = "attacker-chosen".into()),
            ("domain", |p| p.domain = Some("attacker.example".into())),
            ("signingScheme", |p| p.signing_scheme = None),
            ("signingScheme (body-only)", |p| {
                p.signing_scheme = Some(SigningScheme::BodyOnly)
            }),
            ("proofType", |p| {
                p.proof_type = ProofType::DataIntegrityProof
            }),
        ];

        for (name, mutate) in mutations {
            let (_id, mut presentation) = signed_presentation();
            let before = presentation.verify_proof().is_ok();
            assert!(before, "the control must verify before mutating {name}");
            mutate(presentation.proof.as_mut().expect("proof"));
            assert!(
                presentation.verify_proof().is_err(),
                "mutating `{name}` must invalidate the presentation proof"
            );
        }
    }

    /// Every field of a signed presentation *body* is covered too.
    #[test]
    fn every_presentation_body_field_is_covered_by_the_signature() {
        type Mutation = (&'static str, fn(&mut VerifiablePresentation));

        let mutations: &[Mutation] = &[
            ("id", |p| p.id = "urn:uuid:tampered".into()),
            ("holder", |p| p.holder = "did:key:z6MkOther".into()),
            ("created", |p| p.created = "1999-01-01T00:00:00Z".into()),
            ("type", |p| p.presentation_type = vec!["Other".into()]),
            ("@context", |p| p.context = vec!["urn:evil".into()]),
        ];

        for (name, mutate) in mutations {
            let (_id, mut presentation) = signed_presentation();
            assert!(presentation.verify_proof().is_ok());
            mutate(&mut presentation);
            assert!(
                presentation.verify_proof().is_err(),
                "mutating body field `{name}` must invalidate the signature"
            );
        }
    }

    /// Removing a value the signer *did* cover must also break the signature.
    ///
    /// The field table above mutates a control that has no domain, so it can
    /// only test the "add" direction. This signs with a domain present and
    /// then drops it — the direction an attacker uses to strip a binding.
    #[test]
    fn removing_a_covered_domain_breaks_the_presentation_signature() {
        let identity = Identity::from_secret_bytes(&[43u8; 32]).expect("identity");
        let holder_did = identity.did_key().as_str().to_string();
        let mut presentation =
            VerifiablePresentation::new(holder_did.clone(), vec![signed_credential(&identity)]);
        let mut proof = PresentationProof::new(
            ProofType::Ed25519Signature2020,
            format!("{holder_did}#{}", holder_did.trim_start_matches("did:key:")),
            "challenge-1",
            Signature::from_bytes(&[0u8; 64]).expect("placeholder"),
        )
        .with_domain("my-verifier.example");
        let signed = identity
            .sign(&presentation.signing_input(&proof).expect("signing input"))
            .expect("sign");
        proof.proof_value = signed.signature_hex.clone();
        presentation.sign(proof);
        assert!(presentation.verify_proof().is_ok(), "control must verify");

        presentation.proof.as_mut().expect("proof").domain = None;
        assert!(
            presentation.verify_proof().is_err(),
            "dropping a signed domain must invalidate the proof"
        );
    }

    /// Every field of a signed credential proof is covered.
    #[test]
    fn every_credential_proof_field_is_covered_by_the_signature() {
        type Mutation = (&'static str, fn(&mut CredentialProof));

        let mutations: &[Mutation] = &[
            ("created", |p| p.created = "1999-01-01T00:00:00Z".into()),
            ("verificationMethod", |p| {
                p.verification_method = "did:key:z6MkAttacker#key".into()
            }),
            ("purpose", |p| p.purpose = "authentication".into()),
            ("challenge", |p| p.challenge = Some("added".into())),
            ("domain", |p| p.domain = Some("attacker.example".into())),
            ("proofType", |p| {
                p.proof_type = ProofType::DataIntegrityProof
            }),
        ];

        for (name, mutate) in mutations {
            let identity = Identity::from_secret_bytes(&[41u8; 32]).expect("identity");
            let mut vc = signed_credential(&identity);
            assert!(vc.verify_proof().is_ok(), "control must verify");
            mutate(vc.proof.as_mut().expect("proof"));
            assert!(
                vc.verify_proof().is_err(),
                "mutating credential proof field `{name}` must invalidate the signature"
            );
        }
    }

    /// Every field of a signed credential body is covered.
    #[test]
    fn every_credential_body_field_is_covered_by_the_signature() {
        type Mutation = (&'static str, fn(&mut VerifiableCredential));

        let mutations: &[Mutation] = &[
            ("id", |c| c.id = "urn:uuid:tampered".into()),
            ("issuer", |c| c.issuer = "did:key:z6MkOther".into()),
            ("validFrom", |c| {
                c.valid_from = "1999-01-01T00:00:00Z".into()
            }),
            ("type", |c| {
                // Must differ from the control's `["VerifiableCredential"]` —
                // assigning the same value is not a mutation.
                c.credential_type = vec!["VerifiableCredential".into(), "ExtraType".into()]
            }),
            ("credentialSubject", |c| {
                c.credential_subject = CredentialSubject::new("did:key:z6MkOther")
            }),
        ];

        for (name, mutate) in mutations {
            let identity = Identity::from_secret_bytes(&[42u8; 32]).expect("identity");
            let mut vc = signed_credential(&identity);
            assert!(vc.verify_proof().is_ok(), "control must verify");
            mutate(&mut vc);
            assert!(
                vc.verify_proof().is_err(),
                "mutating credential body field `{name}` must invalidate the signature"
            );
        }
    }

    #[test]
    fn an_unsigned_presentation_does_not_verify() {
        let identity = Identity::from_secret_bytes(&[9u8; 32]).expect("identity");
        let did = identity.did_key().as_str().to_string();
        let vc = VerifiableCredential::new(
            "urn:uuid:unsigned",
            did.clone(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(did.clone()),
        );
        let presentation = VerifiablePresentation::new(did, vec![vc]);
        assert!(presentation.verify_proof().is_err());
    }

    /// A valid holder proof over a *forged* credential must be rejected.
    ///
    /// The outer proof only says the holder assembled the document; it says
    /// nothing about the credentials inside, which the holder chooses freely.
    /// Verifying only the outer proof accepted a presentation whose single
    /// credential named a fake issuer and carried no signature — a valid
    /// presentation of a forged claim.
    #[test]
    fn a_presentation_rejects_a_forged_inner_credential() {
        let (identity, mut presentation) = signed_presentation();
        let holder_did = identity.did_key().as_str().to_string();
        // A credential that nobody signed, from an issuer that does not exist.
        presentation.verifiable_credential = vec![VerifiableCredential::new(
            "urn:uuid:forged",
            "did:key:z6MkFakeIssuer".to_string(),
            vec!["VerifiableCredential".to_string()],
            CredentialSubject::new(holder_did).with_claim("role", &"admin"),
        )];
        // The holder signs the presentation around it, so the *outer* proof is
        // genuinely valid.
        sign_presentation(&identity, &mut presentation);
        presentation
            .verify_proof()
            .expect("the outer proof itself is valid");

        let err = presentation
            .verify()
            .expect_err("a forged inner credential must not verify");
        assert!(
            err.to_string().contains("presented credential #0"),
            "the error must name the offending credential: {err}"
        );
    }

    /// An expired credential inside a fresh presentation must be rejected.
    ///
    /// `VerifiableCredential::verify` checks expiry; `VerifiablePresentation
    /// ::verify` must route each credential through it rather than only
    /// checking structure.
    #[test]
    fn a_presentation_rejects_an_expired_inner_credential() {
        let (identity, mut presentation) = signed_presentation();
        let holder_did = identity.did_key().as_str().to_string();
        let mut expired = signed_credential(&identity).with_expiry("2001-01-01T00:00:00Z");
        // Re-sign over the expired body through the sign-then-attach helper.
        // Signing with `encode_for_signing` while a *stale* proof is attached
        // would hash the old proof's `created`, then attach a proof whose
        // `created` is a fresh timestamp — verification would drift with the
        // clock (and the test would pass or fail by luck of the second).
        let mut proof = CredentialProof::new(
            ProofType::Ed25519Signature2020,
            format!("{holder_did}#{}", holder_did.trim_start_matches("did:key:")),
            Signature::from_bytes(&[0u8; 64]).expect("placeholder signature"),
        );
        let signed = identity
            .sign(&expired.signing_input(&proof).expect("signing input"))
            .expect("re-sign expired credential");
        proof.proof_value = Signature::from_hex(&signed.signature_hex)
            .expect("signature")
            .to_hex();
        expired.sign(proof);
        presentation.verifiable_credential = vec![expired];
        sign_presentation(&identity, &mut presentation);

        let err = presentation
            .verify()
            .expect_err("an expired credential must not verify");
        assert!(
            err.to_string().contains("expired"),
            "the error must say the credential expired: {err}"
        );
    }
}
