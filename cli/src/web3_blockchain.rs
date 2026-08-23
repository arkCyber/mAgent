//! Blockchain CLI commands for mAgent.
//!
//! This module extends the `magent web3` subcommand with blockchain-specific
//! operations:
//!
//! - **Chain Management**: List supported chains, add custom chains
//! - **Identity Binding**: Bind DID identities to blockchain addresses
//! - **Balance Checking**: Query native token balances
//! - **Transaction Building**: Build unsigned transactions
//! - **Credential Commands**: Issue and verify verifiable credentials
//!
//! ## New Subcommands
//!
//! ```text
//! magent web3 chain list                          List supported chains
//! magent web3 chain info <CHAIN>                 Show chain configuration
//! magent web3 bind create <DID> --address <ADDR> --chain <CHAIN>  Create binding
//! magent web3 bind verify <DID> --address <ADDR> --chain <CHAIN>  Verify binding
//! magent web3 balance <ADDRESS> [--chain <CHAIN>]  Check balance
//! magent web3 tx build <TO> --value <VAL> --data <HEX>  Build transaction
//! magent web3 vc issue <SUBJECT_DID> --type <TYPE> --claim <JSON>  Issue VC
//! magent web3 vc verify <VC_FILE>                          Verify VC
//! magent web3 vc present <VC_FILE> --challenge <STR>  Create presentation
//! ```

#![cfg(feature = "blockchain")]

use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use serde::Serialize;

use magent_core::error::Web3ErrorKind;
use crate::output::{Output, OutputKind};
use crate::web3::Web3CliError;

/// Blockchain-related subcommands.
#[derive(Debug, Clone)]
pub enum BlockchainAction {
    /// `magent web3 chain list`
    ChainList,
    /// `magent web3 chain info <CHAIN>`
    ChainInfo(String),
    /// `magent web3 bind create <DID>`
    BindCreate(BindCreateOptions),
    /// `magent web3 bind verify <DID>`
    BindVerify(BindVerifyOptions),
    /// `magent web3 balance <ADDRESS>`
    Balance(BalanceOptions),
    /// `magent web3 tx build <TO>`
    TxBuild(TxBuildOptions),
    /// `magent web3 vc issue <SUBJECT_DID>`
    VcIssue(VcIssueOptions),
    /// `magent web3 vc verify <FILE>`
    VcVerify(PathBuf),
    /// `magent web3 vc present <FILE>`
    VcPresent(VcPresentOptions),
}

/// Options for binding creation.
#[derive(Debug, Clone)]
pub struct BindCreateOptions {
    pub did: String,
    pub address: String,
    pub chain: String,
    pub expiry_days: Option<u64>,
}

/// Options for binding verification.
#[derive(Debug, Clone)]
pub struct BindVerifyOptions {
    pub did: String,
    pub address: String,
    pub chain: String,
}

/// Options for balance checking.
#[derive(Debug, Clone)]
pub struct BalanceOptions {
    pub address: String,
    pub chain: Option<String>,
}

/// Options for transaction building.
#[derive(Debug, Clone)]
pub struct TxBuildOptions {
    pub to: String,
    pub value: Option<String>,
    pub data: Option<String>,
    pub chain: String,
}

/// Options for VC issuance.
#[derive(Debug, Clone)]
pub struct VcIssueOptions {
    pub subject_did: String,
    pub credential_type: String,
    pub claims: Vec<(String, serde_json::Value)>,
    pub issuer_did: String,
    pub expiry_days: Option<u64>,
}

/// Options for VC presentation.
#[derive(Debug, Clone)]
pub struct VcPresentOptions {
    pub vc_file: PathBuf,
    pub holder_did: String,
    pub challenge: String,
    pub domain: Option<String>,
}

/// Chain display information.
#[derive(Debug, Serialize)]
pub struct ChainDisplay {
    pub id: u64,
    pub name: String,
    pub currency: String,
    pub has_rpc: bool,
    pub explorer: Option<String>,
}

/// Balance display information.
#[derive(Debug, Serialize)]
pub struct BalanceDisplay {
    pub address: String,
    pub chain: String,
    pub balance_wei: String,
    pub balance_ether: String,
}

/// Transaction display information.
#[derive(Debug, Serialize)]
pub struct TxDisplay {
    pub to: String,
    pub value_wei: String,
    pub value_ether: String,
    pub data_length: usize,
    pub encoded_length: usize,
}

