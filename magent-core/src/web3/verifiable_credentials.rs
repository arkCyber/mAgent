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
        if !self.credential_type.contains(&"VerifiableCredential".to_string()) {
            return Err(Web3ErrorKind::BlockchainError(
                "credential type must include VerifiableCredential".to_string(),
            ));
        }
        Ok(())
    }

    /// Check if the credential is expired.
    pub fn is_expired(&self) -> bool {
        if let Some(ref until) = self.valid_until {
            let now = iso8601_timestamp();
            return until < &now;
        }
        false
    }

    /// Encode for signing (canonical form).
    ///
    /// SECURITY: this must produce the exact same bytes regardless of
    /// whether a proof is currently attached. If it didn't, a malicious
    /// issuer could attach a valid proof, change the credential body,
    /// and the verifier would still call it valid because the proof
    /// matched the *old* canonical bytes.
    ///
    /// We therefore copy the struct, drop the `proof` field, and
    /// serialise the rest. This guarantees the sign-then-verify
    /// round-trip is safe.
    pub fn encode_for_signing(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.proof = None;
        serde_json::to_vec(&clone).unwrap_or_default()
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

    /// Add a claim.
    pub fn with_claim<T: Serialize>(mut self, key: impl Into<String>, value: &T) -> Self {
        if let Ok(val) = serde_json::to_value(value) {
            self.claims.insert(key.into(), val);
        }
        self
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofType {
    /// Ed25519 Signature 2020.
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

impl Default for ProofType {
    fn default() -> Self {
        ProofType::Ed25519Signature2020
    }
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
    pub fn new(
        holder: impl Into<String>,
        credentials: Vec<VerifiableCredential>,
    ) -> Self {
        Self {
            context: vec![
                CREDENTIALS_V1_CONTEXT.to_string(),
                CREDENTIALS_V2_CONTEXT.to_string(),
            ],
            id: format!("urn:uuid:{}", uuid_v4()),
            presentation_type: vec![
                "VerifiablePresentation".to_string(),
            ],
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
    pub fn validate(&self, credential: &VerifiableCredential) -> Result<(), Web3ErrorKind> {
        // Basic validation: check required fields exist
        let subject = &credential.credential_subject;

        if let Some(serde_json::Value::Object(map)) = self.schema.get("required") {
            for field in map.keys() {
                if field != "id" && !subject.claims.contains_key(field) {
                    return Err(Web3ErrorKind::BlockchainError(format!(
                        "missing required field: {}",
                        field
                    )));
                }
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
    use alloc::format;
    use alloc::string::ToString;

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

/// Format a Unix timestamp (seconds) as ISO-8601 UTC
/// (`YYYY-MM-DDTHH:MM:SSZ`) without pulling in the `chrono` crate.
///
/// Implementation notes:
/// - Uses Howard Hinnant's `days_from_civil` algorithm for civil -> days
///   conversion (no leap-second table needed for our 1970-2099 range).
/// - Year/month/day are derived from the day count; hour/minute/second
///   from the seconds-of-day remainder.
fn format_unix_to_iso8601(unix_secs: u64) -> String {
    use alloc::format;
    use alloc::string::String;

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
    let _ = write_iso8601(&mut out, year, month, day, hour, minute, second);
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
    let mut buf: [u8; 4] = [0; 4];
    let pad4 = |n: i64, buf: &mut [u8; 4]| -> alloc::string::String {
        let mut s = n.to_string();
        while s.len() < 4 {
            s = alloc::format!("0{}", s);
        }
        s
    };
    out.push_str(&pad4(year, &mut buf));
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
    const DAYS_PER_4Y: i64 = 1461;
    const DAYS_PER_1Y: i64 = 365;

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
    use super::format_unix_to_iso8601;

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
        assert_eq!(format_unix_to_iso8601(1_705_323_930), "2024-01-15T13:05:30Z");
    }

    #[test]
    fn leap_year_handling() {
        // 2020-02-29T00:00:00Z = 1582934400.
        assert_eq!(format_unix_to_iso8601(1_582_934_400), "2020-02-29T00:00:00Z");
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

    #[test]
    fn test_credential_creation() {
        let subject = CredentialSubject::new("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK")
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
        let mut vc = VerifiableCredential::new(
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

        let vp = VerifiablePresentation::new(
            "did:key:z6Mkholder",
            vec![vc],
        );

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

    #[test]
    fn test_credential_status() {
        let status = CredentialStatus::new(
            "https://example.com/status/123",
            "RevocationList2023",
        );

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
            let variant_top_two_bits = match variant_char {
                b'8'..=b'b' => true,
                _ => false,
            };
            assert!(
                variant_top_two_bits,
                "expected RFC 4122 variant-1 (8/9/a/b) in {id}, got '{variant_char}'"
            );
        }
    }
}