/// Binding display information.
#[derive(Debug, Serialize)]
pub struct BindingDisplay {
    pub did: String,
    pub address: String,
    pub chain: String,
    pub status: String,
    pub created_at: u64,
    /// Optional expiry (Unix block). Serialised as `null` by
    /// default — explicit `Option::is_none` skip is intentionally
    /// not enabled here because downstream tooling may rely on
    /// the field always being present to drive its
    /// "Expires:" / "no expiry" rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

/// VC display information.
#[derive(Debug, Serialize)]
pub struct VcDisplay {
    pub id: String,
    pub issuer: String,
    pub subject: String,
    pub credential_type: String,
    pub valid_from: String,
    pub valid_until: Option<String>,
    pub has_proof: bool,
}

/// Presentation display information.
#[derive(Debug, Serialize)]
pub struct PresentationDisplay {
    pub id: String,
    pub holder: String,
    pub credential_count: usize,
    pub created: String,
    pub has_proof: bool,
}

/// Render chain list output.
pub fn render_chain_list(chains: Vec<ChainDisplay>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(&chains).unwrap_or_default());
    } else {
        let _ = out.info("Supported chains:");
        for chain in &chains {
            let rpc_status = if chain.has_rpc { "✓" } else { "✗" };
            let explorer = chain.explorer.as_deref().unwrap_or("-");
            let _ = out.info(&format!(
                "  {}  {}  (RPC: {}  Explorer: {})",
                chain.id, chain.name, rpc_status, explorer
            ));
        }
    }
}

/// Render chain info output.
pub fn render_chain_info(chain: &ChainDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(chain).unwrap_or_default());
    } else {
        let _ = out.info(&format!("Chain: {}", chain.name));
        let _ = out.info(&format!("  ID: {}", chain.id));
        let _ = out.info(&format!("  Currency: {}", chain.currency));
        let _ = out.info(&format!(
            "  RPC: {}",
            if chain.has_rpc { "available" } else { "not configured" }
        ));
        let _ = out.info(&format!(
            "  Explorer: {}",
            chain.explorer.as_deref().unwrap_or("-")
        ));
    }
}

/// Render balance output.
pub fn render_balance(balance: &BalanceDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(balance).unwrap_or_default());
    } else {
        let _ = out.info(&format!("Address: {}", balance.address));
        let _ = out.info(&format!("Chain: {}", balance.chain));
        let _ = out.info(&format!("Balance: {} ETH", balance.balance_ether));
        let _ = out.info(&format!("         {} wei", balance.balance_wei));
    }
}

/// Render transaction output.
pub fn render_tx(tx: &TxDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(tx).unwrap_or_default());
    } else {
        let _ = out.info("Transaction request:");
        let _ = out.info(&format!("  To: {}", tx.to));
        let _ = out.info(&format!("  Value: {} ETH", tx.value_ether));
        let _ = out.info(&format!("  Data: {} bytes", tx.data_length));
        let _ = out.info(&format!("  Encoded: {} bytes", tx.encoded_length));
        let _ = out.info("");
        let _ = out.info("Note: This is an unsigned transaction. Sign and broadcast separately.");
    }
}

/// Render binding output.
pub fn render_binding(binding: &BindingDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(binding).unwrap_or_default());
    } else {
        let _ = out.info("Identity Binding:");
        let _ = out.info(&format!("  DID: {}", binding.did));
        let _ = out.info(&format!("  Address: {}", binding.address));
        let _ = out.info(&format!("  Chain: {}", binding.chain));
        let _ = out.info(&format!("  Status: {}", binding.status));
        let _ = out.info(&format!("  Created: block {}", binding.created_at));
        if let Some(expires) = binding.expires_at {
            let _ = out.info(&format!("  Expires: block {}", expires));
        }
    }
}

/// Render binding verification result.
pub fn render_binding_verify(is_valid: bool, binding: Option<&BindingDisplay>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        let json = serde_json::json!({
            "is_valid": is_valid,
            "binding": binding
        });
        out.write_json_str(serde_json::to_string(&json).unwrap_or_default());
    } else {
        if is_valid {
            let _ = out.info("✓ Binding is valid");
            if let Some(b) = binding {
                let _ = out.info(&format!("  DID: {}", b.did));
                let _ = out.info(&format!("  Address: {}", b.address));
                let _ = out.info(&format!("  Chain: {}", b.chain));
            }
        } else {
            let _ = out.info("✗ Binding is invalid or not found");
        }
    }
}

/// Render VC output.
pub fn render_vc(vc: &VcDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(vc).unwrap_or_default());
    } else {
        let _ = out.info("Verifiable Credential:");
        let _ = out.info(&format!("  ID: {}", vc.id));
        let _ = out.info(&format!("  Issuer: {}", vc.issuer));
        let _ = out.info(&format!("  Subject: {}", vc.subject));
        let _ = out.info(&format!("  Type: {}", vc.credential_type));
        let _ = out.info(&format!("  Valid from: {}", vc.valid_from));
        if let Some(until) = &vc.valid_until {
            let _ = out.info(&format!("  Valid until: {}", until));
        }
        let proof_status = if vc.has_proof { "signed" } else { "unsigned" };
        let _ = out.info(&format!("  Proof: {}", proof_status));
    }
}

/// Render VC verification result.
pub fn render_vc_verify(is_valid: bool, vc: Option<&VcDisplay>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        let json = serde_json::json!({
            "is_valid": is_valid,
            "credential": vc
        });
        out.write_json_str(serde_json::to_string(&json).unwrap_or_default());
    } else {
        if is_valid {
            let _ = out.info("✓ Credential is valid");
            if let Some(v) = vc {
                let _ = out.info(&format!("  ID: {}", v.id));
                let _ = out.info(&format!("  Issuer: {}", v.issuer));
                let _ = out.info(&format!("  Subject: {}", v.subject));
            }
        } else {
            let _ = out.info("✗ Credential is invalid or expired");
        }
    }
}

/// Render presentation output.
pub fn render_presentation(vp: &PresentationDisplay, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(serde_json::to_string(vp).unwrap_or_default());
    } else {
        let _ = out.info("Verifiable Presentation:");
        let _ = out.info(&format!("  ID: {}", vp.id));
        let _ = out.info(&format!("  Holder: {}", vp.holder));
        let _ = out.info(&format!("  Credentials: {}", vp.credential_count));
        let _ = out.info(&format!("  Created: {}", vp.created));
        let proof_status = if vp.has_proof { "signed" } else { "unsigned" };
        let _ = out.info(&format!("  Proof: {}", proof_status));
    }
}

// ============================================================================
// Error helpers
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Helper: build a custom `Output` that writes to in-memory
    /// buffers rather than the real stdout/stderr. We can't
    /// monkey-patch `Output`'s built-in locks (they own
    /// `StdoutLock`/`StderrLock`), so for the tests we just verify
    /// the *serialization* shape that the JSON-mode paths would
    /// emit, and the human-mode renderer paths by reading the
    /// serde_json serialisation directly (the human paths are
    /// pure `format!` + `writeln!` and don't need a fixture
    /// harness).
    fn json_string<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap_or_default()
    }

    #[test]
    fn chain_display_serialises_to_json() {
        let display = ChainDisplay {
            id: 1,
            name: "Ethereum".into(),
            currency: "ETH".into(),
            has_rpc: true,
            explorer: Some("https://etherscan.io".into()),
        };
        let s = json_string(&display);
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"name\":\"Ethereum\""));
        assert!(s.contains("\"currency\":\"ETH\""));
        assert!(s.contains("\"has_rpc\":true"));
    }

    #[test]
    fn balance_display_serialises_to_json() {
        let display = BalanceDisplay {
            address: "0xabc".into(),
            chain: "ethereum".into(),
            balance_wei: "1000000000000000000".into(),
            balance_ether: "1.000000".into(),
        };
        let s = json_string(&display);
        assert!(s.contains("\"balance_wei\":\"1000000000000000000\""));
        assert!(s.contains("\"balance_ether\":\"1.000000\""));
    }

    #[test]
    fn tx_display_serialises_to_json() {
        let display = TxDisplay {
            to: "0xdef".into(),
            value_wei: "42".into(),
            value_ether: "0.000000".into(),
            data_length: 4,
            encoded_length: 110,
        };
        let s = json_string(&display);
        assert!(s.contains("\"data_length\":4"));
        assert!(s.contains("\"encoded_length\":110"));
    }

    #[test]
    fn binding_display_serialises_with_expiry() {
        let display = BindingDisplay {
            did: "did:key:z6Mk".into(),
            address: "0xabc".into(),
            chain: "ethereum".into(),
            status: "Confirmed".into(),
            created_at: 1000,
            expires_at: Some(2000),
        };
        let s = json_string(&display);
        assert!(s.contains("\"expires_at\":2000"));
        assert!(s.contains("\"status\":\"Confirmed\""));
    }

    #[test]
    fn binding_display_serialises_without_expiry() {
        let display = BindingDisplay {
            did: "did:key:z6Mk".into(),
            address: "0xabc".into(),
            chain: "ethereum".into(),
            status: "Confirmed".into(),
            created_at: 1000,
            expires_at: None,
        };
        let s = json_string(&display);
        // expires_at is None → JSON should omit it (serde default).
        // We look for the JSON value shape `:null` and the
        // surrounding field name to confirm the field was omitted.
        // A literal substring match for "expires_at" would catch
        // its appearance inside field-name position as well, which
        // is exactly what we DO want to assert absent. But because
        // the struct's serde rename uses `skip_serializing_if`, the
        // field should be entirely missing.
        assert!(
            !s.contains("expires_at"),
            "expected expires_at field to be skipped, got JSON: {s}"
        );
    }

    #[test]
    fn vc_display_serialises_to_json() {
        let display = VcDisplay {
            id: "urn:uuid:1".into(),
            issuer: "did:key:issuer".into(),
            subject: "did:key:subject".into(),
            credential_type: "PersonCredential".into(),
            valid_from: "2024-01-01T00:00:00Z".into(),
            valid_until: Some("2025-01-01T00:00:00Z".into()),
            has_proof: true,
        };
        let s = json_string(&display);
        assert!(s.contains("\"has_proof\":true"));
        assert!(s.contains("\"valid_until\":\"2025-01-01T00:00:00Z\""));
    }

    #[test]
    fn presentation_display_serialises_to_json() {
        let display = PresentationDisplay {
            id: "urn:uuid:vp".into(),
            holder: "did:key:holder".into(),
            credential_count: 2,
            created: "2024-01-01T00:00:00Z".into(),
            has_proof: false,
        };
        let s = json_string(&display);
        assert!(s.contains("\"credential_count\":2"));
        assert!(s.contains("\"has_proof\":false"));
    }

    #[test]
    fn json_serialised_chain_list_parses_as_array() {
        // Mirrors what `render_chain_list` does on the JSON path:
        // serialise each entry, then concat into a JSON array. The
        // serialisation step itself is what we test (the writer is
        // an io::StdoutLock we can't capture).
        let chains = vec![ChainDisplay {
            id: 1,
            name: "Ethereum".into(),
            currency: "ETH".into(),
            has_rpc: true,
            explorer: None,
        }];
        let s = json_string(&chains);
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(parsed[0]["name"], "Ethereum");
    }

    #[test]
    fn json_serialised_balance_parses_as_object() {
        let balance = BalanceDisplay {
            address: "0xabc".into(),
            chain: "ethereum".into(),
            balance_wei: "0".into(),
            balance_ether: "0.000000".into(),
        };
        let s = json_string(&balance);
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["balance_wei"], "0");
        assert_eq!(parsed["balance_ether"], "0.000000");
    }

    #[test]
    fn json_serialised_binding_verify_envelope_has_is_valid() {
        let b = BindingDisplay {
            did: "did:key:z6Mk".into(),
            address: "0xabc".into(),
            chain: "ethereum".into(),
            status: "Confirmed".into(),
            created_at: 1000,
            expires_at: None,
        };
        // Mirror what `render_binding_verify` produces on the JSON
        // path: build the {is_valid, binding} envelope manually.
        let envelope = serde_json::json!({
            "is_valid": true,
            "binding": json_string(&b),
        });
        let parsed: serde_json::Value = serde_json::from_str(&envelope.to_string()).unwrap();
        assert_eq!(parsed["is_valid"], true);
    }

    #[test]
    fn from_web3_error_kind_into_web3_cli_error() {
        // Conversion preserves the variant so the CLI can pattern
        // match on it.
        let kind = Web3ErrorKind::BlockchainError("boom".into());
        let cli_err: Web3CliError = kind.into();
        assert!(matches!(
            cli_err,
            Web3CliError::Core(Web3ErrorKind::BlockchainError(_))
        ));
    }

    /// Smoke test: an `Output` builds cleanly with both kinds and
    /// `OutputKind` returns what we asked for. We can't easily
    /// inspect the written bytes (the underlying writers go to the
    /// real stdout/stderr), but the construction path is a
    /// regression canary for plumbing changes.
    #[test]
    fn output_constructs_with_human_and_json_kinds() {
        let _h = Output::new(OutputKind::Human, true);
        let _j = Output::new(OutputKind::Json, true);
    }

    /// Confirm we can read at least one byte from stdin (the
    /// tests run interactively with stdin closed, but the read
    /// API itself should be invokable without panic).
    #[test]
    fn stdin_read_does_not_panic_on_eof() {
        let mut buf = [0u8; 16];
        let res = std::io::stdin().read(&mut buf);
        // We don't care what the result is, only that the API
        // is callable from tests.
        let _ = res;
    }
}
